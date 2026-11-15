// Windows 发布版不弹控制台窗口；调试版保留，方便看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    // 先同步绑定端口：窗口创建时要把端口与 token 写进 initialization script，
    // 所以这两样必须在 build 窗口之前拿到。
    let (listener, state) = match azzer_api::bind_sync() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "绑定本地端口失败");
            std::process::exit(1);
        }
    };
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or_default();
    let token = state.token.clone();

    tauri::Builder::default()
        .setup(move |app| {
            // 注入 bridge：前端据此拼 baseUrl 与 Authorization。
            // 用 initialization_script 而不是页面加载后 eval，保证脚本执行时变量已就绪。
            let script = format!(
                "window.__AZZER__ = {{ baseUrl: 'http://127.0.0.1:{port}', token: '{token}' }};"
            );

            let url = tauri::WebviewUrl::App("index.html".into());
            tauri::WebviewWindowBuilder::new(app, "main", url)
                .title("Azzer")
                .inner_size(1360.0, 860.0)
                .min_inner_size(1024.0, 640.0)
                .initialization_script(&script)
                .build()?;

            tauri::async_runtime::spawn(async move {
                if let Err(e) = azzer_api::serve_on(listener, state).await {
                    tracing::error!(error = %e, "api 启动失败");
                }
            });

            tracing::info!(port, "azzer shell ready");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri 启动失败");
}
