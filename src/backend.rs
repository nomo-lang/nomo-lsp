use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use dashmap::DashMap;
use nomo::Diagnostic as NomoDiagnostic;
use nomo::ast::{BinaryOp, Expr, ForVariant, SourceFile, Span, Stmt, TypeRef};
use nomo::semantic as compiler_semantic;
use nomo::semantic::{SemanticSymbol, SemanticSymbolKind, TextPosition, TextRange};
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

const STD_IMPORTS: &[&str] = &[
    "std.array",
    "std.array.Array",
    "std.array.get",
    "std.array.len",
    "std.array.new",
    "std.array.push",
    "std.array.set",
    "std.char",
    "std.char.is_alpha",
    "std.char.is_digit",
    "std.char.is_whitespace",
    "std.char.to_string",
    "std.env",
    "std.env.args",
    "std.env.cwd",
    "std.env.get",
    "std.env.home_dir",
    "std.env.set",
    "std.env.temp_dir",
    "std.fs",
    "std.fs.File",
    "std.fs.FsError",
    "std.fs.open",
    "std.fs.read_to_string",
    "std.fs.write_string",
    "std.io",
    "std.io.IoError",
    "std.io.eprint",
    "std.io.eprintln",
    "std.io.print",
    "std.io.println",
    "std.io.read_line",
    "std.math",
    "std.math.abs",
    "std.math.ceil",
    "std.math.cos",
    "std.math.floor",
    "std.math.max",
    "std.math.min",
    "std.math.pow",
    "std.math.round",
    "std.math.sin",
    "std.math.sqrt",
    "std.option",
    "std.option.Option",
    "std.option.and_then",
    "std.option.is_none",
    "std.option.is_some",
    "std.option.map",
    "std.option.unwrap_or",
    "std.path",
    "std.path.basename",
    "std.path.dirname",
    "std.path.extension",
    "std.path.is_absolute",
    "std.path.join",
    "std.path.normalize",
    "std.result",
    "std.result.Result",
    "std.result.and_then",
    "std.result.is_err",
    "std.result.is_ok",
    "std.result.map",
    "std.result.map_err",
    "std.result.unwrap_or",
    "std.string",
    "std.string.contains",
    "std.string.concat",
    "std.string.ends_with",
    "std.string.is_empty",
    "std.string.len",
    "std.string.split",
    "std.string.starts_with",
    "std.string.to_lower",
    "std.string.to_upper",
    "std.string.trim",
];

pub struct Backend {
    client: Client,
    /// In-memory contents of every open document, keyed by its URI.
    documents: DashMap<Url, String>,
    /// Workspace roots supplied by the client during initialization.
    workspace_roots: DashMap<String, PathBuf>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
            workspace_roots: DashMap::new(),
        }
    }

    /// Run the compiler front-end over the given text and publish the resulting
    /// diagnostics (currently the first error the compiler reports, or none).
    async fn analyze(&self, uri: Url, text: &str) {
        let path = uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(uri.path()));
        let module_source_overrides = self.document_overrides();

        let diagnostics = diagnostics_for_text(&path, text, &module_source_overrides);

        self.client
            .publish_diagnostics(uri, diagnostics, None)
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
        } else if let Some(root_uri) = params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                self.workspace_roots.insert(root_uri.to_string(), path);
            }
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
                completion_provider: Some(CompletionOptions {
                    trigger_characters: None,
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
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
        Ok(Some(CompletionResponse::Array(completion_for_document(
            &path,
            text.as_deref(),
            Some(params.text_document_position.position),
            &source_overrides,
        ))))
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

        Ok(document_symbols_for_text(&path, &text))
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

fn hover_for_text(path: &Path, text: &str, position: Position) -> Option<Hover> {
    let item = compiler_semantic::symbol_at_position(path, text, to_compiler_position(position))
        .ok()??;

    hover_for_symbol(&item)
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
        compiler_semantic::symbols_for_text(path, text).unwrap_or_default()
    };
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
        .filter_map(|kw| {
            seen.insert((*kw).to_string()).then(|| CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            })
        })
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
    let mut imports = STD_IMPORTS
        .iter()
        .map(|item| ((*item).to_string(), CompletionItemKind::MODULE))
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
    if let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) {
        if let Ok(mut canonical_parent) = std::fs::canonicalize(parent) {
            canonical_parent.push(file_name);
            return canonical_parent;
        }
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
        documentation: (!symbol.docs.is_empty()).then(|| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: symbol.docs,
            })
        }),
        ..Default::default()
    }
}

