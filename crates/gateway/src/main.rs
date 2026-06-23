use std::{net::SocketAddr, path::PathBuf};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    ryvus_gateway::server::serve(ryvus_gateway::server::GatewayServerConfig {
        project_root: PathBuf::from("."),
        manifest_path: PathBuf::from(".ryvus/action-manifest.json"),
        addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
    })
    .await
    .expect("gateway server failed");
}
