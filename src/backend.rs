use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use dashmap::DashMap;
use nomo::Diagnostic as NomoDiagnostic;
use nomo::semantic as compiler_semantic;
use nomo_lsp_bridge as lsp_bridge;
use nomo_lsp_bridge::{SemanticSymbol, SemanticSymbolKind, TextPosition};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::formatting::{formatting_edits_for_text, range_formatting_edits_for_text};
use crate::hover::hover_for_document;
use crate::incremental::{IncrementalSession, QueryKey, dependency_paths};
use crate::inlay_hints::inlay_hints_for_text;
use crate::navigation::{
    definition_for_document, prepare_rename_for_document, references_for_document,
    rename_for_document,
};
use crate::semantic;
use crate::symbols::{document_symbols_for_text, workspace_symbols_for_roots};

/// Keywords offered as completion items. Mirrors the v0.1 keyword set from the
/// language whitepaper, including words reserved for upcoming versions so the
/// editor experience stays stable as the compiler grows.
const KEYWORDS: &[&str] = &[
    "package",
    "import",
    "pub",
    "fn",
    "struct",
    "enum",
    "interface",
    "impl",
    "extern",
    "unsafe",
    "let",
    "mut",
    "const",
    "if",
    "else",
    "match",
    "for",
    "in",
    "return",
    "defer",
    "break",
    "continue",
    "panic",
    "as",
    "true",
    "false",
    "void",
];

fn completion_options() -> CompletionOptions {
    CompletionOptions {
        trigger_characters: Some(vec![".".to_string(), " ".to_string(), "[".to_string()]),
        ..Default::default()
    }
}

pub struct Backend {
    client: Client,
    /// In-memory contents of every open document, keyed by its URI.
    documents: DashMap<Url, String>,
    /// Latest LSP version for open documents, used to reject stale diagnostics.
    document_versions: DashMap<Url, i32>,
    /// Workspace roots supplied by the client during initialization.
    workspace_roots: DashMap<String, PathBuf>,
    incremental: IncrementalSession,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
            document_versions: DashMap::new(),
            workspace_roots: DashMap::new(),
            incremental: IncrementalSession::default(),
        }
    }

    /// Run the compiler front-end over the given text and publish the resulting
    /// diagnostics (currently the first error the compiler reports, or none).
    async fn analyze(&self, uri: Url, text: &str, version: Option<i32>) {
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        let module_source_overrides = self.document_overrides();

        let key = QueryKey::for_document(
            "diagnostics",
            path.to_string_lossy(),
            &path,
            text,
            &module_source_overrides,
        );
        let dependencies = dependency_paths(&path, &module_source_overrides);
        let diagnostics = self.incremental.diagnostics(key, dependencies, || {
            diagnostics_for_text(&path, text, &module_source_overrides)
        });

        self.client
            .publish_diagnostics(uri, diagnostics, version)
            .await;
    }

    fn document_overrides(&self) -> Vec<(PathBuf, String)> {
        self.documents
            .iter()
            .map(|entry| {
                let uri = entry.key();
                let path = uri
                    .to_file_path()
                    .unwrap_or_else(|_| PathBuf::from(uri.path()));
                (path, entry.value().clone())
            })
            .collect()
    }

    fn configured_workspace_roots(&self) -> Vec<PathBuf> {
        self.workspace_roots
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    fn document_text(&self, uri: &Url, path: &Path) -> Option<String> {
        self.documents
            .get(uri)
            .map(|text| text.clone())
            .or_else(|| std::fs::read_to_string(path).ok())
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if let Some(workspace_folders) = params.workspace_folders {
            for folder in workspace_folders {
                if let Ok(path) = folder.uri.to_file_path() {
                    self.workspace_roots.insert(folder.uri.to_string(), path);
                }
            }
        } else if let Some(root_uri) = params.root_uri
            && let Ok(path) = root_uri.to_file_path()
        {
            self.workspace_roots.insert(root_uri.to_string(), path);
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "nomo-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(completion_options()),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: semantic::token_types(),
                                token_modifiers: semantic::token_modifiers(),
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: Some(true),
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "nomo.cache.stats".to_string(),
                        "nomo.cache.clear".to_string(),
                    ],
                    work_done_progress_options: Default::default(),
                }),
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
        let version = params.text_document.version;
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        self.incremental.invalidate_path(&path);
        self.documents.insert(uri.clone(), text.clone());
        self.document_versions.insert(uri.clone(), version);
        self.analyze(uri, &text, Some(version)).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        if let Some(change) = params.content_changes.into_iter().last() {
            let text = change.text;
            let path = uri
                .to_file_path()
                .unwrap_or_else(|_| PathBuf::from(uri.path()));
            self.incremental.invalidate_path(&path);
            self.documents.insert(uri.clone(), text.clone());
            self.document_versions.insert(uri.clone(), version);
            self.analyze(uri, &text, Some(version)).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(text) = params
            .text
            .or_else(|| self.documents.get(&uri).map(|t| t.clone()))
        {
            let path = uri
                .to_file_path()
                .unwrap_or_else(|_| PathBuf::from(uri.path()));
            self.incremental.invalidate_path(&path);
            self.documents.insert(uri.clone(), text.clone());
            let version = self.document_versions.get(&uri).map(|value| *value);
            self.analyze(uri, &text, version).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        self.incremental.invalidate_path(&path);
        self.documents.remove(&uri);
        self.document_versions.remove(&uri);
    }

    async fn did_change_watched_files(&self, _params: DidChangeWatchedFilesParams) {
        self.incremental.clear();
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        let text = self
            .documents
            .get(&uri)
            .map(|t| t.clone())
            .or_else(|| std::fs::read_to_string(&path).ok());
        let source_overrides = self.document_overrides();
        let position = params.text_document_position.position;
        let text_for_key = text.as_deref().unwrap_or("");
        let key = QueryKey::for_document(
            "completion",
            format!(
                "{}:{}:{}",
                path.display(),
                position.line,
                position.character
            ),
            &path,
            text_for_key,
            &source_overrides,
        );
        let dependencies = dependency_paths(&path, &source_overrides);
        let items = self.incremental.completions(key, dependencies, || {
            completion_for_document(&path, text.as_deref(), Some(position), &source_overrides)
        });
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let roots = self.configured_workspace_roots();
        let source_overrides = self.document_overrides();
        Ok(Some(workspace_symbols_for_roots(
            &roots,
            &params.query,
            &source_overrides,
        )))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
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

        let source_overrides = self.document_overrides();
        Ok(hover_for_document(
            &path,
            &text,
            params.text_document_position_params.position,
            &source_overrides,
        ))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
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

        let key = QueryKey::for_document(
            "document-symbols",
            path.to_string_lossy(),
            &path,
            &text,
            &[],
        );
        let symbols = self.incremental.document_symbols(key, [path.clone()], || {
            document_symbols_for_text(&path, &text)
        });
        Ok(symbols)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
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

        let source_overrides = self.document_overrides();
        Ok(definition_for_document(
            &path,
            &text,
            uri,
            params.text_document_position_params.position,
            &source_overrides,
        ))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
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

        let source_overrides = self.document_overrides();
        Ok(references_for_document(
            &path,
            &text,
            uri,
            params.text_document_position.position,
            params.context.include_declaration,
            &source_overrides,
        ))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
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

        let source_overrides = self.document_overrides();
        Ok(rename_for_document(
            &path,
            &text,
            uri,
            params.text_document_position.position,
            &params.new_name,
            &source_overrides,
        ))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
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

        let source_overrides = self.document_overrides();
        Ok(prepare_rename_for_document(
            &path,
            &text,
            uri,
            params.position,
            &source_overrides,
        ))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
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

        let source_overrides = self.document_overrides();
        Ok(code_actions_for_text(
            &path,
            &text,
            uri,
            &source_overrides,
            &params.context.diagnostics,
        ))
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

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
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

        Ok(range_formatting_edits_for_text(&path, &text, params.range))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
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

        Ok(Some(inlay_hints_for_text(&path, &text, params.range)))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        let Some(text) = self.document_text(&uri, &path) else {
            return Ok(None);
        };

        let key =
            QueryKey::for_document("semantic-tokens", path.to_string_lossy(), &path, &text, &[]);
        let data = self
            .incremental
            .semantic_tokens(key, [path.clone()], || semantic::tokens(&path, &text));
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        let uri = params.text_document.uri;
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        let Some(text) = self.document_text(&uri, &path) else {
            return Ok(None);
        };

        let range = params.range;
        let key = QueryKey::for_document(
            "semantic-tokens-range",
            format!(
                "{}:{}:{}:{}:{}",
                path.display(),
                range.start.line,
                range.start.character,
                range.end.line,
                range.end.character
            ),
            &path,
            &text,
            &[],
        );
        let data = self.incremental.semantic_tokens(key, [path.clone()], || {
            semantic::tokens_in_range(&path, &text, range)
        });
        Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        match params.command.as_str() {
            "nomo.cache.stats" => {
                let stats = self.incremental.stats();
                Ok(Some(serde_json::json!({
                    "schema": 1,
                    "hits": stats.hits,
                    "misses": stats.misses,
                    "invalidations": stats.invalidations,
                    "entries": stats.entries,
                })))
            }
            "nomo.cache.clear" => Ok(Some(serde_json::json!({
                "removed": self.incremental.clear(),
            }))),
            _ => Ok(None),
        }
    }
}

