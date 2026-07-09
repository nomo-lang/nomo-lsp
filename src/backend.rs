use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use dashmap::DashMap;
use nomo::Diagnostic as NomoDiagnostic;
use nomo::ast::{BinaryOp, Expr, ForVariant, SourceFile, Span, Stmt, TypeRef};
use nomo::lexer::{Token, TokenKind};
use nomo::semantic as compiler_semantic;
use nomo::semantic::{SemanticSymbol, SemanticSymbolKind, TextPosition, TextRange};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::formatting::{formatting_edits_for_text, range_formatting_edits_for_text};
use crate::semantic;

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

const STD_IMPORTS: &[&str] = &[
    "std.array",
    "std.array.Array",
    "std.array.clear",
    "std.array.get",
    "std.array.insert",
    "std.array.iter",
    "std.array.len",
    "std.array.new",
    "std.array.pop",
    "std.array.push",
    "std.array.remove",
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
    "std.fs.FileMetadata",
    "std.fs.FsError",
    "std.fs.create_dir",
    "std.fs.exists",
    "std.fs.metadata",
    "std.fs.open",
    "std.fs.read_bytes",
    "std.fs.read_to_string",
    "std.fs.read_dir",
    "std.fs.remove_dir",
    "std.fs.write_bytes",
    "std.fs.write_string",
    "std.debug",
    "std.debug.backtrace",
    "std.debug.panic",
    "std.debug.print",
    "std.debug.println",
    "std.log",
    "std.log.debug",
    "std.log.enabled",
    "std.log.error",
    "std.log.info",
    "std.log.warn",
    "std.io",
    "std.io.IoError",
    "std.io.eprint",
    "std.io.eprintln",
    "std.io.print",
    "std.io.println",
    "std.io.read_line",
    "std.hash",
    "std.hash.HashState",
    "std.hash.bytes",
    "std.hash.finish",
    "std.hash.new",
    "std.hash.string",
    "std.hash.write_bytes",
    "std.hash.write_string",
    "std.crypto",
    "std.crypto.random_bytes",
    "std.crypto.sha256",
    "std.crypto.sha512",
    "std.regex",
    "std.regex.Regex",
    "std.regex.RegexError",
    "std.regex.captures",
    "std.regex.compile",
    "std.regex.is_match",
    "std.json",
    "std.json.JsonError",
    "std.json.JsonValue",
    "std.json.parse",
    "std.json.stringify",
    "std.http",
    "std.http.HttpExchange",
    "std.http.HttpError",
    "std.http.HttpResponse",
    "std.http.HttpServer",
    "std.http.accept",
    "std.http.close_exchange",
    "std.http.close_server",
    "std.http.get",
    "std.http.listen",
    "std.http.post",
    "std.http.respond_string",
    "std.net",
    "std.net.NetError",
    "std.net.TcpListener",
    "std.net.TcpStream",
    "std.net.UdpDatagram",
    "std.net.UdpSocket",
    "std.net.connect",
    "std.net.listen",
    "std.net.udp_bind",
    "std.collections",
    "std.collections.StringMap",
    "std.collections.StringSet",
    "std.collections.map_contains",
    "std.collections.map_get",
    "std.collections.map_len",
    "std.collections.map_new",
    "std.collections.map_remove",
    "std.collections.map_set",
    "std.collections.set_contains",
    "std.collections.set_insert",
    "std.collections.set_len",
    "std.collections.set_new",
    "std.collections.set_remove",
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
    "std.num",
    "std.num.NumError",
    "std.num.checked_add",
    "std.num.checked_mul",
    "std.num.checked_sub",
    "std.num.parse_f64",
    "std.num.parse_i64",
    "std.num.parse_u64",
    "std.num.wrapping_add",
    "std.num.wrapping_mul",
    "std.num.wrapping_sub",
    "std.option",
    "std.option.Option",
    "std.option.and_then",
    "std.option.is_none",
    "std.option.is_some",
    "std.option.map",
    "std.option.unwrap_or",
    "std.os",
    "std.os.arch",
    "std.os.line_ending",
    "std.os.path_separator",
    "std.os.platform",
    "std.path",
    "std.path.basename",
    "std.path.dirname",
    "std.path.extension",
    "std.path.is_absolute",
    "std.path.join",
    "std.path.normalize",
    "std.process",
    "std.process.ProcessError",
    "std.process.ProcessOutput",
    "std.process.exec",
    "std.process.exit",
    "std.process.output",
    "std.process.spawn",
    "std.process.status",
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
    "std.testing",
    "std.testing.assert",
    "std.testing.assert_equal",
    "std.testing.assert_error",
    "std.time",
    "std.time.Duration",
    "std.time.duration_as_millis",
    "std.time.duration_millis",
    "std.time.duration_seconds",
    "std.time.format_duration",
    "std.time.monotonic_millis",
    "std.time.now_millis",
    "std.time.sleep",
    "std.time.sleep_millis",
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

        let data = semantic::tokens(&path, &text);
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

        let data = semantic::tokens_in_range(&path, &text, params.range);
        Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }
}

