use std::path::{Path, PathBuf};

use nomo::semantic as compiler_semantic;
use nomo_lsp_bridge::{SemanticSymbol, SemanticSymbolKind, TextPosition};
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

pub(crate) fn hover_for_document(
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

    Some(hover_for_symbol(&item))
}

#[cfg(test)]
fn hover_for_text(path: &Path, text: &str, position: Position) -> Option<Hover> {
    let item = compiler_semantic::symbol_at_position(path, text, to_compiler_position(position))
        .ok()??;

    Some(hover_for_symbol(&item))
}

fn hover_for_symbol(item: &SemanticSymbol) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: hover_markdown(item),
        }),
        range: None,
    }
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

fn to_compiler_position(position: Position) -> TextPosition {
    TextPosition {
        line: position.line,
        character: position.character,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tower_lsp::lsp_types::{HoverContents, Position};

    use super::{hover_for_document, hover_for_text};

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
    fn hover_returns_nested_block_doc_comment() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\n/**\n * Outer docs.\n * /* Nested docs. */\n * Still outer.\n */\npub fn nested() -> void {\n}\n\nfn main() -> void {\n    nested()\n}\n";

        let hover = hover_for_text(
            &path,
            text,
            Position {
                line: 11,
                character: 6,
            },
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("pub fn nested() -> void"));
        assert!(markup.value.contains("Outer docs."));
        assert!(markup.value.contains("/* Nested docs. */"));
        assert!(markup.value.contains("Still outer."));
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
    fn hover_uses_standard_library_source_docs() {
        let root = temp_test_root("semantic-hover-std");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let main_source = "package app.main\n\nimport std.string.split\n\nfn main() -> void {\n    let parts: Array<string> = split(\"a\", \",\")\n}\n";
        fs::write(&main, main_source).unwrap();

        let line = main_source.lines().nth(5).unwrap();
        let hover = hover_for_document(
            &main,
            main_source,
            Position {
                line: 5,
                character: line.find("split").unwrap() as u32 + 1,
            },
            &[],
        )
        .unwrap();

        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(
            markup
                .value
                .contains("pub fn split(value: string, separator: string)")
        );
        assert!(
            markup
                .value
                .contains("Splits a string by a non-empty separator.")
        );
        fs::remove_dir_all(&root).unwrap();
    }

    fn reset_dir(path: &Path) {
        if path.exists() {
            fs::remove_dir_all(path).unwrap();
        }
        fs::create_dir_all(path).unwrap();
    }

    fn temp_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nomo-lsp-hover-test-{name}-{}", std::process::id()))
    }
}
