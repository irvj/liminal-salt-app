//! Integration tests for the library boot seam (`bind` / `Server::serve`).
//!
//! These exercise the exact lifecycle the Tauri shell depends on: bind to a
//! dynamic port, read the bound address back *before* serving starts, drive a
//! real request over TCP, then shut down cleanly via the injected signal. User
//! state lives in a tempdir — `data_dir` is a parameter now, so nothing touches
//! the repo-relative default data dir.

use std::net::SocketAddr;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::oneshot;

/// Loopback with port 0 — the OS assigns a free port, mirroring how the Tauri
/// shell binds so it can read the real port back for the window URL.
fn loopback_port_0() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

#[tokio::test]
async fn bind_reports_os_assigned_port() {
    let dir = TempDir::new().unwrap();
    let server = liminal_salt::bind(dir.path().to_path_buf(), loopback_port_0())
        .await
        .expect("bind should succeed");

    let addr = server.local_addr();
    assert!(addr.ip().is_loopback(), "should bind loopback, got {addr}");
    assert_ne!(
        addr.port(),
        0,
        "port 0 must resolve to a concrete OS-assigned port the caller can read back"
    );
}

#[tokio::test]
async fn bind_seeds_bundled_defaults_into_injected_dir() {
    let dir = TempDir::new().unwrap();
    liminal_salt::bind(dir.path().to_path_buf(), loopback_port_0())
        .await
        .expect("bind should succeed");

    // Seeding + dir creation ran against the injected data dir, not the
    // repo-relative default — this is what makes the Tauri app_data_dir wiring
    // work and what keeps these tests hermetic.
    let sessions = dir.path().join("sessions");
    let personas = dir.path().join("personas");
    let prompts = dir.path().join("prompts");

    assert!(sessions.is_dir(), "sessions dir should be created");
    assert!(
        personas.is_dir() && personas.read_dir().unwrap().next().is_some(),
        "default personas should be seeded into the injected data dir"
    );
    assert!(
        prompts.is_dir() && prompts.read_dir().unwrap().next().is_some(),
        "default prompts should be seeded into the injected data dir"
    );
}

#[tokio::test]
async fn serve_responds_then_shuts_down_on_signal() {
    let dir = TempDir::new().unwrap();
    let server = liminal_salt::bind(dir.path().to_path_buf(), loopback_port_0())
        .await
        .expect("bind should succeed");
    let addr = server.local_addr();

    // serve() runs for the app's lifetime; drive it on a task and hold the
    // shutdown sender, exactly as the Tauri shell holds it to fire on
    // window-close.
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        server
            .serve(async {
                let _ = shutdown_rx.await;
            })
            .await
    });

    // The bound port actually serves — proving the readback in
    // `bind_reports_os_assigned_port` points at a live server. `/health` is
    // exempt from the app-ready redirect, so it answers before setup.
    let body = reqwest::get(format!("http://{addr}/health"))
        .await
        .expect("request to the bound server")
        .text()
        .await
        .expect("health body");
    assert_eq!(body, "ok");

    // Fire the injected shutdown signal; serve() must drain and return Ok, and
    // the task must complete promptly (not hang on the scheduler stop path).
    shutdown_tx.send(()).expect("send shutdown signal");
    let joined = tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("serve should shut down promptly after the signal")
        .expect("serve task should not panic");
    joined.expect("serve should return Ok on clean shutdown");
}
