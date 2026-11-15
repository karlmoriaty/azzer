//! Token 鉴权。
//!
//! ## 为什么不用 axum 中间件
//!
//! 试过两种写法，都不生效（无 token 的请求照样进 handler 返回 200）：
//! 1. `Router::route_layer(from_fn_with_state(...))` 加在 `nest` 的子 Router 上
//! 2. `Router::layer(from_fn_with_state(...))` 加在子 Router 上
//!
//! 测试 `routes::tests::system_info_requires_token` 抓到了这个缺陷——
//! 如果只靠手工 curl，很容易误判为"已经加上鉴权了"。
//!
//! 改为在 handler 内显式校验：多一行调用，但行为确定、可测试。
//! 代价是新增端点时必须记得调用 `authorized()`，所以每个受保护端点
//! 都要配一条 401 测试（见 `routes.rs` 的测试模块）。

use axum::http::{header, HeaderMap};

use crate::state::AppState;

/// 校验请求头里的 Bearer token。
pub fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);

    match presented {
        Some(t) => constant_time_eq(t.as_bytes(), state.token.as_bytes()),
        None => false,
    }
}

/// 统一的 401 响应体。文案给出下一步，不出现"未知错误"。
pub fn unauthorized() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "error": {
            "code": "UNAUTHORIZED",
            "message": "缺少或无效的访问令牌。本服务只接受由宿主进程注入的 token。",
            "recoverable": false,
            "suggest": "restart_app"
        }
    }))
}

/// 定长比较，避免按字节短路带来的时间侧信道。
///
/// 长度不等时仍走完整循环（用 0 补齐），保证耗时与内容无关。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..len {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn equal_tokens_pass() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
    }

    #[test]
    fn different_tokens_fail() {
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc", b"abc123"));
        assert!(!constant_time_eq(b"", b"x"));
    }
}
