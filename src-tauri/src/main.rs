//! Liminal Salt desktop shell (Tauri v2).
//!
//! There is no child process and no IPC bridge: the same `liminal_salt` library
//! the CLI binary boots runs the Axum server *in-process* on a dynamic loopback
//! port, and a native webview window is pointed straight at it. The shell only
//! adds three things over the CLI:
//!   1. the data dir comes from Tauri's `app_data_dir()` instead of
//!      `config::data_dir()` (the one path seam — see CLAUDE.md "Tauri seam");
//!   2. the server binds to `127.0.0.1:0` so the OS assigns a free port, which
//!      we read back via `Server::local_addr()` to build the window URL;
//!   3. closing the window signals graceful shutdown of the Axum server.
//!
//! Window creation happens on the main thread inside `setup`; the only async
//! work done synchronously there is `bind` (fast: dir seeding + a TCP bind),
//! after which `serve` is spawned for the lifetime of the app.

// Hide the spare console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::sync::Arc;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Notify;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "liminal_salt=info,liminal_salt_desktop=info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            // The one seam that differs from the CLI: resolve the platform data
            // dir off Tauri's path resolver rather than `config::data_dir()`.
            let data_dir = app.path().app_data_dir()?;

            // Bind on a dynamic loopback port. `bind` seeds bundled defaults and
            // binds the listener but does not serve yet, so this returns quickly.
            let addr = SocketAddr::from(([127, 0, 0, 1], 0));
            let server = tauri::async_runtime::block_on(liminal_salt::bind(data_dir, addr))
                .map_err(|e| format!("failed to bind liminal-salt server: {e}"))?;
            let bound = server.local_addr();
            tracing::info!("liminal-salt server bound on http://{bound}");

            // Closing the window fires this; `serve` awaits it and then drains.
            let shutdown = Arc::new(Notify::new());

            let serve_signal = shutdown.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = server
                    .serve(async move { serve_signal.notified().await })
                    .await
                {
                    tracing::error!("liminal-salt server exited with error: {e}");
                }
            });

            // Now that the port is known, point the webview at the running
            // server. Created on the main thread (we're inside `setup`).
            let url: tauri::Url = format!("http://{bound}")
                .parse()
                .map_err(|e| format!("invalid server url: {e}"))?;
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Liminal Salt")
                .inner_size(1100.0, 800.0)
                .min_inner_size(640.0, 480.0)
                .build()?;

            let close_signal = shutdown.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    // Best-effort graceful shutdown; the process exits once the
                    // last window closes regardless, but this lets the Axum
                    // server drain in-flight requests first.
                    close_signal.notify_waiters();
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Liminal Salt desktop shell");
}
