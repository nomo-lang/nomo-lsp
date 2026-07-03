use std::path::{Path, PathBuf};

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
        let module_source_overrides = self
            .documents
            .iter()
            .filter_map(|entry| {
                let uri = entry.key();
                let path = uri
                    .to_file_path()
                    .unwrap_or_else(|_| PathBuf::from(uri.path()));
                Some((path, entry.value().clone()))
            })
            .collect::<Vec<_>>();

        let diagnostics = diagnostics_for_text(&path, text, &module_source_overrides);

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
                document_formatting_provider: Some(OneOf::Left(true)),
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
        if let Some(text) = params
            .text
            .or_else(|| self.documents.get(&uri).map(|t| t.clone()))
        {
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

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        let Some(text) = self
            .documents
            .get(&uri)
            .map(|t| t.clone())
            .or_else(|| std::fs::read_to_string(&path).ok())
        else {
            return Ok(None);
        };

        Ok(formatting_edits_for_text(&path, &text))
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

fn formatting_edits_for_text(path: &Path, text: &str) -> Option<Vec<TextEdit>> {
    let formatted = nomo::format_source(path, text).ok()?;
    if formatted == text {
        return Some(Vec::new());
    }

    Some(vec![TextEdit {
        range: full_document_range(text),
        new_text: formatted,
    }])
}

fn full_document_range(text: &str) -> Range {
    let mut line = 0;
    let mut character = 0;

    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }

    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position { line, character },
    }
}

fn diagnostics_for_text(
    path: &Path,
    text: &str,
    module_source_overrides: &[(PathBuf, String)],
) -> Vec<tower_lsp::lsp_types::Diagnostic> {
    let result = if let Ok(project) = nomo::project::discover_project(path) {
        match nomo::project::project_module_context(&project) {
            Ok(context) => nomo::check_source_text_with_project_modules_and_overrides(
                path,
                text,
                Some(&context.local_source_root),
                &context.external_import_roots,
                &context.external_modules,
                module_source_overrides,
            ),
            Err(message) => Err(NomoDiagnostic::new(
                "E0901",
                message,
                &project.root.join("nomo.toml"),
                1,
                1,
                1,
                "",
            )),
        }
    } else {
        nomo::check_source_text(path, text)
    };
    match result {
        Ok(_) => Vec::new(),
        Err(diag) => vec![to_lsp_diagnostic(&diag)],
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn diagnostics_accept_dependency_alias_imports_from_nearest_manifest() {
        let root = temp_test_root("alias-imports");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\njson = { package = \"nomo-lang/json\", version = \"0.1.0\" }\n",
        )
        .unwrap();
        let source = project.join("src/main.nomo");

        let diagnostics = diagnostics_for_text(
            &source,
            "package app.main\n\nimport json.parser\n\nfn main() -> void {\n}\n",
            &[],
        );

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diagnostics_accept_local_project_module_imports() {
        let root = temp_test_root("local-module-imports");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(
            project.join("src/math.nomo"),
            "package app.math\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();
        let source = project.join("src/main.nomo");

        let diagnostics = diagnostics_for_text(
            &source,
            "package app.main\n\nimport app.math\n\nfn main() -> void {\n    let total: i64 = add(40, 2)\n}\n",
            &[],
        );

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diagnostics_use_open_document_overlay_for_imported_modules() {
        let root = temp_test_root("local-module-overlay");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let module_path = project.join("src/math.nomo");
        fs::write(
            &module_path,
            "package app.math\n\nfn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();
        let source = project.join("src/main.nomo");

        let diagnostics = diagnostics_for_text(
            &source,
            "package app.main\n\nimport app.math\n\nfn main() -> void {\n    let total: i64 = add(40, 2)\n}\n",
            &[(
                module_path,
                "package app.math\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n"
                    .to_string(),
            )],
        );

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diagnostics_accept_path_dependency_module_imports() {
        let root = temp_test_root("path-dependency-module-imports");
        reset_dir(&root);
        let dependency = root.join("utils");
        let project = root.join("hello");
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            dependency.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"utils\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(dependency.join("src/main.nomo"), "package utils.main\n").unwrap();
        fs::write(
            dependency.join("src/path.nomo"),
            "package local_utils.path\n\npub fn join(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlocal_utils = { package = \"fynn/utils\", path = \"../utils\" }\n",
        )
        .unwrap();
        let source = project.join("src/main.nomo");

        let diagnostics = diagnostics_for_text(
            &source,
            "package app.main\n\nimport local_utils.path\n\nfn main() -> void {\n    let total: i64 = join(40, 2)\n}\n",
            &[],
        );

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diagnostics_reject_dependency_alias_imports_without_manifest() {
        let root = temp_test_root("alias-imports-no-manifest");
        reset_dir(&root);
        let source = root.join("main.nomo");

        let diagnostics = diagnostics_for_text(
            &source,
            "package app.main\n\nimport json.parser\n\nfn main() -> void {\n}\n",
            &[],
        );

        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("json.parser"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn formatting_formats_standalone_source_text() {
        let path = PathBuf::from("main.nomo");
        let edits = formatting_edits_for_text(
            &path,
            "package app . main\nfn main(){\nlet message:string=\"hi\"\n}\n",
        )
        .unwrap();

        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].range,
            Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 4,
                    character: 0,
                },
            }
        );
        assert_eq!(
            edits[0].new_text,
            "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n}\n"
        );
    }

    #[test]
    fn formatting_returns_empty_edits_for_already_formatted_text() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n}\n";

        let edits = formatting_edits_for_text(&path, text).unwrap();

        assert!(edits.is_empty());
    }

    #[test]
    fn formatting_returns_none_for_invalid_source() {
        let path = PathBuf::from("main.nomo");

        let edits = formatting_edits_for_text(&path, "package app.main\n\nfn main( {\n");

        assert!(edits.is_none());
    }

    #[test]
    fn formatting_formats_script_body_source_text() {
        let path = PathBuf::from("script.nomo");
        let edits = formatting_edits_for_text(
            &path,
            "package app.main\nimport std.io\nlet message:string=\"hi\"\nio.println(message)\n",
        )
        .unwrap();

        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].new_text,
            "package app.main\n\nimport std.io\n\nlet message: string = \"hi\"\nio.println(message)\n"
        );
    }

    fn reset_dir(path: &Path) {
        if path.exists() {
            fs::remove_dir_all(path).unwrap();
        }
        fs::create_dir_all(path).unwrap();
    }

    fn temp_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nomo-lsp-backend-test-{name}-{}",
            std::process::id()
        ))
    }
}
