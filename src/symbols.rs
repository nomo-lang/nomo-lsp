use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nomo::semantic as compiler_semantic;
use nomo_lsp_bridge::{SemanticSymbol, SemanticSymbolKind, TextPosition, TextRange};
use tower_lsp::lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, Location, Position, Range, SymbolInformation,
    SymbolKind, Url,
};

pub(crate) fn workspace_symbols_for_roots(
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

    if let Ok(standard_symbols) = compiler_semantic::standard_library_symbols() {
        for symbol in standard_symbols {
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

#[allow(deprecated)]
pub(crate) fn document_symbols_for_text(path: &Path, text: &str) -> Option<DocumentSymbolResponse> {
    let symbols = nomo_lsp_bridge::symbols_for_text(path, text).ok()?;
    let mut items = Vec::<DocumentSymbol>::new();
    for symbol in symbols {
        match symbol.kind {
            SemanticSymbolKind::Field => {
                let Some(owner) = symbol.container_name.clone() else {
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
                let Some(owner) = symbol.container_name.clone() else {
                    items.push(document_symbol(symbol));
                    continue;
                };
                if let Err(symbol) = push_child_symbol(&mut items, &owner, SymbolKind::ENUM, symbol)
                {
                    items.push(document_symbol(symbol));
                }
            }
            SemanticSymbolKind::InterfaceMethod => {
                let Some(owner) = symbol.container_name.clone() else {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tower_lsp::lsp_types::{DocumentSymbolResponse, Position, Range, SymbolKind, Url};

    use super::{document_symbols_for_text, workspace_symbols_for_roots};

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

        assert_eq!(symbols.len(), 2);
        assert!(symbols.iter().all(|symbol| symbol.name == "join"));
        assert_eq!(
            symbols
                .iter()
                .find(|symbol| symbol.location.uri.to_file_path().unwrap()
                    == fs::canonicalize(&dep_module).unwrap())
                .unwrap()
                .location
                .uri,
            Url::from_file_path(fs::canonicalize(&dep_module).unwrap()).unwrap()
        );
        assert!(symbols.iter().any(|symbol| {
            symbol
                .location
                .uri
                .to_file_path()
                .is_ok_and(|path| path.ends_with("std/src/path.nomo"))
        }));
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

    fn reset_dir(path: &Path) {
        if path.exists() {
            fs::remove_dir_all(path).unwrap();
        }
        fs::create_dir_all(path).unwrap();
    }

    fn temp_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nomo-lsp-symbols-test-{name}-{}",
            std::process::id()
        ))
    }
}
