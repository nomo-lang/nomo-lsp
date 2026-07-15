#![allow(clippy::result_large_err)]

mod backend;
mod formatting;
mod hover;
mod incremental;
mod inlay_hints;
mod navigation;
mod rename;
mod semantic;
mod symbols;

use backend::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
