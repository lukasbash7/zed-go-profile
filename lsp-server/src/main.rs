mod analysis;
mod config;
mod diagnostics;
mod format;
mod hints;
mod lenses;
mod paths;
mod profile;
mod server;
mod watch;

use server::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    // Initialize tracing to stderr. Default level is `info`; override with
    // RUST_LOG env var (e.g. RUST_LOG=go_profile_lsp=debug) for debugging.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("go_profile_lsp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
