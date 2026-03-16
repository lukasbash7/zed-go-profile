mod analysis;
mod config;
mod format;
mod hints;
mod lenses;
mod paths;
mod profile;
mod server;

use server::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    // Initialize tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("go_profile_lsp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