#[cfg(test)]
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
        let Ok(mut symbols) =
            compiler_semantic::symbols_for_project_with_overrides(&project, source_overrides)
        else {
            continue;
        };
        if let Ok(dependency_symbols) =
            compiler_semantic::dependency_symbols_for_project_with_overrides(
                &project,
                source_overrides,
            )
        {
            symbols.extend(dependency_symbols);
        }
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
    let mut items = Vec::<DocumentSymbol>::new();
    for symbol in symbols {
        match symbol.kind {
            SemanticSymbolKind::Field => {
                let Some(owner) = field_owner_name(&symbol.signature).map(str::to_string) else {
                    items.push(document_symbol(symbol));
                    continue;
                };
                if let Err(symbol) =
                    push_child_symbol(&mut items, &owner, SymbolKind::STRUCT, symbol)
                {
                    items.push(document_symbol(symbol));
                }
            }
            SemanticSymbolKind::Variant => {
                let Some(owner) = variant_owner_name(&symbol.signature).map(str::to_string) else {
                    items.push(document_symbol(symbol));
                    continue;
                };
                if let Err(symbol) = push_child_symbol(&mut items, &owner, SymbolKind::ENUM, symbol)
                {
                    items.push(document_symbol(symbol));
                }
            }
            SemanticSymbolKind::InterfaceMethod => {
                let Some(owner) =
                    interface_method_owner_name(&symbol.signature).map(str::to_string)
                else {
                    items.push(document_symbol(symbol));
                    continue;
                };
                if let Err(symbol) =
                    push_child_symbol(&mut items, &owner, SymbolKind::INTERFACE, symbol)
                {
                    items.push(document_symbol(symbol));
                }
            }
            _ => items.push(document_symbol(symbol)),
        }
    }

    Some(DocumentSymbolResponse::Nested(items))
}

#[allow(deprecated)]
fn document_symbol(item: SemanticSymbol) -> DocumentSymbol {
    DocumentSymbol {
        name: item.name,
        detail: Some(item.signature),
        kind: lsp_symbol_kind(item.kind),
        tags: None,
        deprecated: None,
        range: to_lsp_range(item.range),
        selection_range: to_lsp_range(item.selection_range),
        children: None,
    }
}

#[allow(deprecated)]
fn push_child_symbol(
    items: &mut [DocumentSymbol],
    owner: &str,
    owner_kind: SymbolKind,
    child: SemanticSymbol,
) -> std::result::Result<(), SemanticSymbol> {
    let Some(parent) = items
        .iter_mut()
        .find(|item| item.name == owner && item.kind == owner_kind)
    else {
        return Err(child);
    };
    parent
        .children
        .get_or_insert_with(Vec::new)
        .push(document_symbol(child));
    Ok(())
}