fn completion_for_document(
    path: &Path,
    text: Option<&str>,
    position: Option<Position>,
    source_overrides: &[(PathBuf, String)],
) -> Vec<CompletionItem> {
    let mut seen = BTreeSet::new();
    let mut items = keyword_completion_items(&mut seen);

    let Some(text) = text else {
        return items;
    };
    if position.is_some_and(|position| is_attribute_completion_position(text, position)) {
        items.extend(attribute_completion_items(&mut seen));
        return items;
    }
    if position.is_some_and(|position| is_import_completion_position(text, position)) {
        items.extend(import_completion_items(
            path,
            text,
            source_overrides,
            &mut seen,
        ));
        return items;
    }

    let mut symbols = if let Ok(project) = nomo::project::discover_project(path) {
        let source_overrides = overrides_with_current(path, text, source_overrides);
        compiler_semantic::symbols_for_project_with_overrides(&project, &source_overrides)
            .unwrap_or_default()
    } else {
        lsp_bridge::symbols_for_text(path, text).unwrap_or_default()
    };
    if let Ok(standard_symbols) =
        compiler_semantic::standard_library_symbols_for_imports(path, text)
    {
        symbols.extend(standard_symbols);
    }
    symbols.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.source_path.cmp(&right.source_path))
            .then(left.line.cmp(&right.line))
    });

    for symbol in symbols {
        if seen.insert(symbol.name.clone()) {
            items.push(completion_item_for_symbol(symbol));
        }
    }
    items
}

fn keyword_completion_items(seen: &mut BTreeSet<String>) -> Vec<CompletionItem> {
    KEYWORDS
        .iter()
        .copied()
        .filter(|kw| seen.insert((*kw).to_string()))
        .map(|kw| CompletionItem {
            label: kw.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect()
}

fn is_attribute_completion_position(text: &str, position: Position) -> bool {
    let Some(line) = text.lines().nth(position.line as usize) else {
        return false;
    };
    let byte_index = utf16_character_to_byte_index(line, position.character);
    let prefix = line[..byte_index.min(line.len())].trim_start();
    let Some(attribute_prefix) = prefix.strip_prefix("#[") else {
        return false;
    };
    attribute_prefix
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn attribute_completion_items(seen: &mut BTreeSet<String>) -> Vec<CompletionItem> {
    seen.insert("test".to_string())
        .then(|| CompletionItem {
            label: "test".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("attribute".to_string()),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "Marks a top-level function for `nomo test` discovery.".to_string(),
            })),
            ..Default::default()
        })
        .into_iter()
        .collect()
}

fn is_import_completion_position(text: &str, position: Position) -> bool {
    let Some(line) = text.lines().nth(position.line as usize) else {
        return false;
    };
    let byte_index = utf16_character_to_byte_index(line, position.character);
    line[..byte_index.min(line.len())]
        .trim_start()
        .starts_with("import")
}

