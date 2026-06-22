pub mod assets;
pub mod handlers;
pub mod middleware;
pub mod routes;
pub mod services;
pub mod tera_extra;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::middleware as axum_mw;
use tera::Tera;
use tower_http::trace::TraceLayer;
use tower_sessions::{
    Expiry, MemoryStore, SessionManagerLayer, cookie::time::Duration as CookieDuration,
};

use crate::{
    middleware::{app_ready, csrf},
    services::{config, memory_worker::MemoryWorker, prompt, prompts},
};

/// Shared state every Axum handler needs access to. Constructed once in
/// `run_server`; `reqwest::Client` internally uses `Arc`, so cheap clones keep
/// one HTTP connection pool alive for the whole process.
///
/// Bundled assets (templates, static, default personas, default prompts) are
/// not held here — they live in the `assets` module as compile-time embedded
/// resources accessed via `crate::assets::*`.
#[derive(Clone)]
pub struct AppState {
    pub tera: Arc<Tera>,
    pub data_dir: PathBuf,
    pub sessions_dir: PathBuf,
    pub http: reqwest::Client,
    /// Shared memory worker — owns per-persona + per-session "already running"
    /// mutexes and the status map for `/memory/status/` polling. Cheap to
    /// clone (`Arc<Inner>` under the hood).
    pub memory: MemoryWorker,
}

/// A bound-but-not-yet-serving Liminal Salt instance, produced by [`bind`].
///
/// The TCP listener is already bound — so [`Server::local_addr`] reports the
/// real port even when [`bind`] was called with port 0 — bundled defaults are
/// seeded, and the router is assembled. Nothing is served yet and no background
/// scheduler is running. Call [`Server::serve`] to run until a shutdown signal.
///
/// Splitting bind from serve is the seam the Tauri shell needs: it reads the
/// dynamically assigned port *before* serving starts (to point the webview
/// window at it) and holds the shutdown handle to abort on window-close. The
/// CLI binary uses the same seam with a fixed port and `ctrl_c`.
pub struct Server {
    addr: SocketAddr,
    listener: tokio::net::TcpListener,
    app: axum::Router,
    state: AppState,
}

impl Server {
    /// The address the listener is actually bound to. When [`bind`] was called
    /// with port 0, this reports the OS-assigned port.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Start the memory schedulers and serve until `shutdown` resolves, then
    /// stop the schedulers. Schedulers start here rather than in [`bind`] so a
    /// caller that binds without serving (e.g. a test inspecting the bound
    /// port) spawns no background work.
    pub async fn serve(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        // Kick off the two memory schedulers. Stopped AFTER the server drains
        // so any in-flight request that dispatches to the worker still finds
        // it alive, and so a scheduler mid-LLM-call gets to finish.
        let scheduler_handles = self.state.memory.start_schedulers(self.state.clone());

        axum::serve(self.listener, self.app)
            .with_graceful_shutdown(shutdown)
            .await?;

        MemoryWorker::stop_schedulers(scheduler_handles).await;
        tracing::info!("schedulers stopped");
        Ok(())
    }
}

/// Build application state, seed bundled defaults into `data_dir`, assemble the
/// router with its middleware stack, and bind a TCP listener at `addr` — but do
/// not serve yet. Returns a [`Server`] whose [`Server::local_addr`] reports the
/// bound address.
///
/// `data_dir` is injected by the caller — the CLI passes `config::data_dir()`,
/// the Tauri shell passes its resolved `app_data_dir()` — so this function holds
/// no opinion about where state lives. That injection is the one data-root seam
/// for the Tauri wrap. Tracing-subscriber setup is likewise the caller's
/// responsibility (CLI wires it in `main`; a Tauri shell uses its own).
pub async fn bind(data_dir: PathBuf, addr: SocketAddr) -> anyhow::Result<Server> {
    let tera = assets::build_tera()?;

    tokio::fs::create_dir_all(&data_dir).await?;
    let sessions_dir = config::sessions_dir(&data_dir);
    tokio::fs::create_dir_all(&sessions_dir).await?;

    // Bundled defaults ship embedded in the binary; seeders materialize them
    // into `<data_dir>/{personas,prompts}/` on first boot. Existing user
    // files are never overwritten.
    prompt::seed_default_personas(&data_dir).await;
    prompts::seed_default_prompts(&data_dir).await;

    let state = AppState {
        tera: Arc::new(tera),
        data_dir,
        sessions_dir,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?,
        memory: MemoryWorker::new(),
    };

    // Session state (current session id, user timezone, CSRF token) lives in a
    // process-local memory store with a two-week inactivity expiry. It is
    // intentionally NOT persisted: each process launch is a fresh session. In a
    // desktop launch the only visible effect is reopening to the default chat —
    // the on-disk chat history (data/sessions/*.json) is unaffected.
    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_name("liminal_salt_session")
        // `Secure = true` (the default) would make browsers reject the cookie
        // on plain http://localhost, silently breaking every POST because a
        // fresh session (with a new CSRF token) gets created per request.
        .with_secure(false)
        .with_expiry(Expiry::OnInactivity(CookieDuration::weeks(2)));

    // Layer order (outer → inner as written; inner runs first at request time):
    //   TraceLayer  (outermost, sees every request)
    //   session_layer  (must run before any middleware that reads the session)
    //   csrf_layer  (needs session)
    //   app_ready  (needs session for the redirect; runs after csrf so we
    //               don't burn CSRF on a request we're about to redirect)
    let app = routes::build_router(state.clone())
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            app_ready::require_app_ready,
        ))
        .layer(axum_mw::from_fn(csrf::require_csrf))
        .layer(session_layer)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;

    Ok(Server {
        addr,
        listener,
        app,
        state,
    })
}
