use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dashmap::DashMap;
use nomo::Diagnostic as NomoDiagnostic;
use nomo::ast::{
    ConstDef, EnumDef, Function, ImplBlock, Param, SourceFile, Span, StructDef, TypeRef,
};
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
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
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

        Ok(hover_for_text(
            &path,
            &text,
            params.text_document_position_params.position,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HoverSymbol {
    name: String,
    kind: &'static str,
    signature: String,
    docs: String,
    line: usize,
    range: Range,
    selection_range: Range,
    symbol_kind: SymbolKind,
}

fn hover_for_text(path: &Path, text: &str, position: Position) -> Option<Hover> {
    let symbol = identifier_at_position(text, position)?;
    let symbols = hover_symbols(path, text).ok()?;
    let item = symbols
        .iter()
        .filter(|item| item.name == symbol)
        .min_by_key(|item| item.line)?;

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
    let symbols = hover_symbols(path, text).ok()?;
    let items = symbols
        .into_iter()
        .map(|item| DocumentSymbol {
            name: item.name,
            detail: Some(item.signature),
            kind: item.symbol_kind,
            tags: None,
            deprecated: None,
            range: item.range,
            selection_range: item.selection_range,
            children: None,
        })
        .collect::<Vec<_>>();

    Some(DocumentSymbolResponse::Nested(items))
}

fn hover_symbols(path: &Path, text: &str) -> std::result::Result<Vec<HoverSymbol>, NomoDiagnostic> {
    let tokens = nomo::lex(path, text)?;
    let ast = nomo::parser::parse(path, &tokens)?;
    let docs = extract_doc_comments(text);
    Ok(symbols_from_ast(&ast, &docs))
}

fn symbols_from_ast(ast: &SourceFile, docs: &DocComments) -> Vec<HoverSymbol> {
    let mut symbols = Vec::new();
    for item in &ast.structs {
        symbols.push(HoverSymbol {
            name: item.name.clone(),
            kind: "struct",
            signature: struct_signature(item),
            docs: docs
                .item_docs
                .get(&item.span.line)
                .cloned()
                .unwrap_or_default(),
            line: item.span.line,
            range: line_range(&item.span),
            selection_range: name_selection_range(&item.span, &item.name),
            symbol_kind: SymbolKind::STRUCT,
        });
    }
    for item in &ast.enums {
        symbols.push(HoverSymbol {
            name: item.name.clone(),
            kind: "enum",
            signature: enum_signature(item),
            docs: docs
                .item_docs
                .get(&item.span.line)
                .cloned()
                .unwrap_or_default(),
            line: item.span.line,
            range: line_range(&item.span),
            selection_range: name_selection_range(&item.span, &item.name),
            symbol_kind: SymbolKind::ENUM,
        });
    }
    for item in &ast.consts {
        symbols.push(HoverSymbol {
            name: item.name.clone(),
            kind: "const",
            signature: const_signature(item),
            docs: docs
                .item_docs
                .get(&item.span.line)
                .cloned()
                .unwrap_or_default(),
            line: item.span.line,
            range: line_range(&item.span),
            selection_range: name_selection_range(&item.span, &item.name),
            symbol_kind: SymbolKind::CONSTANT,
        });
    }
    for item in &ast.functions {
        symbols.push(HoverSymbol {
            name: item.name.clone(),
            kind: "function",
            signature: function_signature(item),
            docs: docs
                .item_docs
                .get(&item.span.line)
                .cloned()
                .unwrap_or_default(),
            line: item.span.line,
            range: line_range(&item.span),
            selection_range: name_selection_range(&item.span, &item.name),
            symbol_kind: SymbolKind::FUNCTION,
        });
    }
    for impl_block in &ast.impls {
        symbols.extend(method_symbols(impl_block, docs));
    }
    symbols
}

fn method_symbols(impl_block: &ImplBlock, docs: &DocComments) -> Vec<HoverSymbol> {
    let receiver = type_ref(&impl_block.type_name);
    impl_block
        .methods
        .iter()
        .map(|method| HoverSymbol {
            name: method.name.clone(),
            kind: "method",
            signature: method_signature(&receiver, method),
            docs: docs
                .item_docs
                .get(&method.span.line)
                .cloned()
                .unwrap_or_default(),
            line: method.span.line,
            range: line_range(&method.span),
            selection_range: name_selection_range(&method.span, &method.name),
            symbol_kind: SymbolKind::METHOD,
        })
        .collect()
}

fn line_range(span: &Span) -> Range {
    let line = span.line.saturating_sub(1) as u32;
    Range {
        start: Position { line, character: 0 },
        end: Position {
            line,
            character: span.text.chars().map(|ch| ch.len_utf16() as u32).sum(),
        },
    }
}

fn name_selection_range(span: &Span, name: &str) -> Range {
    let line = span.line.saturating_sub(1) as u32;
    let fallback_start = span.column.saturating_sub(1) as u32;
    let start = span
        .text
        .find(name)
        .map(|byte_index| span.text[..byte_index].encode_utf16().count() as u32)
        .unwrap_or(fallback_start);
    let end = start + name.encode_utf16().count() as u32;
    Range {
        start: Position {
            line,
            character: start,
        },
        end: Position {
            line,
            character: end,
        },
    }
}

fn hover_markdown(item: &HoverSymbol) -> String {
    let mut value = format!("```nomo\n{}\n```", item.signature);
    if !item.docs.is_empty() {
        value.push_str("\n\n");
        value.push_str(&item.docs);
    }
    value.push_str("\n\n");
    value.push_str(item.kind);
    value
}

fn identifier_at_position(text: &str, position: Position) -> Option<String> {
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
    Some(line[start..=end].to_string())
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

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn struct_signature(item: &StructDef) -> String {
    format!(
        "{}struct {}{}",
        visibility_prefix(item.public),
        item.name,
        type_params(&item.type_params)
    )
}

fn enum_signature(item: &EnumDef) -> String {
    format!(
        "{}enum {}{}",
        visibility_prefix(item.public),
        item.name,
        type_params(&item.type_params)
    )
}

fn const_signature(item: &ConstDef) -> String {
    format!(
        "{}const {}: {}",
        visibility_prefix(item.public),
        item.name,
        type_ref(&item.type_ref)
    )
}

fn function_signature(function: &Function) -> String {
    let params = function
        .params
        .iter()
        .map(param)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}fn {}{}({}) -> {}",
        visibility_prefix(function.public),
        function.name,
        type_params(&function.type_params),
        params,
        type_ref(&function.return_type)
    )
}

fn method_signature(receiver: &str, function: &Function) -> String {
    let params = function
        .params
        .iter()
        .map(param)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}fn {receiver}.{}{}({}) -> {}",
        visibility_prefix(function.public),
        function.name,
        type_params(&function.type_params),
        params,
        type_ref(&function.return_type)
    )
}