fn import_completion_items(
    path: &Path,
    text: &str,
    source_overrides: &[(PathBuf, String)],
    seen: &mut BTreeSet<String>,
) -> Vec<CompletionItem> {
    let mut imports = nomo::standard_library::all_imports()
        .into_iter()
        .map(|item| (item, CompletionItemKind::MODULE))
        .collect::<Vec<_>>();

    if let Ok(project) = nomo::project::discover_project(path) {
        if let Some(local_root) = local_import_root(text) {
            imports.extend(module_imports_from_source_root(
                &project.root.join("src"),
                &local_root,
                source_overrides,
            ));
        }
        if let Ok(context) = nomo::project::project_module_context(&project) {
            for alias in &context.external_import_roots {
                imports.push((alias.clone(), CompletionItemKind::MODULE));
            }
            for module in &context.external_modules {
                imports.extend(module_imports_from_source_root(
                    &module.source_root,
                    &module.import_root,
                    source_overrides,
                ));
            }
        }
    }

    imports.sort_by(|left, right| left.0.cmp(&right.0));
    imports.dedup_by(|left, right| left.0 == right.0);
    imports
        .into_iter()
        .filter_map(|(label, kind)| {
            seen.insert(label.clone()).then(|| CompletionItem {
                label,
                kind: Some(kind),
                ..Default::default()
            })
        })
        .collect()
}

fn local_import_root(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        let package = trimmed.strip_prefix("package ")?;
        package
            .split('.')
            .next()
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.to_string())
    })
}

fn module_imports_from_source_root(
    source_root: &Path,
    import_root: &str,
    source_overrides: &[(PathBuf, String)],
) -> Vec<(String, CompletionItemKind)> {
    let normalized_source_root = normalize_path(source_root);
    let mut files = Vec::new();
    collect_nomo_files(source_root, &mut files);
    for (path, _) in source_overrides {
        let normalized_path = normalize_path(path);
        if normalized_path.starts_with(&normalized_source_root)
            && path.extension().and_then(|ext| ext.to_str()) == Some("nomo")
        {
            files.push(normalized_path);
        }
    }
    files.sort();
    files.dedup();

    files
        .into_iter()
        .filter_map(|path| {
            module_import_from_file(&normalized_source_root, import_root, &normalize_path(&path))
        })
        .map(|import| (import, CompletionItemKind::MODULE))
        .collect()
}

fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    if let (Some(parent), Some(file_name)) = (path.parent(), path.file_name())
        && let Ok(mut canonical_parent) = std::fs::canonicalize(parent)
    {
        canonical_parent.push(file_name);
        return canonical_parent;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn collect_nomo_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_nomo_files(&path, files);
        } else if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("nomo") {
            files.push(path);
        }
    }
}

fn module_import_from_file(source_root: &Path, import_root: &str, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(source_root).ok()?;
    if relative == Path::new("main.nomo") {
        return Some(format!("{import_root}.main"));
    }
    let mut parts = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let last = parts.last_mut()?;
    if last == "main.nomo" {
        parts.pop();
    } else {
        *last = last.strip_suffix(".nomo")?.to_string();
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{import_root}.{}", parts.join(".")))
}

fn utf16_character_to_byte_index(line: &str, character: u32) -> usize {
    let mut utf16_count = 0u32;
    for (byte_index, ch) in line.char_indices() {
        if utf16_count >= character {
            return byte_index;
        }
        utf16_count += ch.len_utf16() as u32;
    }
    line.len()
}

fn completion_item_for_symbol(symbol: SemanticSymbol) -> CompletionItem {
    CompletionItem {
        label: symbol.name,
        kind: Some(completion_kind(symbol.kind)),
        detail: Some(symbol.signature),
        documentation: (!symbol.docs.is_empty()).then_some({
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: symbol.docs,
            })
        }),
        ..Default::default()
    }
}

fn completion_kind(kind: SemanticSymbolKind) -> CompletionItemKind {
    match kind {
        SemanticSymbolKind::Struct => CompletionItemKind::STRUCT,
        SemanticSymbolKind::Enum => CompletionItemKind::ENUM,
        SemanticSymbolKind::Field => CompletionItemKind::FIELD,
        SemanticSymbolKind::Variant => CompletionItemKind::ENUM_MEMBER,
        SemanticSymbolKind::Interface => CompletionItemKind::INTERFACE,
        SemanticSymbolKind::InterfaceMethod => CompletionItemKind::METHOD,
        SemanticSymbolKind::Const => CompletionItemKind::CONSTANT,
        SemanticSymbolKind::Function => CompletionItemKind::FUNCTION,
        SemanticSymbolKind::ExternFunction => CompletionItemKind::FUNCTION,
        SemanticSymbolKind::Method => CompletionItemKind::METHOD,
    }
}

fn overrides_with_current(
    path: &Path,
    source: &str,
    source_overrides: &[(PathBuf, String)],
) -> Vec<(PathBuf, String)> {
    let mut overrides = source_overrides.to_vec();
    if let Some(existing) = overrides
        .iter_mut()
        .find(|(entry_path, _)| entry_path == path)
    {
        existing.1 = source.to_string();
    } else {
        overrides.push((path.to_path_buf(), source.to_string()));
    }
    overrides
}

fn is_nomo_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return false;
    }
    !KEYWORDS.contains(&name)
}

fn code_actions_for_text(
    path: &Path,
    text: &str,
    uri: Url,
    module_source_overrides: &[(PathBuf, String)],
    diagnostics: &[tower_lsp::lsp_types::Diagnostic],
) -> Option<CodeActionResponse> {
    let diagnostic = first_diagnostic_for_text(path, text, module_source_overrides)?;
    let lsp_diagnostic = diagnostics
        .iter()
        .find(|item| {
            item.code.as_ref().is_some_and(
                |code| matches!(code, NumberOrString::String(value) if value == diagnostic.code),
            )
        })
        .cloned()
        .unwrap_or_else(|| to_lsp_diagnostic(&diagnostic));

    let mut actions = diagnostic
        .suggestions
        .iter()
        .map(|suggestion| {
            CodeActionOrCommand::CodeAction(CodeAction {
                title: suggestion.description.clone(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![lsp_diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        vec![TextEdit {
                            range: suggestion_range(suggestion),
                            new_text: suggestion.text.clone(),
                        }],
                    )])),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                is_preferred: Some(true),
                disabled: None,
                data: None,
            })
        })
        .collect::<Vec<_>>();
    actions.extend(add_import_code_actions(
        path,
        text,
        &uri,
        module_source_overrides,
        &diagnostic,
        &lsp_diagnostic,
    ));
    actions.extend(module_package_code_actions(
        path,
        text,
        &uri,
        module_source_overrides,
        &diagnostic,
        &lsp_diagnostic,
    ));
    Some(actions)
}

