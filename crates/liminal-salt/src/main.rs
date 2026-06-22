use std::net::SocketAddr;

use liminal_salt::services::config;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "liminal_salt=debug,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let addr = SocketAddr::from(([127, 0, 0, 1], 8420));
    let server = liminal_salt::bind(config::data_dir(), addr).await?;
    let bound = server.local_addr();

    println!();
    println!("Liminal Salt v{}", env!("CARGO_PKG_VERSION"));
    println!("Listening on http://{bound}");
    println!("Press Ctrl-C to stop.");
    println!();

    server
        .serve(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("ctrl_c received, shutting down");
        })
        .await
}