fn param(param: &Param) -> String {
    let mutable = if param.mutable { "mut " } else { "" };
    format!("{mutable}{}: {}", param.name, type_ref(&param.type_ref))
}

fn type_params(params: &[String]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        format!("<{}>", params.join(", "))
    }
}

fn type_ref(type_ref_value: &TypeRef) -> String {
    let base = type_ref_value.path.join(".");
    if type_ref_value.args.is_empty() {
        base
    } else {
        format!(
            "{base}<{}>",
            type_ref_value
                .args
                .iter()
                .map(type_ref)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn visibility_prefix(public: bool) -> &'static str {
    if public { "pub " } else { "" }
}

#[derive(Debug, Default)]
struct DocComments {
    item_docs: BTreeMap<usize, String>,
}

fn extract_doc_comments(source: &str) -> DocComments {
    let lines = source.lines().collect::<Vec<_>>();
    let mut comments = DocComments::default();
    let mut pending = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if let Some(text) = trimmed.strip_prefix("///") {
            pending.push(text.trim_start().to_string());
            index += 1;
            continue;
        }
        if trimmed.starts_with("/**") {
            let (doc, next_index) = collect_block_doc(&lines, index);
            pending.push(doc);
            index = next_index;
            continue;
        }
        if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with("/*") {
            if !pending.is_empty() {
                comments.item_docs.insert(index + 1, pending.join("\n"));
                pending.clear();
            }
        }
        index += 1;
    }
    comments
}

fn collect_block_doc(lines: &[&str], start: usize) -> (String, usize) {
    let mut raw = String::new();
    let mut index = start;
    while index < lines.len() {
        if !raw.is_empty() {
            raw.push('\n');
        }
        raw.push_str(lines[index]);
        if lines[index].contains("*/") {
            index += 1;
            break;
        }
        index += 1;
    }
    let raw = raw.trim().trim_start_matches("/**").trim_end_matches("*/");
    let doc = raw
        .lines()
        .map(|line| line.trim().trim_start_matches('*').trim_start())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    (doc, index)
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