fn module_package_code_actions(
    path: &Path,
    text: &str,
    uri: &Url,
    module_source_overrides: &[(PathBuf, String)],
    diagnostic: &NomoDiagnostic,
    lsp_diagnostic: &tower_lsp::lsp_types::Diagnostic,
) -> Vec<CodeActionOrCommand> {
    if diagnostic.code != "E0904"
        || normalize_path(Path::new(&diagnostic.file)) != normalize_path(path)
    {
        return Vec::new();
    }
    let Some(package) = package_declaration(text) else {
        return Vec::new();
    };
    let Some(expected) = expected_package_for_current_file(path, text, module_source_overrides)
    else {
        return Vec::new();
    };
    if package.name == expected {
        return Vec::new();
    }

    let mut actions = vec![CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("update package declaration to match module `{expected}`"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![lsp_diagnostic.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(
                uri.clone(),
                vec![TextEdit {
                    range: package.range,
                    new_text: expected,
                }],
            )])),
            document_changes: None,
            change_annotations: None,
        }),
        command: None,
        is_preferred: Some(true),
        disabled: None,
        data: None,
    })];
    if let Some(target_path) =
        module_file_path_for_package(path, text, module_source_overrides, &package.name)
        && normalize_path(&target_path) != normalize_path(path)
        && !target_path.exists()
        && target_path.parent().is_some_and(Path::is_dir)
        && let Ok(new_uri) = Url::from_file_path(&target_path)
    {
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("rename module file to match package `{}`", package.name),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![lsp_diagnostic.clone()]),
            edit: Some(WorkspaceEdit {
                changes: None,
                document_changes: Some(DocumentChanges::Operations(vec![
                    DocumentChangeOperation::Op(ResourceOp::Rename(RenameFile {
                        old_uri: uri.clone(),
                        new_uri,
                        options: None,
                        annotation_id: None,
                    })),
                ])),
                change_annotations: None,
            }),
            command: None,
            is_preferred: Some(false),
            disabled: None,
            data: None,
        }));
    }
    actions
}

fn add_import_code_actions(
    path: &Path,
    text: &str,
    uri: &Url,
    module_source_overrides: &[(PathBuf, String)],
    diagnostic: &NomoDiagnostic,
    lsp_diagnostic: &tower_lsp::lsp_types::Diagnostic,
) -> Vec<CodeActionOrCommand> {
    if !matches!(diagnostic.code, "E0303" | "E0305") {
        return Vec::new();
    }
    let Some(symbol_name) = diagnostic_symbol_name(diagnostic).or_else(|| {
        lsp_bridge::identifier_at_position(
            text,
            TextPosition {
                line: diagnostic.line.saturating_sub(1) as u32,
                character: diagnostic.column.saturating_sub(1) as u32,
            },
        )
    }) else {
        return Vec::new();
    };
    let Ok(project) = nomo::project::discover_project(path) else {
        return Vec::new();
    };
    let Some(local_root) = local_import_root(text) else {
        return Vec::new();
    };
    let current_imports = imported_paths(text);
    let Ok(context) = nomo::project::project_module_context(&project) else {
        return Vec::new();
    };
    let source_roots = std::iter::once((local_root.as_str(), context.local_source_root.as_path()))
        .chain(
            context
                .external_modules
                .iter()
                .map(|module| (module.import_root.as_str(), module.source_root.as_path())),
        )
        .collect::<Vec<_>>();
    let overrides = overrides_with_current(path, text, module_source_overrides);

    let mut imports = source_roots
        .into_iter()
        .flat_map(|(import_root, source_root)| {
            add_import_candidates_from_source_root(
                source_root,
                import_root,
                path,
                &symbol_name,
                &overrides,
            )
        })
        .filter(|import| import != &format!("{local_root}.main"))
        .filter(|import| !current_imports.contains(import))
        .collect::<Vec<_>>();
    imports.sort();
    imports.dedup();

    let insertion_range = import_insertion_range(text);
    imports
        .into_iter()
        .map(|import| {
            CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("add `import {import}` to use `{symbol_name}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![lsp_diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        vec![TextEdit {
                            range: insertion_range,
                            new_text: format!("import {import}\n"),
                        }],
                    )])),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                is_preferred: Some(false),
                disabled: None,
                data: None,
            })
        })
        .collect()
}

fn add_import_candidates_from_source_root(
    source_root: &Path,
    import_root: &str,
    current_path: &Path,
    symbol_name: &str,
    source_overrides: &[(PathBuf, String)],
) -> Vec<String> {
    let normalized_source_root = normalize_path(source_root);
    let normalized_current_path = normalize_path(current_path);
    let mut files = Vec::new();
    collect_nomo_files(source_root, &mut files);
    for (path, _) in source_overrides {
        let normalized_path = normalize_path(path);
        if normalized_path.starts_with(&normalized_source_root)
            && path.extension().and_then(|ext| ext.to_str()) == Some("nomo")
        {
            files.push(normalized_path);
        }
    }
    files.sort();
    files.dedup();

    let overrides = source_overrides
        .iter()
        .map(|(path, source)| (normalize_path(path), source.clone()))
        .collect::<HashMap<_, _>>();

    files
        .into_iter()
        .filter(|path| normalize_path(path) != normalized_current_path)
        .filter_map(|path| {
            let normalized_path = normalize_path(&path);
            let source = overrides
                .get(&normalized_path)
                .cloned()
                .or_else(|| std::fs::read_to_string(&path).ok())?;
            let has_symbol = lsp_bridge::symbols_for_text(&path, &source)
                .ok()?
                .into_iter()
                .any(|symbol| symbol.name == symbol_name && is_importable_symbol(&symbol));
            has_symbol.then(|| {
                module_import_from_file(&normalized_source_root, import_root, &normalized_path)
            })?
        })
        .collect()
}

fn diagnostic_symbol_name(diagnostic: &NomoDiagnostic) -> Option<String> {
    let (_, rest) = diagnostic.message.split_once('`')?;
    let (name, _) = rest.split_once('`')?;
    is_nomo_identifier(name).then(|| name.to_string())
}

