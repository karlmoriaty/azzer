//! 路由表。
//!
//! **2026-11-09 审计修复**：M0 时期只有 `/health` 与 `/v1/system/info`，
//! 导致 N01–N16 的能力完全无法通过 HTTP 触达（能力孤岛）。
//! 现在补上三条业务端点，把 pipeline 真正接到用户手上：
//!
//! | 方法 | 路径 | 作用 |
//! |---|---|---|
//! | POST | `/v1/covers` | 上传 WAV 原始字节，异步跑 A 入口，立即返回 job_id |
//! | GET | `/v1/covers/{id}` | 查状态与完整报告（含 37 项评估） |
//! | GET | `/v1/covers/{id}/master.wav` | 取成品音频 |
//!
//! 为什么异步：4 分钟源端到端约 40 秒，同步 HTTP 会超时。上传后立刻返回
//! job_id，处理在 `spawn_blocking` 里跑（A 入口是 CPU 密集，不能占 tokio 工作线程）。
//!
//! 鉴权在 handler 内显式调用 `authorized()`，原因见 `auth.rs` 模块注释：
//! axum 中间件在 `nest` 场景下实测不生效。新增受保护端点时必须同时加 401 测试。

use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::{
    auth::{authorized, unauthorized},
    state::{AppState, Job},
};

/// 上传体积上限：200MB。10 分钟 48k/24bit 立体声约 173MB，留一点余量。
const MAX_UPLOAD_BYTES: usize = 200 * 1024 * 1024;

/// 组装路由。
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/system/info", get(system_info))
        .route("/v1/covers", post(create_cover))
        .route("/v1/covers/{id}", get(get_cover))
        .route("/v1/covers/{id}/master.wav", get(get_master))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "service": "azzer-api" }))
}

/// 系统信息：前端用它决定显示"GPU 加速"还是"仅 CPU"。
///
/// `probe_runtime()` 内部有 `OnceLock` 缓存——ONNX Runtime 的 Environment
/// 是进程单例，不能重复初始化。
async fn system_info(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, unauthorized()).into_response();
    }

    let probe = azzer_infer::probe_runtime();
    let uptime = state.started_at.elapsed().as_secs();
    let cuda = azzer_infer::cuda_available();
    Json(json!({
        "version": state.version,
        "uptime_s": uptime,
        "runtime": probe,
        "gpu_accelerated": cuda,
        "backend": probe.recommended,
        "ready": probe.init_error.is_none(),
        // 让前端能显示"当前走哪条入口"
        "capabilities": state.pipeline.capabilities(),
        "models_declared": state.pipeline.models.models().len(),
    }))
    .into_response()
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": { "code": "BAD_REQUEST", "message": message } })),
    )
        .into_response()
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": { "code": "NOT_FOUND", "message": message } })),
    )
        .into_response()
}

fn new_job_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 上传源 WAV 并启动处理。
///
/// 请求体是**原始 WAV 字节**，不是 multipart。理由：只传一个文件，
/// multipart 要额外依赖且解析出错时的错误信息更差。
async fn create_cover(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, unauthorized()).into_response();
    }
    if body.is_empty() {
        return bad_request("请求体为空。请把 WAV 文件字节作为请求体发送。");
    }
    if body.len() > MAX_UPLOAD_BYTES {
        return bad_request(&format!(
            "源文件 {:.1}MB 超过 200MB 上限。请先裁短或降采样后再上传。",
            body.len() as f64 / 1_048_576.0
        ));
    }

    let job_id = new_job_id();
    let upload_dir = state.pipeline.root.join("uploads");
    if let Err(e) = std::fs::create_dir_all(&upload_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": { "code": "IO", "message": format!("创建上传目录失败：{e}。检查磁盘权限与剩余空间。") } })),
        )
            .into_response();
    }
    let src_path: PathBuf = upload_dir.join(format!("{job_id}.wav"));
    if let Err(e) = std::fs::write(&src_path, &body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": { "code": "IO", "message": format!("写入上传文件失败：{e}。检查磁盘剩余空间。") } })),
        )
            .into_response();
    }

    if let Ok(mut jobs) = state.jobs.lock() {
        jobs.insert(job_id.clone(), Job::Running);
    }

    let ctx = state.pipeline.clone();
    let jobs = state.jobs.clone();
    let id_for_task = job_id.clone();
    tokio::task::spawn_blocking(move || {
        let result = azzer_pipeline::run_a_entry(&ctx, &src_path, &azzer_dsp::MasterConfig::default());
        let job = match result {
            Ok(report) => Job::Done {
                master_file: report.master_blob.clone(),
                report: Box::new(report),
            },
            Err(e) => Job::Failed {
                message: e.to_string(),
            },
        };
        if let Ok(mut map) = jobs.lock() {
            map.insert(id_for_task, job);
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "job_id": job_id,
            "status_url": format!("/v1/covers/{job_id}"),
        })),
    )
        .into_response()
}

