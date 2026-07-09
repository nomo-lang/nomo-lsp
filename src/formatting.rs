use std::path::Path;

use tower_lsp::lsp_types::{Position, Range, TextEdit};

pub(crate) fn formatting_edits_for_text(path: &Path, text: &str) -> Option<Vec<TextEdit>> {
    let formatted = nomo::format_source(path, text).ok()?;
    if formatted == text {
        return Some(Vec::new());
    }

    Some(vec![TextEdit {
        range: full_document_range(text),
        new_text: formatted,
    }])
}

pub(crate) fn range_formatting_edits_for_text(
    path: &Path,
    text: &str,
    range: Range,
) -> Option<Vec<TextEdit>> {
    let formatted = nomo::format_source(path, text).ok()?;
    if formatted == text {
        return Some(Vec::new());
    }
    let Some(edit) = minimal_formatting_edit(text, &formatted) else {
        return Some(Vec::new());
    };
    if ranges_overlap(&edit.range, &range) {
        Some(vec![edit])
    } else {
        Some(Vec::new())
    }
}

fn minimal_formatting_edit(original: &str, formatted: &str) -> Option<TextEdit> {
    if original == formatted {
        return None;
    }

    let mut prefix = 0usize;
    let max_prefix = original.len().min(formatted.len());
    while prefix < max_prefix {
        let original_char = original[prefix..].chars().next()?;
        let formatted_char = formatted[prefix..].chars().next()?;
        if original_char != formatted_char {
            break;
        }
        let len = original_char.len_utf8();
        prefix += len;
    }

    let mut original_suffix = original.len();
    let mut formatted_suffix = formatted.len();
    while original_suffix > prefix && formatted_suffix > prefix {
        let original_char = original[..original_suffix].chars().next_back()?;
        let formatted_char = formatted[..formatted_suffix].chars().next_back()?;
        if original_char != formatted_char {
            break;
        }
        original_suffix -= original_char.len_utf8();
        formatted_suffix -= formatted_char.len_utf8();
    }

    Some(TextEdit {
        range: byte_range_to_lsp_range(original, prefix, original_suffix),
        new_text: formatted[prefix..formatted_suffix].to_string(),
    })
}

fn byte_range_to_lsp_range(text: &str, start: usize, end: usize) -> Range {
    Range {
        start: byte_index_to_position(text, start),
        end: byte_index_to_position(text, end),
    }
}

fn byte_index_to_position(text: &str, target: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (byte_index, ch) in text.char_indices() {
        if byte_index >= target {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    Position { line, character }
}

fn ranges_overlap(left: &Range, right: &Range) -> bool {
    compare_positions(left.end, right.start) == std::cmp::Ordering::Greater
        && compare_positions(left.start, right.end) == std::cmp::Ordering::Less
}

fn compare_positions(left: Position, right: Position) -> std::cmp::Ordering {
    left.line
        .cmp(&right.line)
        .then(left.character.cmp(&right.character))
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tower_lsp::lsp_types::{Position, Range};

    use super::{formatting_edits_for_text, full_document_range, range_formatting_edits_for_text};

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
    fn range_formatting_formats_requested_region() {
        let path = PathBuf::from("main.nomo");
        let text = "package app.main\n\nfn main() -> void {\nlet message:string=\"hi\"\n}\n";

        let edits = range_formatting_edits_for_text(
            &path,
            text,
            Range {
                start: Position {
                    line: 2,
                    character: 0,
                },
                end: Position {
                    line: 5,
                    character: 0,
                },
            },
        )
        .unwrap();

        assert_eq!(edits.len(), 1);
        assert_ne!(edits[0].range, full_document_range(text));
        assert!(edits[0].new_text.contains("let message: string = "));
    }

    #[test]
    fn range_formatting_ignores_changes_outside_requested_region() {
        let path = PathBuf::from("main.nomo");
        let text =
            "package app . main\n\nfn main() -> void {\n    let message: string = \"hi\"\n}\n";

        let edits = range_formatting_edits_for_text(
            &path,
            text,
            Range {
                start: Position {
                    line: 3,
                    character: 0,
                },
                end: Position {
                    line: 4,
                    character: 0,
                },
            },
        )
        .unwrap();

        assert!(edits.is_empty());
    }
}