fn workspace_symbols_for_roots(
    roots: &[PathBuf],
    query: &str,
    source_overrides: &[(PathBuf, String)],
) -> Vec<SymbolInformation> {
    let query = query.to_ascii_lowercase();
    let mut seen_projects = BTreeSet::new();
    let mut seen_symbols = BTreeSet::new();
    let mut items = Vec::new();

    for project in projects_for_roots(roots) {
        if !seen_projects.insert(project.root.clone()) {
            continue;
        }
        let Ok(symbols) =
            compiler_semantic::symbols_for_project_with_overrides(&project, source_overrides)
        else {
            continue;
        };
        for symbol in symbols {
            if !query.is_empty() && !symbol.name.to_ascii_lowercase().contains(&query) {
                continue;
            }
            let key = (
                symbol.source_path.clone(),
                symbol.name.clone(),
                symbol.selection_range.start.line,
                symbol.selection_range.start.character,
            );
            if !seen_symbols.insert(key) {
                continue;
            }
            if let Some(item) = symbol_information(symbol) {
                items.push(item);
            }
        }
    }

    items.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.location.uri.cmp(&right.location.uri))
            .then(
                left.location
                    .range
                    .start
                    .line
                    .cmp(&right.location.range.start.line),
            )
            .then(
                left.location
                    .range
                    .start
                    .character
                    .cmp(&right.location.range.start.character),
            )
    });
    items
}

fn projects_for_roots(roots: &[PathBuf]) -> Vec<nomo::project::Project> {
    let mut projects = Vec::new();
    for root in roots {
        if let Ok(workspace) = nomo::project::discover_workspace(root) {
            projects.extend(workspace.members);
            continue;
        }
        if let Ok(project) = nomo::project::discover_project(root) {
            projects.push(project);
        }
    }
    projects
}

fn symbol_information(symbol: SemanticSymbol) -> Option<SymbolInformation> {
    Some(SymbolInformation {
        name: symbol.name,
        kind: lsp_symbol_kind(symbol.kind),
        tags: None,
        #[allow(deprecated)]
        deprecated: None,
        location: Location {
            uri: Url::from_file_path(&symbol.source_path).ok()?,
            range: to_lsp_range(symbol.selection_range),
        },
        container_name: symbol
            .source_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string()),
    })
}

fn completion_kind(kind: SemanticSymbolKind) -> CompletionItemKind {
    match kind {
        SemanticSymbolKind::Struct => CompletionItemKind::STRUCT,
        SemanticSymbolKind::Enum => CompletionItemKind::ENUM,
        SemanticSymbolKind::Const => CompletionItemKind::CONSTANT,
        SemanticSymbolKind::Function => CompletionItemKind::FUNCTION,
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

fn hover_for_document(
    path: &Path,
    text: &str,
    position: Position,
    source_overrides: &[(PathBuf, String)],
) -> Option<Hover> {
    let compiler_position = to_compiler_position(position);
    let item = if let Ok(project) = nomo::project::discover_project(path) {
        compiler_semantic::symbol_at_project_position(
            &project,
            path,
            text,
            compiler_position,
            source_overrides,
        )
        .ok()?
    } else {
        compiler_semantic::symbol_at_position(path, text, compiler_position).ok()?
    }?;

    hover_for_symbol(&item)
}

fn hover_for_symbol(item: &SemanticSymbol) -> Option<Hover> {
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: hover_markdown(item),
        }),
        range: None,
    })
}

#[allow(deprecated)]
fn document_symbols_for_text(path: &Path, text: &str) -> Option<DocumentSymbolResponse> {
    let symbols = compiler_semantic::symbols_for_text(path, text).ok()?;
    let items = symbols
        .into_iter()
        .map(|item| DocumentSymbol {
            name: item.name,
            detail: Some(item.signature),
            kind: lsp_symbol_kind(item.kind),
            tags: None,
            deprecated: None,
            range: to_lsp_range(item.range),
            selection_range: to_lsp_range(item.selection_range),
            children: None,
        })
        .collect::<Vec<_>>();

    Some(DocumentSymbolResponse::Nested(items))
}

fn definition_for_text(
    path: &Path,
    text: &str,
    uri: Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let range = compiler_semantic::definition_for_text(path, text, to_compiler_position(position))
        .ok()??;

    Some(GotoDefinitionResponse::Scalar(Location {
        uri,
        range: to_lsp_range(range),
    }))
}

fn definition_for_document(
    path: &Path,
    text: &str,
    uri: Url,
    position: Position,
    source_overrides: &[(PathBuf, String)],
) -> Option<GotoDefinitionResponse> {
    let compiler_position = to_compiler_position(position);
    if let Ok(project) = nomo::project::discover_project(path) {
        let location = compiler_semantic::definition_for_project_text(
            &project,
            path,
            text,
            compiler_position,
            source_overrides,
        )
        .ok()??;
        return Some(GotoDefinitionResponse::Scalar(to_lsp_location(location)?));
    }
    definition_for_text(path, text, uri, position)
}