fn field_owner_name(signature: &str) -> Option<&str> {
    let rest = signature
        .strip_prefix("pub field ")
        .or_else(|| signature.strip_prefix("field "))?;
    let (path, _) = rest.split_once(':')?;
    let (owner, _) = path.rsplit_once('.')?;
    Some(owner)
}

fn variant_owner_name(signature: &str) -> Option<&str> {
    let rest = signature.strip_prefix("variant ")?;
    let path = rest.split('(').next().unwrap_or(rest);
    let (owner, _) = path.rsplit_once('.')?;
    Some(owner)
}

fn interface_method_owner_name(signature: &str) -> Option<&str> {
    let rest = signature.strip_prefix("fn ")?;
    let path = rest.split('(').next().unwrap_or(rest);
    let (owner, _) = path.rsplit_once('.')?;
    Some(owner)
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
        if let Some(location) = module_definition_for_document(text, position, &project) {
            return Some(GotoDefinitionResponse::Scalar(location));
        }
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

fn module_definition_for_document(
    text: &str,
    position: Position,
    project: &nomo::project::Project,
) -> Option<Location> {
    let import = import_path_at_position(text, position)?;
    let local_root = local_import_root(text)?;
    let context = nomo::project::project_module_context(project).ok()?;
    let source_path = nomo::project::resolve_module_source_path(&context, &local_root, &import)?;
    let uri = Url::from_file_path(&source_path).ok()?;
    let range = module_definition_range(&source_path);
    Some(Location { uri, range })
}

fn import_path_at_position(text: &str, position: Position) -> Option<Vec<String>> {
    let line = text.lines().nth(position.line as usize)?;
    let trimmed_start = line.len() - line.trim_start().len();
    let rest = line[trimmed_start..].strip_prefix("import ")?;
    let path_start = trimmed_start + "import ".len();
    let path_end = path_start
        + rest
            .find(|ch: char| ch.is_ascii_whitespace())
            .unwrap_or(rest.len());
    let character = utf16_character_to_byte_index(line, position.character);
    if character < path_start || character > path_end {
        return None;
    }
    let import = &line[path_start..path_end];
    let parts = import
        .split('.')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
        .collect::<Vec<_>>();
    (parts.len() >= 2).then_some(parts)
}

fn module_definition_range(path: &Path) -> Range {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let first_line_len = text
        .lines()
        .next()
        .map(|line| line.encode_utf16().count() as u32)
        .unwrap_or(0);
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: 0,
            character: first_line_len,
        },
    }
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

fn prepare_rename_for_document(
    path: &Path,
    text: &str,
    uri: Url,
    position: Position,
    source_overrides: &[(PathBuf, String)],
) -> Option<PrepareRenameResponse> {
    let locations = references_for_document(path, text, uri, position, true, source_overrides)?;
    if locations.is_empty() {
        return None;
    }
    let range = identifier_range_at_position(text, position)?;
    Some(PrepareRenameResponse::Range(range))
}

fn identifier_range_at_position(text: &str, position: Position) -> Option<Range> {
    let line = text.lines().nth(position.line as usize)?;
    let byte_index = utf16_character_to_byte_index(line, position.character);
    let bytes = line.as_bytes();
    if byte_index > bytes.len() {
        return None;
    }

    let mut start = byte_index;
    if start == bytes.len() && start > 0 {
        start -= 1;
    }
    if !is_ident_byte(bytes.get(start).copied()?) && start > 0 {
        start -= 1;
    }
    if !is_ident_byte(bytes.get(start).copied()?) {
        return None;
    }

    let mut end = start;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    while end + 1 < bytes.len() && is_ident_byte(bytes[end + 1]) {
        end += 1;
    }
    let start_character = line[..start].encode_utf16().count() as u32;
    let end_character = line[..=end].encode_utf16().count() as u32;
    Some(Range {
        start: Position {
            line: position.line,
            character: start_character,
        },
        end: Position {
            line: position.line,
            character: end_character,
        },
    })
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
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
        SemanticSymbolKind::Field => "field",
        SemanticSymbolKind::Variant => "enum variant",
        SemanticSymbolKind::Interface => "interface",
        SemanticSymbolKind::InterfaceMethod => "interface method",
        SemanticSymbolKind::Const => "const",
        SemanticSymbolKind::Function => "function",
        SemanticSymbolKind::ExternFunction => "extern function",
        SemanticSymbolKind::Method => "method",
    }
}