fn is_importable_symbol(symbol: &SemanticSymbol) -> bool {
    matches!(
        symbol.kind,
        SemanticSymbolKind::Struct
            | SemanticSymbolKind::Enum
            | SemanticSymbolKind::Interface
            | SemanticSymbolKind::Const
            | SemanticSymbolKind::Function
    ) && symbol.signature.starts_with("pub ")
}

fn imported_paths(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("import "))
        .map(|import| import.trim().to_string())
        .collect()
}

fn import_insertion_range(text: &str) -> Range {
    let mut line = 1u32;
    for (index, source_line) in text.lines().enumerate() {
        let trimmed = source_line.trim();
        if trimmed.starts_with("import ") || trimmed.starts_with("package ") && line == 1 {
            line = index as u32 + 1;
        }
    }
    Range {
        start: Position { line, character: 0 },
        end: Position { line, character: 0 },
    }
}

fn suggestion_range(suggestion: &nomo::Suggestion) -> Range {
    let line = suggestion.line.saturating_sub(1) as u32;
    let start_char = suggestion.column.saturating_sub(1) as u32;
    let end_char = start_char + suggestion.length as u32;
    Range {
        start: Position {
            line,
            character: start_char,
        },
        end: Position {
            line,
            character: end_char,
        },
    }
}

#[derive(Debug, Clone)]
struct PackageDeclaration {
    name: String,
    line: usize,
    column: usize,
    length: usize,
    line_text: String,
    range: Range,
}

fn package_declaration(text: &str) -> Option<PackageDeclaration> {
    text.lines()
        .enumerate()
        .find_map(|(line_index, source_line)| {
            let trimmed_start = source_line.len() - source_line.trim_start().len();
            let rest = source_line[trimmed_start..].strip_prefix("package ")?;
            let name_start = trimmed_start + "package ".len();
            let name = rest
                .split_whitespace()
                .next()
                .filter(|name| !name.is_empty())?;
            let name_end = name_start + name.len();
            let character_start = source_line[..name_start]
                .chars()
                .map(char::len_utf16)
                .sum::<usize>();
            let character_end = source_line[..name_end]
                .chars()
                .map(char::len_utf16)
                .sum::<usize>();
            Some(PackageDeclaration {
                name: name.to_string(),
                line: line_index + 1,
                column: name_start + 1,
                length: name.len(),
                line_text: source_line.to_string(),
                range: Range {
                    start: Position {
                        line: line_index as u32,
                        character: character_start as u32,
                    },
                    end: Position {
                        line: line_index as u32,
                        character: character_end as u32,
                    },
                },
            })
        })
}

fn expected_package_for_current_file(
    path: &Path,
    text: &str,
    module_source_overrides: &[(PathBuf, String)],
) -> Option<String> {
    let project = nomo::project::discover_project(path).ok()?;
    let context = nomo::project::project_module_context(&project).ok()?;
    let source_root = normalize_path(&context.local_source_root);
    let normalized_path = normalize_path(path);
    if !normalized_path.starts_with(&source_root) {
        return None;
    }
    let local_root = project_main_import_root(&project, module_source_overrides)
        .or_else(|| local_import_root(text))?;
    module_import_from_file(&source_root, &local_root, &normalized_path)
}

fn module_file_path_for_package(
    path: &Path,
    text: &str,
    module_source_overrides: &[(PathBuf, String)],
    package: &str,
) -> Option<PathBuf> {
    let project = nomo::project::discover_project(path).ok()?;
    let context = nomo::project::project_module_context(&project).ok()?;
    let local_root = project_main_import_root(&project, module_source_overrides)
        .or_else(|| local_import_root(text))?;
    let parts = package.split('.').collect::<Vec<_>>();
    if parts.first().copied() != Some(local_root.as_str())
        || parts.iter().any(|part| part.is_empty())
    {
        return None;
    }
    let module_path = &parts[1..];
    if module_path.is_empty() || (module_path.len() == 1 && module_path[0] == "main") {
        return Some(context.local_source_root.join("main.nomo"));
    }
    let mut target = context.local_source_root.clone();
    for segment in module_path {
        target.push(segment);
    }
    target.set_extension("nomo");
    Some(target)
}

fn project_main_import_root(
    project: &nomo::project::Project,
    module_source_overrides: &[(PathBuf, String)],
) -> Option<String> {
    let main = normalize_path(&project.main);
    let main_source = module_source_overrides
        .iter()
        .find(|(path, _)| normalize_path(path) == main)
        .map(|(_, source)| source.clone())
        .or_else(|| std::fs::read_to_string(&project.main).ok())?;
    local_import_root(&main_source)
}

fn diagnostics_for_text(
    path: &Path,
    text: &str,
    module_source_overrides: &[(PathBuf, String)],
) -> Vec<tower_lsp::lsp_types::Diagnostic> {
    first_diagnostic_for_text(path, text, module_source_overrides)
        .map(|diag| vec![to_lsp_diagnostic(&diag)])
        .unwrap_or_default()
}

fn first_diagnostic_for_text(
    path: &Path,
    text: &str,
    module_source_overrides: &[(PathBuf, String)],
) -> Option<NomoDiagnostic> {
    module_package_mismatch_diagnostic(path, text, module_source_overrides)
        .or_else(|| compiler_diagnostic_for_text(path, text, module_source_overrides).err())
}

fn module_package_mismatch_diagnostic(
    path: &Path,
    text: &str,
    module_source_overrides: &[(PathBuf, String)],
) -> Option<NomoDiagnostic> {
    let package = package_declaration(text)?;
    let expected = expected_package_for_current_file(path, text, module_source_overrides)?;
    if package.name == expected {
        return None;
    }
    Some(NomoDiagnostic::new(
        "E0904",
        format!("module `{}` declares package `{}`", expected, package.name),
        path,
        package.line,
        package.column,
        package.length,
        package.line_text,
    ))
}

