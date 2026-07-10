use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nomo::semantic as compiler_semantic;
use nomo_lsp_bridge::{SemanticLocation, TextPosition, TextRange};
use tower_lsp::lsp_types::{
    GotoDefinitionResponse, Location, Position, PrepareRenameResponse, Range, TextEdit, Url,
    WorkspaceEdit,
};

use crate::rename::rename_preserves_compilation;

const RESERVED_KEYWORDS: &[&str] = &[
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

pub(crate) fn definition_for_document(
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

pub(crate) fn references_for_document(
    path: &Path,
    text: &str,
    uri: Url,
    position: Position,
    include_declaration: bool,
    source_overrides: &[(PathBuf, String)],
) -> Option<Vec<Location>> {
    let compiler_position = to_compiler_position(position);
    if let Ok(project) = nomo::project::discover_project(path) {
        let locations = if let Ok(workspace) = nomo::project::discover_workspace(path)
            && workspace
                .members
                .iter()
                .any(|member| member.root == project.root)
        {
            compiler_semantic::references_for_workspace_text(
                &workspace,
                &project,
                path,
                text,
                compiler_position,
                include_declaration,
                source_overrides,
            )
        } else {
            compiler_semantic::references_for_project_text(
                &project,
                path,
                text,
                compiler_position,
                include_declaration,
                source_overrides,
            )
        }
        .ok()??;
        return locations.into_iter().map(to_lsp_location).collect();
    }
    references_for_text(path, text, uri, position, include_declaration)
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

pub(crate) fn rename_for_document(
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
    let current_uri = uri.clone();
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

    if !rename_preserves_compilation(path, text, &current_uri, &changes, source_overrides) {
        return None;
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

pub(crate) fn prepare_rename_for_document(
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
    !RESERVED_KEYWORDS.contains(&name)
}

fn to_lsp_location(location: SemanticLocation) -> Option<Location> {
    Some(Location {
        uri: Url::from_file_path(location.path).ok()?,
        range: to_lsp_range(location.range),
    })
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tower_lsp::lsp_types::{
        GotoDefinitionResponse, Position, PrepareRenameResponse, Range, Url,
    };

    use super::{
        definition_for_document, definition_for_text, prepare_rename_for_document,
        references_for_document, references_for_text, rename_for_document,
    };

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
    fn definition_uses_receiver_type_for_same_name_fields_without_manifest() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nstruct User {\n    name: string\n}\n\nstruct Team {\n    name: string\n}\n\nfn read(user: User) -> string {\n    return user.name\n}\n";

        let definition = definition_for_text(
            &path,
            text,
            uri,
            Position {
                line: 11,
                character: 17,
            },
        )
        .unwrap();

        let GotoDefinitionResponse::Scalar(location) = definition else {
            panic!("expected scalar definition location");
        };
        assert_eq!(location.range.start.line, 3);
        assert_eq!(location.range.start.character, 4);
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
    fn definition_returns_local_binding_declaration() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n    io.println(message)\n}\n";

        let definition = definition_for_text(
            &path,
            text,
            uri,
            Position {
                line: 4,
                character: 16,
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
                    character: 8,
                },
                end: Position {
                    line: 3,
                    character: 15,
                },
            }
        );
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
    fn references_return_local_binding_locations() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n    io.println(message)\n}\n";

        let references = references_for_text(
            &path,
            text,
            uri,
            Position {
                line: 4,
                character: 16,
            },
            true,
        )
        .unwrap();

        assert_eq!(references.len(), 2);
        assert_eq!(references[0].range.start.line, 3);
        assert_eq!(references[1].range.start.line, 4);
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
    fn rename_excludes_shadowing_parameter_references() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn value() -> i64 {\n    return 1\n}\n\nfn consume(value: i64) -> i64 {\n    return value\n}\n\nfn main() -> void {\n    let result: i64 = value()\n}\n";

        let edit = rename_for_document(
            &path,
            text,
            uri.clone(),
            Position {
                line: 11,
                character: 24,
            },
            "answer",
            &[],
        )
        .unwrap();

        let edits = edit.changes.unwrap().remove(&uri).unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].range.start.line, 2);
        assert_eq!(edits[1].range.start.line, 11);
        assert!(edits.iter().all(|edit| edit.new_text == "answer"));
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
    fn rename_rejects_top_level_declaration_collision() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn first() -> i64 {\n    return 1\n}\n\nfn second() -> i64 {\n    return 2\n}\n\nfn main() -> void {\n    let value: i64 = first()\n}\n";

        let edit = rename_for_document(
            &path,
            text,
            uri,
            Position {
                line: 11,
                character: 24,
            },
            "second",
            &[],
        );

        assert!(edit.is_none());
    }

    #[test]
    fn rename_rejects_local_binding_collision() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let first: i64 = 1\n    let second: i64 = first\n}\n";

        let edit = rename_for_document(
            &path,
            text,
            uri,
            Position {
                line: 4,
                character: 25,
            },
            "second",
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
    fn prepare_rename_accepts_local_binding() {
        let path = PathBuf::from("main.nomo");
        let uri = Url::parse("file:///tmp/main.nomo").unwrap();
        let text = "package app.main\n\nfn main() -> void {\n    let message: string = \"hi\"\n    io.println(message)\n}\n";

        let prepared = prepare_rename_for_document(
            &path,
            text,
            uri,
            Position {
                line: 4,
                character: 16,
            },
            &[],
        )
        .unwrap();

        assert_eq!(
            prepared,
            PrepareRenameResponse::Range(Range {
                start: Position {
                    line: 4,
                    character: 15,
                },
                end: Position {
                    line: 4,
                    character: 22,
                },
            })
        );
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
    fn workspace_references_include_dependent_member_calls() {
        let root = temp_test_root("workspace-references");
        let (core_main, cli_main, core_source) = setup_workspace(
            &root,
            "package cli.main\n\nimport core.main\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n",
        );

        let references = references_for_document(
            &core_main,
            core_source,
            Url::from_file_path(&core_main).unwrap(),
            Position {
                line: 2,
                character: 8,
            },
            true,
            &[],
        )
        .unwrap();

        assert!(references.iter().any(|location| {
            location.uri == Url::from_file_path(&core_main).unwrap()
                && location.range.start
                    == Position {
                        line: 2,
                        character: 7,
                    }
        }));
        assert!(references.iter().any(|location| {
            location.uri == Url::from_file_path(&cli_main).unwrap()
                && location.range.start
                    == Position {
                        line: 5,
                        character: 21,
                    }
        }));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_rename_updates_declaration_and_dependent_member() {
        let root = temp_test_root("workspace-rename");
        let (core_main, cli_main, core_source) = setup_workspace(
            &root,
            "package cli.main\n\nimport core.main\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n",
        );
        let core_uri = Url::from_file_path(&core_main).unwrap();
        let cli_uri = Url::from_file_path(&cli_main).unwrap();

        let edit = rename_for_document(
            &core_main,
            core_source,
            core_uri.clone(),
            Position {
                line: 2,
                character: 8,
            },
            "sum",
            &[],
        )
        .unwrap();

        let changes = edit.changes.unwrap();
        assert_eq!(changes.get(&core_uri).unwrap().len(), 1);
        assert_eq!(changes.get(&cli_uri).unwrap().len(), 1);
        assert!(
            changes
                .values()
                .flatten()
                .all(|edit| edit.new_text == "sum")
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_rename_rejects_collision_in_dependent_member() {
        let root = temp_test_root("workspace-rename-dependent-collision");
        let (core_main, _, core_source) = setup_workspace(
            &root,
            "package cli.main\n\nimport core.main\n\nfn sum(a: i64, b: i64) -> i64 {\n    return a - b\n}\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n",
        );

        let edit = rename_for_document(
            &core_main,
            core_source,
            Url::from_file_path(&core_main).unwrap(),
            Position {
                line: 2,
                character: 8,
            },
            "sum",
            &[],
        );

        assert!(edit.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_references_keep_external_path_dependencies_read_only() {
        let root = temp_test_root("workspace-external-references");
        reset_dir(&root);
        let app = root.join("app");
        let external = root.join("external");
        fs::create_dir_all(app.join("src")).unwrap();
        fs::create_dir_all(external.join("src")).unwrap();
        fs::write(root.join("nomo.toml"), "[workspace]\nmembers = [\"app\"]\n").unwrap();
        fs::write(
            app.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nexternal = { package = \"other/external\", path = \"../external\" }\n",
        )
        .unwrap();
        fs::write(
            external.join("nomo.toml"),
            "[package]\nnamespace = \"other\"\nname = \"external\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let app_main = app.join("src/main.nomo");
        let external_main = external.join("src/main.nomo");
        let app_source = "package app.main\n\nimport external.main\n\nfn main() -> void {\n    let total: i64 = add(1, 2)\n}\n";
        fs::write(&app_main, app_source).unwrap();
        fs::write(
            &external_main,
            "package external.main\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n",
        )
        .unwrap();

        let references = references_for_document(
            &app_main,
            app_source,
            Url::from_file_path(&app_main).unwrap(),
            Position {
                line: 5,
                character: 23,
            },
            true,
            &[],
        );

        assert!(references.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rename_updates_only_members_with_the_selected_receiver_type() {
        let root = temp_test_root("receiver-type-rename");
        reset_dir(&root);
        let project = root.join("hello");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        let main = project.join("src/main.nomo");
        let source = "package app.main\n\nstruct User {\n    name: string\n}\n\nstruct Team {\n    name: string\n}\n\nimpl User {\n    fn label(self) -> string {\n        return self.name\n    }\n}\n\nimpl Team {\n    fn label(self) -> string {\n        return self.name\n    }\n}\n\nfn main() -> void {\n    let user = User { name: \"Ada\" }\n    let team = Team { name: \"Core\" }\n    let user_name: string = user.name\n    let team_name: string = team.name\n}\n";
        fs::write(&main, source).unwrap();
        let uri = Url::from_file_path(&main).unwrap();

        let edit = rename_for_document(
            &main,
            source,
            uri.clone(),
            Position {
                line: 25,
                character: 35,
            },
            "title",
            &[],
        )
        .unwrap();

        let edits = edit.changes.unwrap().remove(&uri).unwrap();
        assert_eq!(edits.len(), 4, "{edits:?}");
        assert_eq!(
            edits
                .iter()
                .map(|edit| edit.range.start.line)
                .collect::<Vec<_>>(),
            vec![3, 12, 23, 25]
        );
        assert!(edits.iter().all(|edit| edit.new_text == "title"));
        fs::remove_dir_all(&root).unwrap();
    }

    fn setup_workspace(root: &Path, cli_source: &str) -> (PathBuf, PathBuf, &'static str) {
        const CORE_SOURCE: &str = "package core.main\n\npub fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> void {\n}\n";
        reset_dir(root);
        let core = root.join("packages/core");
        let cli = root.join("apps/cli");
        fs::create_dir_all(core.join("src")).unwrap();
        fs::create_dir_all(cli.join("src")).unwrap();
        fs::write(
            root.join("nomo.toml"),
            "[workspace]\nmembers = [\"apps/*\", \"packages/*\"]\n",
        )
        .unwrap();
        fs::write(
            core.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"core\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        )
        .unwrap();
        fs::write(
            cli.join("nomo.toml"),
            "[package]\nnamespace = \"fynn\"\nname = \"cli\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\ncore = { package = \"fynn/core\", path = \"../../packages/core\" }\n",
        )
        .unwrap();
        let core_main = core.join("src/main.nomo");
        let cli_main = cli.join("src/main.nomo");
        fs::write(&core_main, CORE_SOURCE).unwrap();
        fs::write(&cli_main, cli_source).unwrap();
        (core_main, cli_main, CORE_SOURCE)
    }

    fn reset_dir(path: &Path) {
        if path.exists() {
            fs::remove_dir_all(path).unwrap();
        }
        fs::create_dir_all(path).unwrap();
    }

    fn temp_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nomo-lsp-navigation-test-{name}-{}",
            std::process::id()
        ))
    }
}
