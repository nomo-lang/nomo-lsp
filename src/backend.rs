use std::path::PathBuf;

use dashmap::DashMap;
use nomo::Diagnostic as NomoDiagnostic;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::semantic;

/// Keywords offered as completion items. Mirrors the v0.1 keyword set from the
/// language whitepaper, including words reserved for upcoming versions so the
/// editor experience stays stable as the compiler grows.
const KEYWORDS: &[&str] = &[
    "package", "import", "pub", "fn", "struct", "enum", "impl", "let", "mut", "const", "if",
    "else", "match", "for", "in", "return", "defer", "break", "continue", "panic", "as", "true",
    "false", "void",
];

pub struct Backend {
    client: Client,
    /// In-memory contents of every open document, keyed by its URI.
    documents: DashMap<Url, String>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
        }
    }

    /// Run the compiler front-end over the given text and publish the resulting
    /// diagnostics (currently the first error the compiler reports, or none).
    async fn analyze(&self, uri: Url, text: &str) {
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));

        let diagnostics = match nomo::check_source_text(&path, text) {
            Ok(_) => Vec::new(),
            Err(diag) => vec![to_lsp_diagnostic(&diag)],
        };

        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "nomo-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: None,
                    ..Default::default()
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: semantic::token_types(),
                                token_modifiers: vec![],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "nomo-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.documents.insert(uri.clone(), text.clone());
        self.analyze(uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().last() {
            let text = change.text;
            self.documents.insert(uri.clone(), text.clone());
            self.analyze(uri, &text).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(text) = params.text.or_else(|| self.documents.get(&uri).map(|t| t.clone())) {
            self.documents.insert(uri.clone(), text.clone());
            self.analyze(uri, &text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.remove(&params.text_document.uri);
    }

    async fn completion(&self, _params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let items = KEYWORDS
            .iter()
            .map(|kw| CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            })
            .collect();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|t| t.clone()) else {
            return Ok(None);
        };
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));

        let data = semantic::tokens(&path, &text);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }
}

/// Convert a compiler diagnostic (1-based line/column) into an LSP diagnostic
/// (0-based positions).
fn to_lsp_diagnostic(diag: &NomoDiagnostic) -> tower_lsp::lsp_types::Diagnostic {
    let line = diag.line.saturating_sub(1) as u32;
    let start_char = diag.column.saturating_sub(1) as u32;
    let end_char = start_char + diag.length.max(1) as u32;

    tower_lsp::lsp_types::Diagnostic {
        range: Range {
            start: Position {
                line,
                character: start_char,
            },
            end: Position {
                line,
                character: end_char,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String(diag.code.to_string())),
        code_description: None,
        source: Some("nomo".to_string()),
        message: diag.message.clone(),
        related_information: None,
        tags: None,
        data: None,
    }
}