fn references_for_text(
    path: &Path,
    text: &str,
    uri: Url,
    position: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let ranges = compiler_semantic::references_for_text(
        path,
        text,
        to_compiler_position(position),
        include_declaration,
    )
    .ok()??;
    Some(
        ranges
            .iter()
            .map(|range| Location {
                uri: uri.clone(),
                range: to_lsp_range(*range),
            })
            .collect::<Vec<_>>(),
    )
}

fn references_for_document(
    path: &Path,
    text: &str,
    uri: Url,
    position: Position,
    include_declaration: bool,
    source_overrides: &[(PathBuf, String)],
) -> Option<Vec<Location>> {
    let compiler_position = to_compiler_position(position);
    if let Ok(project) = nomo::project::discover_project(path) {
        let locations = compiler_semantic::references_for_project_text(
            &project,
            path,
            text,
            compiler_position,
            include_declaration,
            source_overrides,
        )
        .ok()??;
        return locations.into_iter().map(to_lsp_location).collect();
    }
    references_for_text(path, text, uri, position, include_declaration)
}

fn rename_for_document(
    path: &Path,
    text: &str,
    uri: Url,
    position: Position,
    new_name: &str,
    source_overrides: &[(PathBuf, String)],
) -> Option<WorkspaceEdit> {
    if !is_nomo_identifier(new_name) {
        return None;
    }
    let locations = references_for_document(path, text, uri, position, true, source_overrides)?;
    if locations.is_empty() {
        return None;
    }

    let mut changes = HashMap::<Url, Vec<TextEdit>>::new();
    for location in locations {
        let edits = changes.entry(location.uri).or_default();
        if edits.iter().any(|edit| edit.range == location.range) {
            continue;
        }
        edits.push(TextEdit {
            range: location.range,
            new_text: new_name.to_string(),
        });
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
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

fn to_lsp_location(location: compiler_semantic::SemanticLocation) -> Option<Location> {
    Some(Location {
        uri: Url::from_file_path(location.path).ok()?,
        range: to_lsp_range(location.range),
    })
}

fn hover_markdown(item: &SemanticSymbol) -> String {
    let mut value = format!("```nomo\n{}\n```", item.signature);
    if !item.docs.is_empty() {
        value.push_str("\n\n");
        value.push_str(&item.docs);
    }
    value.push_str("\n\n");
    value.push_str(semantic_kind_label(item.kind));
    value
}

fn semantic_kind_label(kind: SemanticSymbolKind) -> &'static str {
    match kind {
        SemanticSymbolKind::Struct => "struct",
        SemanticSymbolKind::Enum => "enum",
        SemanticSymbolKind::Const => "const",
        SemanticSymbolKind::Function => "function",
        SemanticSymbolKind::Method => "method",
    }
}

fn lsp_symbol_kind(kind: SemanticSymbolKind) -> SymbolKind {
    match kind {
        SemanticSymbolKind::Struct => SymbolKind::STRUCT,
        SemanticSymbolKind::Enum => SymbolKind::ENUM,
        SemanticSymbolKind::Const => SymbolKind::CONSTANT,
        SemanticSymbolKind::Function => SymbolKind::FUNCTION,
        SemanticSymbolKind::Method => SymbolKind::METHOD,
    }
}

fn to_compiler_position(position: Position) -> TextPosition {
    TextPosition {
        line: position.line,
        character: position.character,
    }
}

fn to_lsp_position(position: TextPosition) -> Position {
    Position {
        line: position.line,
        character: position.character,
    }
}

fn to_lsp_range(range: TextRange) -> Range {
    Range {
        start: to_lsp_position(range.start),
        end: to_lsp_position(range.end),
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

fn inlay_hints_for_text(path: &Path, text: &str, range: Range) -> Vec<InlayHint> {
    let Ok(tokens) = nomo::lex(path, text) else {
        return Vec::new();
    };
    let Ok(ast) = nomo::parser::parse(path, &tokens) else {
        return Vec::new();
    };

    let mut hints = Vec::new();
    collect_inlay_hints_from_file(&ast, &range, &mut hints);
    hints
}

fn collect_inlay_hints_from_file(file: &SourceFile, range: &Range, hints: &mut Vec<InlayHint>) {
    for function in &file.functions {
        collect_inlay_hints_from_stmts(&function.body, range, hints);
    }
    for impl_block in &file.impls {
        for method in &impl_block.methods {
            collect_inlay_hints_from_stmts(&method.body, range, hints);
        }
    }
    collect_inlay_hints_from_stmts(&file.script_body, range, hints);
}

fn collect_inlay_hints_from_stmts(stmts: &[Stmt], range: &Range, hints: &mut Vec<InlayHint>) {
    for stmt in stmts {
        match stmt {
            Stmt::Let {
                name,
                type_annotation: None,
                value,
                span,
                ..
            } => {
                if let (Some(position), Some(label)) =
                    (let_name_end_position(span, name), infer_hint_type(value))
                {
                    if position_in_range(position, range) {
                        hints.push(type_inlay_hint(position, label));
                    }
                }
            }
            Stmt::Let { .. } => {}
            Stmt::LetElse { else_body, .. } => {
                collect_inlay_hints_from_stmts(else_body, range, hints)
            }
            Stmt::IfLet {
                body, else_body, ..
            } => {
                collect_inlay_hints_from_stmts(body, range, hints);
                if let Some(else_body) = else_body {
                    collect_inlay_hints_from_stmts(else_body, range, hints);
                }
            }
            Stmt::Match { arms, .. } => {
                for arm in arms {
                    collect_inlay_hints_from_stmts(&arm.body, range, hints);
                }
            }
            Stmt::For { variant, .. } => match variant {
                ForVariant::Infinite { body }
                | ForVariant::While { body, .. }
                | ForVariant::Iterate { body, .. } => {
                    collect_inlay_hints_from_stmts(body, range, hints)
                }
            },
            Stmt::Defer { stmt, .. } => {
                collect_inlay_hints_from_stmts(std::slice::from_ref(stmt), range, hints);
            }
            Stmt::Assign { .. }
            | Stmt::Postfix { .. }
            | Stmt::Return { .. }
            | Stmt::Expr { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. } => {}
        }
    }
}

fn type_inlay_hint(position: Position, label: String) -> InlayHint {
    InlayHint {
        position,
        label: InlayHintLabel::String(format!(": {label}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: None,
        padding_right: Some(true),
        data: None,
    }
}

fn let_name_end_position(span: &Span, name: &str) -> Option<Position> {
    let mut name_start = span.column.saturating_sub(1) + "let".len();
    name_start = skip_ascii_whitespace(&span.text, name_start);
    if span.text[name_start..].starts_with("mut")
        && span
            .text
            .as_bytes()
            .get(name_start + "mut".len())
            .is_some_and(u8::is_ascii_whitespace)
    {
        name_start += "mut".len();
        name_start = skip_ascii_whitespace(&span.text, name_start);
    }
    if !span.text[name_start..].starts_with(name) {
        return None;
    }
    let character = span.text[..name_start].encode_utf16().count() + name.encode_utf16().count();
    Some(Position {
        line: span.line.saturating_sub(1) as u32,
        character: character as u32,
    })
}

fn skip_ascii_whitespace(text: &str, mut byte_index: usize) -> usize {
    while text
        .as_bytes()
        .get(byte_index)
        .is_some_and(u8::is_ascii_whitespace)
    {
        byte_index += 1;
    }
    byte_index
}

fn position_in_range(position: Position, range: &Range) -> bool {
    compare_positions(position, range.start) != std::cmp::Ordering::Less
        && compare_positions(position, range.end) == std::cmp::Ordering::Less
}

fn compare_positions(left: Position, right: Position) -> std::cmp::Ordering {
    left.line
        .cmp(&right.line)
        .then(left.character.cmp(&right.character))
}

fn infer_hint_type(expr: &Expr) -> Option<String> {
    match expr {
        Expr::String(_) => Some("string".to_string()),
        Expr::Int(_) => Some("i64".to_string()),
        Expr::Float(_) => Some("f64".to_string()),
        Expr::Char(_) => Some("char".to_string()),
        Expr::Bool(_) => Some("bool".to_string()),
        Expr::StructLiteral { type_name, .. } => Some(type_name.join(".")),
        Expr::Cast { target, .. } => Some(type_ref_label(target)),
        Expr::Unary { op: _, expr } => match infer_hint_type(expr).as_deref() {
            Some("bool") => Some("bool".to_string()),
            _ => None,
        },
        Expr::Binary { left, op, right } => infer_binary_hint_type(left, op, right),
        Expr::If {
            then_branch,
            else_branch,
            ..
        } => same_hint_type(then_branch, else_branch),
        Expr::Match { arms, .. } => {
            let mut inferred = arms.iter().filter_map(|arm| infer_hint_type(&arm.value));
            let first = inferred.next()?;
            inferred.all(|label| label == first).then_some(first)
        }
        Expr::Call { .. }
        | Expr::Name(_)
        | Expr::Question { .. }
        | Expr::MutArg { .. }
        | Expr::Panic { .. }
        | Expr::Void => None,
    }
}

fn infer_binary_hint_type(left: &Expr, op: &BinaryOp, right: &Expr) -> Option<String> {
    match op {
        BinaryOp::Equal
        | BinaryOp::NotEqual
        | BinaryOp::Less
        | BinaryOp::LessEqual
        | BinaryOp::Greater
        | BinaryOp::GreaterEqual
        | BinaryOp::LogicalAnd
        | BinaryOp::LogicalOr => Some("bool".to_string()),
        BinaryOp::Add
        | BinaryOp::Subtract
        | BinaryOp::Multiply
        | BinaryOp::Divide
        | BinaryOp::Remainder
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::ShiftLeft
        | BinaryOp::ShiftRight
        | BinaryOp::BitAnd
        | BinaryOp::BitAndNot => same_hint_type(left, right),
    }
}

fn same_hint_type(left: &Expr, right: &Expr) -> Option<String> {
    let left = infer_hint_type(left)?;
    let right = infer_hint_type(right)?;
    (left == right).then_some(left)
}

fn type_ref_label(type_ref: &TypeRef) -> String {
    if type_ref.args.is_empty() {
        type_ref.path.join(".")
    } else {
        format!(
            "{}<{}>",
            type_ref.path.join("."),
            type_ref
                .args
                .iter()
                .map(type_ref_label)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn code_actions_for_text(
    path: &Path,
    text: &str,
    uri: Url,
    module_source_overrides: &[(PathBuf, String)],
    diagnostics: &[tower_lsp::lsp_types::Diagnostic],
) -> Option<CodeActionResponse> {
    let diagnostic = compiler_diagnostic_for_text(path, text, module_source_overrides).err()?;
    if diagnostic.suggestions.is_empty() {
        return Some(Vec::new());
    }
    let lsp_diagnostic = diagnostics
        .iter()
        .find(|item| {
            item.code.as_ref().is_some_and(
                |code| matches!(code, NumberOrString::String(value) if value == diagnostic.code),
            )
        })
        .cloned()
        .unwrap_or_else(|| to_lsp_diagnostic(&diagnostic));

    let actions = diagnostic
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
    Some(actions)
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
    match compiler_diagnostic_for_text(path, text, module_source_overrides) {
        Ok(_) => Vec::new(),
        Err(diag) => vec![to_lsp_diagnostic(&diag)],
    }
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
    fn completion_keeps_keywords_for_invalid_source() {
        let path = PathBuf::from("main.nomo");

        let items =
            completion_for_document(&path, Some("package app.main\n\nfn main( {\n"), None, &[]);

        assert!(items.iter().any(|item| item.label == "fn"));
        assert!(!items.iter().any(|item| item.label == "main"));
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
    fn workspace_symbols_include_project_symbols() {
        let root = temp_test_root("workspace-symbol-project");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(
            project.join("src/main.nomo"),
            "package app.main\n\npub struct User {\n    email: string\n}\n\npub fn make_user() -> User {\n    return User { email: \"hi\" }\n}\n",
        )
        .unwrap();

        let symbols = workspace_symbols_for_roots(std::slice::from_ref(&project), "user", &[]);

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["User", "make_user"]
        );
        assert!(symbols.iter().all(|symbol| {
            symbol
                .location
                .uri
                .to_file_path()
                .unwrap()
                .starts_with(&project)
        }));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_symbols_include_workspace_members() {
        let root = temp_test_root("workspace-symbol-members");
        reset_dir(&root);
        let app = root.join("apps/cli");
        let core = root.join("packages/core");
        fs::create_dir_all(app.join("src")).unwrap();
        fs::create_dir_all(core.join("src")).unwrap();
        fs::write(
            root.join("nomo.toml"),
            "[workspace]\nmembers = [\"apps/*\", \"packages/*\"]\n\n[workspace.package]\nnamespace = \"fynn\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(
            app.join("nomo.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            app.join("src/main.nomo"),
            "package app.main\n\npub fn run_cli() -> void {\n}\n",
        )
        .unwrap();
        fs::write(
            core.join("nomo.toml"),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            core.join("src/main.nomo"),
            "package core.main\n\npub fn run_core() -> void {\n}\n",
        )
        .unwrap();

        let symbols = workspace_symbols_for_roots(std::slice::from_ref(&root), "run_", &[]);

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["run_cli", "run_core"]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_symbols_use_open_document_overlays() {
        let root = temp_test_root("workspace-symbol-overlay");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let module = project.join("src/math.nomo");
        fs::write(
            project.join("src/main.nomo"),
            "package app.main\n\nfn main() -> void {\n}\n",
        )
        .unwrap();
        fs::write(
            &module,
            "package app.math\n\npub fn stale_name() -> i64 {\n    return 1\n}\n",
        )
        .unwrap();
        let overlay = "package app.math\n\npub fn fresh_name() -> i64 {\n    return 1\n}\n";

        let symbols = workspace_symbols_for_roots(
            std::slice::from_ref(&project),
            "fresh",
            &[(module, overlay.to_string())],
        );

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "fresh_name");
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
    fn code_actions_return_empty_for_diagnostic_without_suggestions() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let value: i32 = \"bad\"\n}\n";
        let diagnostics = diagnostics_for_text(&path, text, &[]);

        let actions = code_actions_for_text(&path, text, uri, &[], &diagnostics).unwrap();

        assert!(actions.is_empty());
    }

    #[test]
    fn hover_returns_function_signature_and_doc_comment() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\n/// Adds two numbers.\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";

        let hover = hover_for_text(
            &path,
            text,
            Position {
                line: 8,
                character: 22,
            },
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("pub fn add(a: i64, b: i64) -> i64"));
        assert!(markup.value.contains("Adds two numbers."));
        assert!(markup.value.contains("function"));
    }

    #[test]
    fn hover_returns_struct_signature_and_block_doc_comment() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\n/** User record.\n * Stores identity fields.\n */\npub struct User {\n    pub id: string\n}\n\nfn main() -> void {\n    let user: User = User { id: \"1\" }\n}\n";

        let hover = hover_for_text(
            &path,
            text,
            Position {
                line: 10,
                character: 14,
            },
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("pub struct User"));
        assert!(markup.value.contains("User record."));
        assert!(markup.value.contains("Stores identity fields."));
    }

    #[test]
    fn hover_returns_method_signature_and_doc_comment() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nstruct User {\n    email: string\n}\n\nimpl User {\n    /// Reads the stored email.\n    pub fn email(self) -> string {\n        return self.email\n    }\n}\n\nfn main() -> void {\n    let user: User = User { email: \"hi\" }\n    let email: string = user.email()\n}\n";

        let hover = hover_for_text(
            &path,
            text,
            Position {
                line: 15,
                character: 30,
            },
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(
            markup
                .value
                .contains("pub fn User.email(self: User) -> string")
        );
        assert!(markup.value.contains("Reads the stored email."));
        assert!(markup.value.contains("method"));
    }

    #[test]
    fn hover_returns_none_for_unknown_identifier() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n}\n";

        let hover = hover_for_text(
            &path,
            text,
            Position {
                line: 3,
                character: 8,
            },
        );

        assert!(hover.is_none());
    }

    #[test]
    fn document_symbols_return_top_level_declarations_and_methods() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\npub struct User {\n    email: string\n}\n\nconst MAX: i64 = 10\n\nimpl User {\n    pub fn email(self) -> string {\n        return self.email\n    }\n}\n\nfn main() -> void {\n}\n";

        let Some(DocumentSymbolResponse::Nested(symbols)) = document_symbols_for_text(&path, text)
        else {
            panic!("expected document symbols");
        };

        let names = symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["User", "MAX", "main", "email"]);
        assert_eq!(symbols[0].kind, SymbolKind::STRUCT);
        assert_eq!(symbols[1].kind, SymbolKind::CONSTANT);
        assert_eq!(symbols[2].kind, SymbolKind::FUNCTION);
        assert_eq!(symbols[3].kind, SymbolKind::METHOD);
        assert_eq!(
            symbols[0].selection_range,
            Range {
                start: Position {
                    line: 2,
                    character: 11,
                },
                end: Position {
                    line: 2,
                    character: 15,
                },
            }
        );
        assert_eq!(
            symbols[3].detail.as_deref(),
            Some("pub fn User.email(self: User) -> string")
        );
    }

    #[test]
    fn document_symbols_return_none_for_invalid_source() {
        let path = PathBuf::from("main.nomo");

        let symbols = document_symbols_for_text(&path, "package app.main\n\nfn main( {\n");

        assert!(symbols.is_none());
    }

    #[test]
    fn definition_returns_function_declaration_location() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";

        let definition = definition_for_text(
            &path,
            text,
            uri.clone(),
            Position {
                line: 7,
                character: 22,
            },
        )
        .unwrap();

        let GotoDefinitionResponse::Scalar(location) = definition else {
            panic!("expected scalar definition location");
        };
        assert_eq!(location.uri, uri);
        assert_eq!(
            location.range,
            Range {
                start: Position {
                    line: 2,
                    character: 3,
                },
                end: Position {
                    line: 2,
                    character: 6,
                },
            }
        );
    }

    #[test]
    fn definition_returns_type_declaration_location() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\npub struct User {\n    email: string\n}\n\nfn main() -> void {\n    let user: User = User { email: \"hi\" }\n}\n";

        let definition = definition_for_text(
            &path,
            text,
            uri,
            Position {
                line: 7,
                character: 14,
            },
        )
        .unwrap();

        let GotoDefinitionResponse::Scalar(location) = definition else {
            panic!("expected scalar definition location");
        };
        assert_eq!(
            location.range,
            Range {
                start: Position {
                    line: 2,
                    character: 11,
                },
                end: Position {
                    line: 2,
                    character: 15,
                },
            }
        );
    }

    #[test]
    fn definition_returns_none_for_unknown_identifier() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n}\n";

        let definition = definition_for_text(
            &path,
            text,
            uri,
            Position {
                line: 3,
                character: 8,
            },
        );

        assert!(definition.is_none());
    }

    #[test]
    fn references_return_current_document_identifier_locations() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> void {\n    let first: i64 = add(1, 2)\n    let second: i64 = add(first, 3)\n}\n";

        let references = references_for_text(
            &path,
            text,
            uri.clone(),
            Position {
                line: 7,
                character: 23,
            },
            true,
        )
        .unwrap();

        assert_eq!(references.len(), 3);
        assert!(references.iter().all(|location| location.uri == uri));
        assert_eq!(
            references
                .iter()
                .map(|location| location.range)
                .collect::<Vec<_>>(),
            vec![
                Range {
                    start: Position {
                        line: 2,
                        character: 3,
                    },
                    end: Position {
                        line: 2,
                        character: 6,
                    },
                },
                Range {
                    start: Position {
                        line: 7,
                        character: 21,
                    },
                    end: Position {
                        line: 7,
                        character: 24,
                    },
                },
                Range {
                    start: Position {
                        line: 8,
                        character: 22,
                    },
                    end: Position {
                        line: 8,
                        character: 25,
                    },
                },
            ]
        );
    }

    #[test]
    fn references_can_exclude_declaration() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nstruct User {\n    email: string\n}\n\nfn main() -> void {\n    let user: User = User { email: \"hi\" }\n}\n";

        let references = references_for_text(
            &path,
            text,
            uri,
            Position {
                line: 7,
                character: 14,
            },
            false,
        )
        .unwrap();

        assert_eq!(
            references
                .iter()
                .map(|location| location.range)
                .collect::<Vec<_>>(),
            vec![
                Range {
                    start: Position {
                        line: 7,
                        character: 14,
                    },
                    end: Position {
                        line: 7,
                        character: 18,
                    },
                },
                Range {
                    start: Position {
                        line: 7,
                        character: 21,
                    },
                    end: Position {
                        line: 7,
                        character: 25,
                    },
                },
            ]
        );
    }

    #[test]
    fn references_return_none_for_unknown_identifier() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n}\n";

        let references = references_for_text(
            &path,
            text,
            uri,
            Position {
                line: 3,
                character: 8,
            },
            true,
        );

        assert!(references.is_none());
    }

    #[test]
    fn rename_returns_current_document_workspace_edit() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";

        let edit = rename_for_document(
            &path,
            text,
            uri.clone(),
            Position {
                line: 7,
                character: 22,
            },
            "sum",
            &[],
        )
        .unwrap();

        let changes = edit.changes.unwrap();
        let edits = changes.get(&uri).unwrap();
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.new_text == "sum"));
        assert_eq!(
            edits
                .iter()
                .map(|edit| edit.range.start)
                .collect::<Vec<_>>(),
            vec![
                Position {
                    line: 2,
                    character: 3,
                },
                Position {
                    line: 7,
                    character: 21,
                },
            ]
        );
    }

    #[test]
    fn rename_rejects_invalid_identifier() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n";

        let edit = rename_for_document(
            &path,
            text,
            uri,
            Position {
                line: 2,
                character: 4,
            },
            "for",
            &[],
        );

        assert!(edit.is_none());
    }

    #[test]
    fn definition_returns_cross_file_project_location() {
        let root = temp_test_root("semantic-definition-project");
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
        let main_source = "package app.main\n\nimport app.math\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &math,
            "package app.math\n\n/// Adds numbers.\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();

        let definition = definition_for_document(
            &main,
            main_source,
            Url::from_file_path(&main).unwrap(),
            Position {
                line: 5,
                character: 23,
            },
            &[],
        )
        .unwrap();

        let GotoDefinitionResponse::Scalar(location) = definition else {
            panic!("expected scalar definition location");
        };
        assert_eq!(location.uri, Url::from_file_path(&math).unwrap());
        assert_eq!(
            location.range,
            Range {
                start: Position {
                    line: 3,
                    character: 7,
                },
                end: Position {
                    line: 3,
                    character: 10,
                },
            }
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hover_uses_cross_file_project_symbol_docs() {
        let root = temp_test_root("semantic-hover-project");
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
        let main_source = "package app.main\n\nimport app.math\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &math,
            "package app.math\n\n/// Adds numbers.\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();

        let hover = hover_for_document(
            &main,
            main_source,
            Position {
                line: 5,
                character: 23,
            },
            &[],
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("pub fn add(a: i64, b: i64) -> i64"));
        assert!(markup.value.contains("Adds numbers."));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn references_return_cross_file_project_locations_with_overlays() {
        let root = temp_test_root("semantic-references-project-overlay");
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
        let main_source = "package app.main\n\nimport app.math\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &math,
            "package app.math\n\npub fn sub(a: i64, b: i64) -> i64 {\n    return a - b\n}\n",
        )
        .unwrap();
        let overlay =
            "package app.math\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n";

        let references = references_for_document(
            &main,
            main_source,
            Url::from_file_path(&main).unwrap(),
            Position {
                line: 5,
                character: 23,
            },
            true,
            &[(math.clone(), overlay.to_string())],
        )
        .unwrap();

        assert!(references.iter().any(|location| {
            location.uri == Url::from_file_path(&main).unwrap()
                && location.range.start
                    == Position {
                        line: 5,
                        character: 21,
                    }
        }));
        assert!(references.iter().any(|location| {
            location.uri == Url::from_file_path(&math).unwrap()
                && location.range.start
                    == Position {
                        line: 2,
                        character: 7,
                    }
        }));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rename_returns_cross_file_project_workspace_edit() {
        let root = temp_test_root("semantic-rename-project");
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
        let main_source = "package app.main\n\nimport app.math\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &math,
            "package app.math\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();
        let main_uri = Url::from_file_path(&main).unwrap();
        let math_uri = Url::from_file_path(&math).unwrap();

        let edit = rename_for_document(
            &main,
            main_source,
            main_uri.clone(),
            Position {
                line: 5,
                character: 23,
            },
            "sum",
            &[],
        )
        .unwrap();

        let changes = edit.changes.unwrap();
        let main_edits = changes.get(&main_uri).unwrap();
        let math_edits = changes.get(&math_uri).unwrap();
        assert_eq!(main_edits.len(), 1);
        assert_eq!(math_edits.len(), 1);
        assert_eq!(main_edits[0].new_text, "sum");
        assert_eq!(math_edits[0].new_text, "sum");
        assert_eq!(
            main_edits[0].range.start,
            Position {
                line: 5,
                character: 21,
            }
        );
        assert_eq!(
            math_edits[0].range.start,
            Position {
                line: 2,
                character: 7,
            }
        );
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

    #[test]
    fn inlay_hints_include_unannotated_let_literal_types() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nfn main() -> void {\n    let message = \"hi\"\n    let count = 1\n    let ok = true\n    let et = 'x'\n    let mut mutable = 3.14\n    let explicit: string = \"shown in source\"\n}\n";

        let hints = inlay_hints_for_text(&path, text, full_document_range(text));

        let labels = hints
            .iter()
            .map(|hint| match &hint.label {
                InlayHintLabel::String(label) => label.as_str(),
                InlayHintLabel::LabelParts(_) => "",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![": string", ": i64", ": bool", ": char", ": f64"]
        );
        assert_eq!(
            hints[0].position,
            Position {
                line: 3,
                character: 15,
            }
        );
        assert!(
            hints
                .iter()
                .all(|hint| hint.kind == Some(InlayHintKind::TYPE))
        );
        assert_eq!(
            hints[3].position,
            Position {
                line: 6,
                character: 10,
            }
        );
        assert_eq!(
            hints[4].position,
            Position {
                line: 7,
                character: 19,
            }
        );
    }

    #[test]
    fn inlay_hints_include_nested_and_expression_types() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nstruct Label {\n    value: string\n}\n\nfn main() -> void {\n    for {\n        let label = Label { value: \"hi\" }\n        let casted = 1 as i32\n        let compared = 1 < 2\n        let uncertain = load()\n        break\n    }\n}\n";

        let hints = inlay_hints_for_text(&path, text, full_document_range(text));

        let labels = hints
            .iter()
            .map(|hint| match &hint.label {
                InlayHintLabel::String(label) => label.as_str(),
                InlayHintLabel::LabelParts(_) => "",
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, vec![": Label", ": i32", ": bool"]);
    }

    #[test]
    fn inlay_hints_respect_requested_range() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nfn main() -> void {\n    let first = \"hi\"\n    let second = 1\n}\n";
        let range = Range {
            start: Position {
                line: 4,
                character: 0,
            },
            end: Position {
                line: 4,
                character: u32::MAX,
            },
        };

        let hints = inlay_hints_for_text(&path, text, range);

        assert_eq!(hints.len(), 1);
        assert_eq!(
            hints[0].position,
            Position {
                line: 4,
                character: 14,
            }
        );
        match &hints[0].label {
            InlayHintLabel::String(label) => assert_eq!(label, ": i64"),
            InlayHintLabel::LabelParts(_) => panic!("expected string label"),
        }
    }

    #[test]
    fn inlay_hints_return_empty_for_invalid_source() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nfn main( {\n";

        let hints = inlay_hints_for_text(&path, text, full_document_range(text));

        assert!(hints.is_empty());
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