fn compiler_diagnostic_for_text(
    path: &Path,
    text: &str,
    module_source_overrides: &[(PathBuf, String)],
) -> std::result::Result<(), NomoDiagnostic> {
    if let Ok(project) = nomo::project::discover_project(path) {
        match nomo::project::project_module_context(&project) {
            Ok(context) => nomo::check_source_text_with_project_modules_and_overrides(
                path,
                text,
                Some(&context.local_source_root),
                &context.external_import_roots,
                &context.external_modules,
                module_source_overrides,
            )
            .map(|_| ()),
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
        nomo::check_source_text(path, text).map(|_| ())
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
        code_description: diagnostic_code_description(diag.code),
        source: Some("nomo".to_string()),
        message: diag.message.clone(),
        related_information: None,
        tags: None,
        data: None,
    }
}

fn diagnostic_code_description(code: &str) -> Option<CodeDescription> {
    Url::parse(&nomo::diagnostic::diagnostic_documentation_url(code)?)
        .ok()
        .map(|href| CodeDescription { href })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn completion_options_trigger_after_import_space_and_path_dot() {
        let options = completion_options();

        assert_eq!(
            options.trigger_characters,
            Some(vec![".".to_string(), " ".to_string(), "[".to_string()])
        );
    }

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
    fn diagnostics_accept_builtin_std_imports_without_dependency() {
        let root = temp_test_root("builtin-std-imports");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let source = project.join("src/main.nomo");

        let diagnostics = diagnostics_for_text(
            &source,
            "package app.main\n\nimport std.io\nimport std.path\n\nfn main() -> void {\n    io.println(path.basename(\"/tmp/demo.txt\"))\n}\n",
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
    fn diagnostics_include_code_description_links() {
        let path = PathBuf::from("main.nomo");
        let diagnostics = diagnostics_for_text(&path, "package app.main\n@\n", &[]);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0]
                .code_description
                .as_ref()
                .map(|description| description.href.as_str()),
            Some("https://github.com/nomo-lang/nomo/blob/main/docs/diagnostics/E0102.md")
        );
    }

    #[test]
    fn undocumented_diagnostics_do_not_include_code_description_links() {
        assert!(diagnostic_code_description("E9999").is_none());
    }

    #[test]
    fn completion_includes_keywords_and_current_document_symbols() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\n/// Adds numbers.\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nstruct User {\n    email: string\n}\n";

        let items = completion_for_document(&path, Some(text), None, &[]);

        assert!(
            items.iter().any(|item| {
                item.label == "fn" && item.kind == Some(CompletionItemKind::KEYWORD)
            })
        );
        let add = items.iter().find(|item| item.label == "add").unwrap();
        assert_eq!(add.kind, Some(CompletionItemKind::FUNCTION));
        assert_eq!(
            add.detail.as_deref(),
            Some("pub fn add(a: i64, b: i64) -> i64")
        );
        assert!(matches!(
            add.documentation.as_ref(),
            Some(Documentation::MarkupContent(markup)) if markup.value == "Adds numbers."
        ));
        assert!(
            items.iter().any(|item| {
                item.label == "User" && item.kind == Some(CompletionItemKind::STRUCT)
            })
        );
    }

    #[test]
    fn completion_uses_nested_block_doc_comments() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\n/**\n * Outer docs.\n * /* Nested docs. */\n * Still outer.\n */\npub fn nested() -> void {\n}\n";

        let items = completion_for_document(&path, Some(text), None, &[]);
        let nested = items.iter().find(|item| item.label == "nested").unwrap();

        assert_eq!(nested.kind, Some(CompletionItemKind::FUNCTION));
        assert!(matches!(
            nested.documentation.as_ref(),
            Some(Documentation::MarkupContent(markup))
                if markup.value == "Outer docs.\n/* Nested docs. */\nStill outer."
        ));
    }

    #[test]
    fn completion_keeps_keywords_for_invalid_source() {
        let path = PathBuf::from("main.nomo");

        let items =
            completion_for_document(&path, Some("package app.main\n\nfn main( {\n"), None, &[]);

        assert!(items.iter().any(|item| item.label == "fn"));
        assert!(!items.iter().any(|item| item.label == "main"));
    }

    #[test]
    fn completion_includes_test_attribute_at_attribute_position() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\n#[\nfn checks() -> void {\n}\n";

        let items = completion_for_document(
            &path,
            Some(text),
            Some(Position {
                line: 2,
                character: 2,
            }),
            &[],
        );

        let item = items.iter().find(|item| item.label == "test").unwrap();
        assert_eq!(item.kind, Some(CompletionItemKind::KEYWORD));
        assert_eq!(item.detail.as_deref(), Some("attribute"));
    }

    #[test]
    fn completion_includes_project_module_symbols_with_overlays() {
        let root = temp_test_root("completion-project-overlay");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let math = project.join("src/math.nomo");
        let main_source = "package app.main\n\nimport app.math\n\nfn main() -> void {\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &math,
            "package app.math\n\npub fn sub(a: i64, b: i64) -> i64 {\n    return a - b\n}\n",
        )
        .unwrap();
        let overlay =
            "package app.math\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n";

        let items = completion_for_document(
            &main,
            Some(main_source),
            None,
            &[(math, overlay.to_string())],
        );

        assert!(items.iter().any(|item| {
            item.label == "add" && item.kind == Some(CompletionItemKind::FUNCTION)
        }));
        assert!(!items.iter().any(|item| item.label == "sub"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn import_completion_includes_std_and_local_modules() {
        let root = temp_test_root("import-completion-local");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src/math")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let source = "package app.main\n\nimport \n\nfn main() -> void {\n}\n";
        fs::write(&main, source).unwrap();
        fs::write(project.join("src/math.nomo"), "package app.math\n").unwrap();
        fs::write(
            project.join("src/math/extra.nomo"),
            "package app.math.extra\n",
        )
        .unwrap();
        fs::write(project.join("src/math/main.nomo"), "package app.math\n").unwrap();

        let items = completion_for_document(
            &main,
            Some(source),
            Some(Position {
                line: 2,
                character: 7,
            }),
            &[],
        );

        assert!(items.iter().any(|item| item.label == "std.io"));
        assert!(items.iter().any(|item| item.label == "std.io.println"));
        assert!(items.iter().any(|item| item.label == "std.array.clear"));
        assert!(items.iter().any(|item| item.label == "std.array.iter"));
        assert!(items.iter().any(|item| item.label == "std.array.remove"));
        assert!(items.iter().any(|item| item.label == "std.fs.FileMetadata"));
        assert!(items.iter().any(|item| item.label == "std.fs.metadata"));
        assert!(items.iter().any(|item| item.label == "std.fs.read_bytes"));
        assert!(items.iter().any(|item| item.label == "std.fs.read_dir"));
        assert!(items.iter().any(|item| item.label == "std.fs.write_bytes"));
        assert!(items.iter().any(|item| item.label == "std.debug"));
        assert!(items.iter().any(|item| item.label == "std.debug.backtrace"));
        assert!(items.iter().any(|item| item.label == "std.debug.println"));
        assert!(items.iter().any(|item| item.label == "std.hash"));
        assert!(items.iter().any(|item| item.label == "std.hash.HashState"));
        assert!(items.iter().any(|item| item.label == "std.hash.bytes"));
        assert!(items.iter().any(|item| item.label == "std.hash.string"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.hash.write_bytes")
        );
        assert!(items.iter().any(|item| item.label == "std.crypto"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.crypto.random_bytes")
        );
        assert!(items.iter().any(|item| item.label == "std.crypto.sha256"));
        assert!(items.iter().any(|item| item.label == "std.regex"));
        assert!(items.iter().any(|item| item.label == "std.ffi"));
        assert!(items.iter().any(|item| item.label == "std.ffi.CString"));
        assert!(items.iter().any(|item| item.label == "std.ffi.Opaque"));
        assert!(!items.iter().any(|item| item.label == "std.io.IoError"));
        assert!(items.iter().any(|item| item.label == "std.regex.Regex"));
        assert!(items.iter().any(|item| item.label == "std.regex.compile"));
        assert!(items.iter().any(|item| item.label == "std.regex.captures"));
        assert!(items.iter().any(|item| item.label == "std.json"));
        assert!(items.iter().any(|item| item.label == "std.json.JsonValue"));
        assert!(items.iter().any(|item| item.label == "std.json.parse"));
        assert!(items.iter().any(|item| item.label == "std.json.stringify"));
        assert!(items.iter().any(|item| item.label == "std.http"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.http.HttpExchange")
        );
        assert!(items.iter().any(|item| item.label == "std.http.HttpError"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.http.HttpResponse")
        );
        assert!(items.iter().any(|item| item.label == "std.http.HttpServer"));
        assert!(items.iter().any(|item| item.label == "std.http.accept"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.http.close_exchange")
        );
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.http.close_server")
        );
        assert!(items.iter().any(|item| item.label == "std.http.get"));
        assert!(items.iter().any(|item| item.label == "std.http.listen"));
        assert!(items.iter().any(|item| item.label == "std.http.post"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.http.respond_string")
        );
        assert!(items.iter().any(|item| item.label == "std.net"));
        assert!(items.iter().any(|item| item.label == "std.net.NetError"));
        assert!(items.iter().any(|item| item.label == "std.net.TcpListener"));
        assert!(items.iter().any(|item| item.label == "std.net.TcpStream"));
        assert!(items.iter().any(|item| item.label == "std.net.UdpDatagram"));
        assert!(items.iter().any(|item| item.label == "std.net.UdpSocket"));
        assert!(items.iter().any(|item| item.label == "std.net.connect"));
        assert!(items.iter().any(|item| item.label == "std.net.listen"));
        assert!(items.iter().any(|item| item.label == "std.net.udp_bind"));
        assert!(items.iter().any(|item| item.label == "std.log"));
        assert!(items.iter().any(|item| item.label == "std.log.enabled"));
        assert!(items.iter().any(|item| item.label == "std.log.info"));
        assert!(items.iter().any(|item| item.label == "std.process"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.process.ProcessError")
        );
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.process.ProcessOutput")
        );
        assert!(items.iter().any(|item| item.label == "std.process.exec"));
        assert!(items.iter().any(|item| item.label == "std.process.output"));
        assert!(items.iter().any(|item| item.label == "std.process.spawn"));
        assert!(items.iter().any(|item| item.label == "std.process.status"));
        assert!(items.iter().any(|item| item.label == "std.testing"));
        assert!(items.iter().any(|item| item.label == "std.testing.assert"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.testing.assert_equal")
        );
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.testing.assert_error")
        );
        assert!(items.iter().any(|item| item.label == "std.time.Duration"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.time.duration_millis")
        );
        assert!(
            items
                .iter()
                .any(|item| item.label == "std.time.format_duration")
        );
        assert!(items.iter().any(|item| item.label == "std.time.sleep"));
        assert!(items.iter().any(|item| item.label == "app.math"));
        assert!(items.iter().any(|item| item.label == "app.math.extra"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn import_completion_includes_dependency_modules_and_overlays() {
        let root = temp_test_root("import-completion-dependency");
        reset_dir(&root);
        let dependency = root.join("utils");
        let project = root.join("hello");
        fs::create_dir_all(dependency.join("src/path")).unwrap();
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            dependency.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"utils\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(dependency.join("src/main.nomo"), "package utils.main\n").unwrap();
        fs::write(
            dependency.join("src/path.nomo"),
            "package local_utils.path\n",
        )
        .unwrap();
        let overlay_path = dependency.join("src/path/extra.nomo");
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlocal_utils = { package = \"fynn/utils\", path = \"../utils\" }\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let source = "package app.main\n\nimport local_\n\nfn main() -> void {\n}\n";
        fs::write(&main, source).unwrap();

        let items = completion_for_document(
            &main,
            Some(source),
            Some(Position {
                line: 2,
                character: 13,
            }),
            &[(overlay_path, "package local_utils.path.extra\n".to_string())],
        );

        assert!(items.iter().any(|item| item.label == "local_utils"));
        assert!(items.iter().any(|item| item.label == "local_utils.path"));
        assert!(
            items
                .iter()
                .any(|item| item.label == "local_utils.path.extra")
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn code_actions_return_quick_fix_for_compiler_suggestion() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    io.println(\"Hello\")\n}\n";
        let diagnostics = diagnostics_for_text(&path, text, &[]);

        let actions = code_actions_for_text(&path, text, uri.clone(), &[], &diagnostics).unwrap();

        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected code action");
        };
        assert_eq!(action.title, "add `import std.io` to use `io.println`");
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.diagnostics.as_ref().unwrap().len(), 1);
        let changes = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        let edits = changes.get(&uri).unwrap();
        assert_eq!(
            edits,
            &vec![TextEdit {
                range: Range {
                    start: Position {
                        line: 1,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 0,
                    },
                },
                new_text: "import std.io\n".to_string(),
            }]
        );
    }

    #[test]
    fn code_actions_add_import_for_local_module_symbol() {
        let root = temp_test_root("code-action-add-local-import");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main_path = project.join("src/main.nomo");
        let math_path = project.join("src/math.nomo");
        let text = "package app.main\n\nfn main() -> void {\n    let total: i64 = add(40, 2)\n}\n";
        fs::write(&main_path, text).unwrap();
        fs::write(
            math_path,
            "package app.math\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();
        let uri = Url::from_file_path(&main_path).unwrap();
        let diagnostics = diagnostics_for_text(&main_path, text, &[]);

        let actions =
            code_actions_for_text(&main_path, text, uri.clone(), &[], &diagnostics).unwrap();

        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected code action");
        };
        assert_eq!(action.title, "add `import app.math` to use `add`");
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        let changes = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        let edits = changes.get(&uri).unwrap();
        assert_eq!(
            edits,
            &vec![TextEdit {
                range: Range {
                    start: Position {
                        line: 1,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 0,
                    },
                },
                new_text: "import app.math\n".to_string(),
            }]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn code_actions_add_import_for_dependency_module_symbol() {
        let root = temp_test_root("code-action-add-dependency-import");
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
        let main_path = project.join("src/main.nomo");
        let text = "package app.main\n\nfn main() -> void {\n    let total: i64 = join(40, 2)\n}\n";
        fs::write(&main_path, text).unwrap();
        let uri = Url::from_file_path(&main_path).unwrap();
        let diagnostics = diagnostics_for_text(&main_path, text, &[]);

        let actions =
            code_actions_for_text(&main_path, text, uri.clone(), &[], &diagnostics).unwrap();

        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected code action");
        };
        assert_eq!(action.title, "add `import local_utils.path` to use `join`");
        let changes = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        let edits = changes.get(&uri).unwrap();
        assert_eq!(
            edits,
            &vec![TextEdit {
                range: Range {
                    start: Position {
                        line: 1,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 0,
                    },
                },
                new_text: "import local_utils.path\n".to_string(),
            }]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn code_actions_do_not_add_import_for_private_module_symbol() {
        let root = temp_test_root("code-action-no-private-import");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main_path = project.join("src/main.nomo");
        let math_path = project.join("src/math.nomo");
        let text =
            "package app.main\n\nfn main() -> void {\n    let total: i64 = hidden(40, 2)\n}\n";
        fs::write(&main_path, text).unwrap();
        fs::write(
            math_path,
            "package app.math\n\nfn hidden(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();
        let uri = Url::from_file_path(&main_path).unwrap();
        let diagnostics = diagnostics_for_text(&main_path, text, &[]);

        let actions = code_actions_for_text(&main_path, text, uri, &[], &diagnostics).unwrap();

        assert!(actions.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diagnostics_flag_module_package_mismatch_for_current_file() {
        let root = temp_test_root("diagnostic-current-module-package-mismatch");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(project.join("src/main.nomo"), "package app.main\n").unwrap();
        let module_path = project.join("src/math.nomo");
        let text =
            "package app.other\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n";

        let diagnostics = diagnostics_for_text(&module_path, text, &[]);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(NumberOrString::String("E0904".to_string()))
        );
        assert!(diagnostics[0].message.contains("app.math"));
        assert!(diagnostics[0].message.contains("app.other"));
        assert_eq!(
            diagnostics[0].range,
            Range {
                start: Position {
                    line: 0,
                    character: 8,
                },
                end: Position {
                    line: 0,
                    character: 17,
                },
            }
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn code_actions_update_package_for_module_file_mismatch() {
        let root = temp_test_root("code-action-module-package-mismatch");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(project.join("src/main.nomo"), "package app.main\n").unwrap();
        let module_path = project.join("src/math.nomo");
        let text =
            "package app.other\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n";
        let uri = Url::from_file_path(&module_path).unwrap();
        let diagnostics = diagnostics_for_text(&module_path, text, &[]);

        let actions =
            code_actions_for_text(&module_path, text, uri.clone(), &[], &diagnostics).unwrap();

        assert_eq!(actions.len(), 2);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected code action");
        };
        assert_eq!(
            action.title,
            "update package declaration to match module `app.math`"
        );
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        let changes = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        let edits = changes.get(&uri).unwrap();
        assert_eq!(
            edits,
            &vec![TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 8,
                    },
                    end: Position {
                        line: 0,
                        character: 17,
                    },
                },
                new_text: "app.math".to_string(),
            }]
        );
        let CodeActionOrCommand::CodeAction(action) = &actions[1] else {
            panic!("expected code action");
        };
        assert_eq!(
            action.title,
            "rename module file to match package `app.other`"
        );
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        let Some(DocumentChanges::Operations(operations)) =
            action.edit.as_ref().unwrap().document_changes.as_ref()
        else {
            panic!("expected document change operations");
        };
        assert_eq!(operations.len(), 1);
        let DocumentChangeOperation::Op(ResourceOp::Rename(rename)) = &operations[0] else {
            panic!("expected rename file operation");
        };
        assert_eq!(rename.old_uri, uri);
        assert_eq!(
            rename.new_uri,
            Url::from_file_path(project.join("src/other.nomo")).unwrap()
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn code_actions_return_empty_for_diagnostic_without_suggestions() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let value: i32 = \"bad\"\n}\n";
        let diagnostics = diagnostics_for_text(&path, text, &[]);

        let actions = code_actions_for_text(&path, text, uri, &[], &diagnostics).unwrap();

        assert!(actions.is_empty());
    }

    #[test]
    fn diagnostics_link_registered_ffi_docs() {
        let path = PathBuf::from("main.nomo");
        let text =
            "package app.main\n\nextern \"system\" {\n    fn puts(message: string) -> i32\n}\n";
        let diagnostics = diagnostics_for_text(&path, text, &[]);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(NumberOrString::String("E1511".to_string()))
        );
        assert_eq!(
            diagnostics[0]
                .code_description
                .as_ref()
                .map(|description| description.href.as_str()),
            Some("https://github.com/nomo-lang/nomo/blob/main/docs/diagnostics/E1511.md")
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
