//! Azzer 的 HTTP 层。
//!
//! 设计要点（见 v3.2 §1.1）：不用纯 Tauri IPC，而是本地 HTTP。
//! 好处是开发期可以不开 Tauri、直接用浏览器联调；SSE 原生支持；
//! 大文件走流式而不必序列化。
//!
//! 安全模型：绑定 `127.0.0.1:0`（系统分配端口），启动时生成一次性 token，
//! 除 `/health` 外全部要求 `Authorization: Bearer {token}`。
//! token 不落盘、不写日志，只通过 Tauri 的 initialization script 注入 WebView。

pub mod auth;
pub mod routes;
pub mod state;

pub use routes::build_router;
pub use state::{AppState, BoundServer, Job, ServeError};

/// 同步绑定端口，返回 listener 与状态。
///
/// 为什么单独拆出来：Tauri 需要在创建窗口**之前**知道端口与 token，
/// 才能把它们写进 `initialization_script`。而 `axum::serve` 是长驻的 async，
/// 不能先 await 再拿信息。所以先同步绑定，再把 listener 交给异步运行时。
pub fn bind_sync() -> Result<(std::net::TcpListener, AppState), ServeError> {
    let state = AppState::from_env()?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(ServeError::Bind)?;
    listener.set_nonblocking(true).map_err(ServeError::Bind)?;
    Ok((listener, state))
}

/// 在指定根目录上绑定端口。Tauri 壳与测试用这个，避免依赖环境变量。
pub fn bind_sync_with_root(
    root: impl Into<std::path::PathBuf>,
) -> Result<(std::net::TcpListener, AppState), ServeError> {
    let state = AppState::open(root)?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(ServeError::Bind)?;
    listener.set_nonblocking(true).map_err(ServeError::Bind)?;
    Ok((listener, state))
}

/// 在已绑定的 listener 上启动服务。
pub async fn serve_on(
    std_listener: std::net::TcpListener,
    state: AppState,
) -> Result<BoundServer, ServeError> {
    let listener = tokio::net::TcpListener::from_std(std_listener).map_err(ServeError::Bind)?;
    let addr = listener.local_addr().map_err(ServeError::Bind)?;
    let app = build_router(state.clone());
    tracing::info!(port = addr.port(), "azzer api listening");
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "axum serve exited");
        }
    });
    Ok(BoundServer {
        port: addr.port(),
        token: state.token.clone(),
        handle,
    })
}

/// 一步启动（内部先同步绑定再异步服务），供 `serve` bin 使用。
pub async fn serve() -> Result<BoundServer, ServeError> {
    let (listener, state) = bind_sync()?;
    serve_on(listener, state).await
}