/// 查任务状态与报告。
async fn get_cover(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, unauthorized()).into_response();
    }
    let job = state.jobs.lock().ok().and_then(|m| m.get(&id).cloned());
    match job {
        Some(j) => Json(j).into_response(),
        None => not_found("找不到这个任务。它可能已随进程重启清空，请重新上传。"),
    }
}

/// 取成品音频。
async fn get_master(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, unauthorized()).into_response();
    }
    let job = state.jobs.lock().ok().and_then(|m| m.get(&id).cloned());
    let digest = match job {
        Some(Job::Done { master_file, .. }) => master_file,
        Some(Job::Running) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": { "code": "NOT_READY", "message": "还在处理，请轮询 /v1/covers/{id} 直到 state=done。" } })),
            )
                .into_response()
        }
        Some(Job::Failed { message }) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": { "code": "FAILED", "message": message } })),
            )
                .into_response()
        }
        None => return not_found("找不到这个任务。请重新上传。"),
    };

    let path = state.pipeline.store.path_for(&digest, "wav");
    match std::fs::read(&path) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "audio/wav"),
                (header::CONTENT_DISPOSITION, "inline; filename=\"master.wav\""),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": { "code": "IO", "message": format!("读取成品失败：{e}。产物可能已被清理，请重新处理。") } })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> (Router, String) {
        let root = std::env::current_dir()
            .unwrap_or_default()
            .join(".test-api")
            .join(format!("run-{}", new_job_id()));
        let state = AppState::open(root).expect("打开测试根目录");
        let token = state.token.clone();
        (build_router(state), token)
    }

    #[tokio::test]
    async fn health_is_public() {
        let (router, _) = app();
        let resp = router
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn system_info_requires_token() {
        let (router, _) = app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/v1/system/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn upload_requires_token() {
        let (router, _) = app();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/covers")
                    .body(Body::from(vec![1u8, 2, 3]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn empty_upload_is_rejected_with_reason() {
        let (router, token) = app();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/covers")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_job_is_404() {
        let (router, token) = app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/v1/covers/deadbeef")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 业务端点必须真的走通到 pipeline：喂一个合法 WAV，等任务完成，
    /// 再取成品。这是审计里「能力孤岛」那条的回归测试。
    #[tokio::test]
    async fn full_roundtrip_produces_master() {
        use azzer_dsp::{write_wav_24, AudioBuffer};

        let root = std::env::current_dir()
            .unwrap_or_default()
            .join(".test-api")
            .join(format!("rt-{}", new_job_id()));
        let state = AppState::open(&root).expect("打开测试根目录");
        let token = state.token.clone();
        let jobs = state.jobs.clone();
        let router = build_router(state);

        // 4 秒 220Hz，绕开 N01 的「时长 >3s」断言
        let sr = 48_000u32;
        let n = sr as usize * 4;
        let v: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let mut s = 0.0;
                for h in 1..=4 {
                    s += (2.0 * std::f64::consts::PI * 220.0 * h as f64 * t).sin() / h as f64;
                }
                (s * 0.2) as f32
            })
            .collect();
        let buf = AudioBuffer::new(vec![v], sr, 24).unwrap();
        let src = root.join("src.wav");
        std::fs::create_dir_all(&root).unwrap();
        write_wav_24(&src, &buf).unwrap();
        let bytes = std::fs::read(&src).unwrap();

        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/covers")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let job_id = v["job_id"].as_str().unwrap().to_string();

        // 等任务完成（最多 60 秒）
        let mut final_state = String::new();
        for _ in 0..600 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let j = jobs.lock().unwrap().get(&job_id).cloned();
            match j {
                Some(Job::Running) => continue,
                Some(Job::Done { .. }) => {
                    final_state = "done".into();
                    break;
                }
                Some(Job::Failed { message }) => {
                    panic!("任务失败：{message}");
                }
                None => panic!("任务从表里消失了"),
            }
        }
        assert_eq!(final_state, "done", "任务未在 60 秒内完成");

        // 取成品
        let resp = router
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/covers/{job_id}/master.wav"))
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
            .await
            .unwrap();
        assert!(body.len() > 1000, "成品应是一个真实的 WAV，实得 {} 字节", body.len());
        assert_eq!(&body[0..4], b"RIFF");

        let _ = std::fs::remove_dir_all(&root);
    }
}