fn lsp_symbol_kind(kind: SemanticSymbolKind) -> SymbolKind {
    match kind {
        SemanticSymbolKind::Struct => SymbolKind::STRUCT,
        SemanticSymbolKind::Enum => SymbolKind::ENUM,
        SemanticSymbolKind::Field => SymbolKind::FIELD,
        SemanticSymbolKind::Variant => SymbolKind::ENUM_MEMBER,
        SemanticSymbolKind::Interface => SymbolKind::INTERFACE,
        SemanticSymbolKind::InterfaceMethod => SymbolKind::METHOD,
        SemanticSymbolKind::Const => SymbolKind::CONSTANT,
        SemanticSymbolKind::Function => SymbolKind::FUNCTION,
        SemanticSymbolKind::ExternFunction => SymbolKind::FUNCTION,
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

fn inlay_hints_for_text(path: &Path, text: &str, range: Range) -> Vec<InlayHint> {
    let Ok(tokens) = nomo::lex(path, text) else {
        return Vec::new();
    };
    let Ok(ast) = nomo::parser::parse(path, &tokens) else {
        return Vec::new();
    };

    let mut hints = Vec::new();
    collect_inlay_hints_from_file(&ast, &range, &mut hints);
    collect_parameter_inlay_hints(&tokens, &ast, &range, &mut hints);
    hints.sort_by(|left, right| compare_positions(left.position, right.position));
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
                    && position_in_range(position, range)
                {
                    hints.push(type_inlay_hint(position, label));
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
            Stmt::Unsafe { body, .. } => {
                collect_inlay_hints_from_stmts(body, range, hints);
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

fn parameter_inlay_hint(position: Position, label: &str) -> InlayHint {
    InlayHint {
        position,
        label: InlayHintLabel::String(format!("{label}:")),
        kind: Some(InlayHintKind::PARAMETER),
        text_edits: None,
        tooltip: None,
        padding_left: None,
        padding_right: Some(true),
        data: None,
    }
}

fn collect_parameter_inlay_hints(
    tokens: &[Token],
    file: &SourceFile,
    range: &Range,
    hints: &mut Vec<InlayHint>,
) {
    let params = parameter_hint_signatures(file);
    let mut index = 0;
    while index < tokens.len() {
        let Some((callee, lparen_index, next_index)) = call_callee_at(tokens, index) else {
            index += 1;
            continue;
        };
        index = next_index;
        let Some(param_names) = params.get(&callee) else {
            continue;
        };
        let Some(args) = call_argument_start_indices(tokens, lparen_index) else {
            continue;
        };
        for (arg_index, param_name) in args.iter().zip(param_names.iter()) {
            if argument_matches_parameter(tokens, *arg_index, param_name) {
                continue;
            }
            let position = token_position(&tokens[*arg_index]);
            if position_in_range(position, range) {
                hints.push(parameter_inlay_hint(position, param_name));
            }
        }
    }
}

fn parameter_hint_signatures(file: &SourceFile) -> HashMap<String, Vec<String>> {
    let mut signatures = HashMap::new();
    for function in &file.functions {
        signatures.insert(
            function.name.clone(),
            function
                .params
                .iter()
                .map(|param| param.name.clone())
                .collect(),
        );
    }
    for extern_block in &file.extern_blocks {
        for function in &extern_block.functions {
            signatures.insert(
                function.name.clone(),
                function
                    .params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect(),
            );
        }
    }
    for interface in &file.interfaces {
        for method in &interface.methods {
            let params = method
                .params
                .iter()
                .filter(|param| param.name != "self")
                .map(|param| param.name.clone())
                .collect::<Vec<_>>();
            signatures.entry(method.name.clone()).or_insert(params);
        }
    }
    for impl_block in &file.impls {
        for method in &impl_block.methods {
            let params = method
                .params
                .iter()
                .filter(|param| param.name != "self")
                .map(|param| param.name.clone())
                .collect::<Vec<_>>();
            signatures.entry(method.name.clone()).or_insert(params);
        }
    }
    signatures.retain(|_, params| !params.is_empty());
    signatures
}

fn call_callee_at(tokens: &[Token], index: usize) -> Option<(String, usize, usize)> {
    if matches!(
        previous_significant_token(tokens, index).map(|token| &token.kind),
        Some(TokenKind::Fn | TokenKind::Struct | TokenKind::Enum | TokenKind::Interface)
    ) {
        return None;
    }
    let mut cursor = index;
    let mut last_ident = match &tokens.get(cursor)?.kind {
        TokenKind::Ident(name) => name.clone(),
        _ => return None,
    };
    cursor += 1;
    while matches!(
        tokens.get(cursor).map(|token| &token.kind),
        Some(TokenKind::Dot)
    ) {
        let Some(Token {
            kind: TokenKind::Ident(name),
            ..
        }) = tokens.get(cursor + 1)
        else {
            return None;
        };
        last_ident = name.clone();
        cursor += 2;
    }
    if !matches!(
        tokens.get(cursor).map(|token| &token.kind),
        Some(TokenKind::LParen)
    ) {
        return None;
    }
    Some((last_ident, cursor, cursor + 1))
}

fn previous_significant_token(tokens: &[Token], index: usize) -> Option<&Token> {
    tokens[..index]
        .iter()
        .rev()
        .find(|token| !matches!(token.kind, TokenKind::Newline))
}

fn call_argument_start_indices(tokens: &[Token], lparen_index: usize) -> Option<Vec<usize>> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut expect_arg = true;
    let mut index = lparen_index + 1;
    while let Some(token) = tokens.get(index) {
        match &token.kind {
            TokenKind::RParen if depth == 0 => return Some(args),
            TokenKind::Comma if depth == 0 => {
                expect_arg = true;
                index += 1;
                continue;
            }
            TokenKind::Newline if expect_arg => {
                index += 1;
                continue;
            }
            _ if expect_arg => {
                args.push(index);
                expect_arg = false;
            }
            _ => {}
        }
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace if depth > 0 => depth -= 1,
            _ => {}
        }
        index += 1;
    }
    None
}

fn argument_matches_parameter(tokens: &[Token], arg_index: usize, param_name: &str) -> bool {
    matches!(
        tokens.get(arg_index).map(|token| &token.kind),
        Some(TokenKind::Ident(name)) if name == param_name
    )
}

fn token_position(token: &Token) -> Position {
    let byte_index = token.column.saturating_sub(1);
    let character = token
        .text
        .get(..byte_index)
        .map(|prefix| prefix.encode_utf16().count())
        .unwrap_or(byte_index);
    Position {
        line: token.line.saturating_sub(1) as u32,
        character: character as u32,
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
        compiler_semantic::identifier_at_position(
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
            let has_symbol = compiler_semantic::symbols_for_text(&path, &source)
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
    fn workspace_symbols_include_dependency_public_symbols() {
        let root = temp_test_root("workspace-symbol-dependency");
        reset_dir(&root);
        let project = root.join("hello");
        let dependency = root.join("utils");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlocal_utils = { package = \"fynn/utils\", path = \"../utils\" }\n",
        )
        .unwrap();
        fs::write(
            project.join("src/main.nomo"),
            "package app.main\n\nfn main() -> void {\n}\n",
        )
        .unwrap();
        fs::write(
            dependency.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"utils\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let dep_module = dependency.join("src/path.nomo");
        fs::write(
            &dep_module,
            "package local_utils.path\n\npub fn join(a: string, b: string) -> string {\n    return a\n}\n\nfn hidden_join() -> string {\n    return \"hidden\"\n}\n",
        )
        .unwrap();

        let symbols = workspace_symbols_for_roots(std::slice::from_ref(&project), "join", &[]);

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["join"]
        );
        assert_eq!(
            symbols[0].location.uri,
            Url::from_file_path(fs::canonicalize(&dep_module).unwrap()).unwrap()
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
    fn hover_returns_extern_function_signature_and_doc_comment() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nextern \"C\" {\n    /// Writes a C string.\n    fn puts(message: string) -> i32\n}\n\nfn main() -> void {\n    unsafe {\n        puts(\"hello\")\n    }\n}\n";

        let hover = hover_for_text(
            &path,
            text,
            Position {
                line: 9,
                character: 10,
            },
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(
            markup
                .value
                .contains("extern \"C\" fn puts(message: string) -> i32")
        );
        assert!(markup.value.contains("Writes a C string."));
        assert!(markup.value.contains("extern function"));
    }

    #[test]
    fn hover_returns_interface_method_signature_and_doc_comment() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\n/// Display contract.\npub interface Display {\n    /// Converts to text.\n    fn to_string(self) -> string\n}\n";

        let hover = hover_for_text(
            &path,
            text,
            Position {
                line: 5,
                character: 10,
            },
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(
            markup
                .value
                .contains("fn Display.to_string(self: Self) -> string")
        );
        assert!(markup.value.contains("Converts to text."));
        assert!(markup.value.contains("interface method"));
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
    fn document_symbols_nest_fields_and_variants_under_parent_types() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\npub struct User {\n    email: string\n}\n\nenum Status {\n    Ready\n    Done(string)\n}\n\npub interface Display {\n    fn to_string(self) -> string\n}\n\nconst MAX: i64 = 10\n\nextern \"C\" {\n    fn puts(message: string) -> i32\n}\n\nimpl User {\n    pub fn email(self) -> string {\n        return self.email\n    }\n}\n\nfn main() -> void {\n}\n";

        let Some(DocumentSymbolResponse::Nested(symbols)) = document_symbols_for_text(&path, text)
        else {
            panic!("expected document symbols");
        };

        let names = symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["User", "Status", "Display", "MAX", "main", "puts", "email"]
        );
        assert_eq!(symbols[0].kind, SymbolKind::STRUCT);
        assert_eq!(symbols[1].kind, SymbolKind::ENUM);
        assert_eq!(symbols[2].kind, SymbolKind::INTERFACE);
        assert_eq!(symbols[3].kind, SymbolKind::CONSTANT);
        assert_eq!(symbols[4].kind, SymbolKind::FUNCTION);
        assert_eq!(symbols[5].kind, SymbolKind::FUNCTION);
        assert_eq!(symbols[6].kind, SymbolKind::METHOD);
        let user_children = symbols[0].children.as_ref().expect("struct children");
        assert_eq!(user_children.len(), 1);
        assert_eq!(user_children[0].name, "email");
        assert_eq!(user_children[0].kind, SymbolKind::FIELD);
        let status_children = symbols[1].children.as_ref().expect("enum children");
        assert_eq!(
            status_children
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Ready", "Done"]
        );
        assert_eq!(status_children[0].kind, SymbolKind::ENUM_MEMBER);
        let display_children = symbols[2].children.as_ref().expect("interface children");
        assert_eq!(display_children.len(), 1);
        assert_eq!(display_children[0].name, "to_string");
        assert_eq!(display_children[0].kind, SymbolKind::METHOD);
        assert_eq!(
            display_children[0].detail.as_deref(),
            Some("fn Display.to_string(self: Self) -> string")
        );
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
            symbols[5].detail.as_deref(),
            Some("extern \"C\" fn puts(message: string) -> i32")
        );
        assert_eq!(
            symbols[6].detail.as_deref(),
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
    fn definition_returns_field_declaration_location() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\npub struct User {\n    email: string\n}\n\nfn main() -> void {\n    let user: User = User { email: \"hi\" }\n}\n";

        let definition = definition_for_text(
            &path,
            text,
            uri,
            Position {
                line: 7,
                character: 30,
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
                    line: 3,
                    character: 4,
                },
                end: Position {
                    line: 3,
                    character: 9,
                },
            }
        );
    }

    #[test]
    fn definition_returns_enum_variant_declaration_location() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nenum Status {\n    Ok\n    Err(string)\n}\n\nfn main() -> void {\n    let status: Status = Status.Err(\"bad\")\n}\n";

        let definition = definition_for_text(
            &path,
            text,
            uri,
            Position {
                line: 8,
                character: 33,
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
                    line: 4,
                    character: 4,
                },
                end: Position {
                    line: 4,
                    character: 7,
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
    fn prepare_rename_returns_current_identifier_range() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";

        let prepared = prepare_rename_for_document(
            &path,
            text,
            uri,
            Position {
                line: 7,
                character: 22,
            },
            &[],
        )
        .unwrap();

        assert_eq!(
            prepared,
            PrepareRenameResponse::Range(Range {
                start: Position {
                    line: 7,
                    character: 21,
                },
                end: Position {
                    line: 7,
                    character: 24,
                },
            })
        );
    }

    #[test]
    fn prepare_rename_uses_project_symbols() {
        let root = temp_test_root("prepare-rename-project");
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

        let prepared = prepare_rename_for_document(
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

        assert_eq!(
            prepared,
            PrepareRenameResponse::Range(Range {
                start: Position {
                    line: 5,
                    character: 21,
                },
                end: Position {
                    line: 5,
                    character: 24,
                },
            })
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn prepare_rename_returns_none_for_unknown_identifier() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n}\n";

        let prepared = prepare_rename_for_document(
            &path,
            text,
            uri,
            Position {
                line: 3,
                character: 8,
            },
            &[],
        );

        assert!(prepared.is_none());
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
    fn definition_returns_local_module_file_for_import_path() {
        let root = temp_test_root("module-definition-local");
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
            "package app.math\n\npub fn add() -> i64 {\n    return 1\n}\n",
        )
        .unwrap();

        let definition = definition_for_document(
            &main,
            main_source,
            Url::from_file_path(&main).unwrap(),
            Position {
                line: 2,
                character: 12,
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
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 16,
                },
            }
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn definition_returns_dependency_module_file_for_import_path() {
        let root = temp_test_root("module-definition-dependency");
        reset_dir(&root);
        let project = root.join("hello");
        let dependency = root.join("local-utils");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(dependency.join("src/path")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlocal_utils = { package = \"local/utils\", path = \"../local-utils\" }\n",
        )
        .unwrap();
        fs::write(
            dependency.join("nomo.toml"),
            "[package]\nnamespace = \"local\"\nname = \"utils\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let dep_module = dependency.join("src/path/main.nomo");
        let main_source = "package app.main\n\nimport local_utils.path\n\nfn main() -> void {\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &dep_module,
            "package local_utils.path\n\npub fn join() -> i64 {\n    return 1\n}\n",
        )
        .unwrap();

        let definition = definition_for_document(
            &main,
            main_source,
            Url::from_file_path(&main).unwrap(),
            Position {
                line: 2,
                character: 20,
            },
            &[],
        )
        .unwrap();

        let GotoDefinitionResponse::Scalar(location) = definition else {
            panic!("expected scalar definition location");
        };
        assert_eq!(
            location.uri,
            Url::from_file_path(fs::canonicalize(&dep_module).unwrap()).unwrap()
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn definition_returns_dependency_symbol_location() {
        let root = temp_test_root("symbol-definition-dependency");
        reset_dir(&root);
        let project = root.join("hello");
        let dependency = root.join("local-utils");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlocal_utils = { package = \"local/utils\", path = \"../local-utils\" }\n",
        )
        .unwrap();
        fs::write(
            dependency.join("nomo.toml"),
            "[package]\nnamespace = \"local\"\nname = \"utils\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let dep_module = dependency.join("src/path.nomo");
        let main_source = "package app.main\n\nimport local_utils.path\n\nfn main() -> void {\n    let total: i64 = join(1, 2)\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &dep_module,
            "package local_utils.path\n\n/// Joins values.\npub fn join(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
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
        assert_eq!(
            location.uri,
            Url::from_file_path(fs::canonicalize(&dep_module).unwrap()).unwrap()
        );
        assert_eq!(
            location.range,
            Range {
                start: Position {
                    line: 3,
                    character: 7,
                },
                end: Position {
                    line: 3,
                    character: 11,
                },
            }
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hover_uses_dependency_symbol_docs() {
        let root = temp_test_root("semantic-hover-dependency");
        reset_dir(&root);
        let project = root.join("hello");
        let dependency = root.join("local-utils");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlocal_utils = { package = \"local/utils\", path = \"../local-utils\" }\n",
        )
        .unwrap();
        fs::write(
            dependency.join("nomo.toml"),
            "[package]\nnamespace = \"local\"\nname = \"utils\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let dep_module = dependency.join("src/path.nomo");
        let main_source = "package app.main\n\nimport local_utils.path\n\nfn main() -> void {\n    let total: i64 = join(1, 2)\n}\n";
        fs::write(&main, main_source).unwrap();
        fs::write(
            &dep_module,
            "package local_utils.path\n\n/// Joins values.\npub fn join(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
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
        assert!(markup.value.contains("pub fn join(a: i64, b: i64) -> i64"));
        assert!(markup.value.contains("Joins values."));
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
    fn inlay_hints_include_same_file_function_parameter_names() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nfn add(left: i64, right: i64) -> i64 {\n    return left + right\n}\n\nfn main() -> void {\n    let total = add(1, 2)\n    let copied = add(left, right)\n}\n";

        let hints = inlay_hints_for_text(&path, text, full_document_range(text));

        let labels = hints
            .iter()
            .map(|hint| match &hint.label {
                InlayHintLabel::String(label) => label.as_str(),
                InlayHintLabel::LabelParts(_) => "",
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["left:", "right:"]);
        assert_eq!(hints[0].kind, Some(InlayHintKind::PARAMETER));
        assert_eq!(
            hints[0].position,
            Position {
                line: 7,
                character: 20,
            }
        );
        assert_eq!(
            hints[1].position,
            Position {
                line: 7,
                character: 23,
            }
        );
    }

    #[test]
    fn inlay_hints_include_same_file_extern_function_parameter_names() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nextern \"C\" {\n    fn puts(message: string) -> i32\n}\n\nfn main() -> void {\n    let status = puts(\"hi\")\n}\n";

        let hints = inlay_hints_for_text(&path, text, full_document_range(text));

        let labels = hints
            .iter()
            .map(|hint| match &hint.label {
                InlayHintLabel::String(label) => label.as_str(),
                InlayHintLabel::LabelParts(_) => "",
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["message:"]);
        assert_eq!(hints[0].kind, Some(InlayHintKind::PARAMETER));
    }

    #[test]
    fn inlay_hints_include_same_file_method_parameter_names() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nstruct Counter {\n    value: i64\n}\n\nimpl Counter {\n    fn add(self, delta: i64) -> i64 {\n        return self.value + delta\n    }\n}\n\ninterface Writer {\n    fn write(self, message: string) -> void\n}\n\nfn main() -> void {\n    let counter = Counter { value: 1 }\n    let value = counter.add(2)\n    writer.write(\"hi\")\n}\n";

        let hints = inlay_hints_for_text(&path, text, full_document_range(text));

        let labels = hints
            .iter()
            .filter_map(|hint| match (&hint.kind, &hint.label) {
                (Some(InlayHintKind::PARAMETER), InlayHintLabel::String(label)) => {
                    Some(label.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["delta:", "message:"]);
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
