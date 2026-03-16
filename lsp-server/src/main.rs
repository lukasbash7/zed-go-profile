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
    // Initialize tracing — write to a log file so we can inspect when running under Zed.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/go-profile-lsp.log")
        .expect("failed to open log file");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("go_profile_lsp=debug".parse().unwrap()),
        )
        .with_writer(log_file)
        .with_ansi(false)
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
