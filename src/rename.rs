use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{Position, TextEdit, Url};

pub(crate) fn rename_preserves_compilation(
    path: &Path,
    text: &str,
    current_uri: &Url,
    changes: &HashMap<Url, Vec<TextEdit>>,
    source_overrides: &[(PathBuf, String)],
) -> bool {
    let Some(renamed_sources) =
        renamed_source_overrides(path, text, current_uri, changes, source_overrides)
    else {
        return false;
    };

    if let Ok(project) = nomo::project::discover_project(path) {
        let mut original_sources = source_overrides.to_vec();
        replace_source_override(&mut original_sources, path, text.to_string());
        if let Ok(workspace) = nomo::project::discover_workspace(path)
            && workspace
                .members
                .iter()
                .any(|member| member.root == project.root)
        {
            if nomo::project::check_workspace_with_overrides(&workspace, &original_sources).is_err()
            {
                return true;
            }
            return nomo::project::check_workspace_with_overrides(&workspace, &renamed_sources)
                .is_ok();
        }
        if nomo::project::check_project_with_overrides(&project, &original_sources).is_err() {
            return true;
        }
        return nomo::project::check_project_with_overrides(&project, &renamed_sources).is_ok();
    }

    if nomo::check_source_text(path, text).is_err() {
        return true;
    }
    renamed_sources
        .iter()
        .find(|(source_path, _)| source_path == path)
        .is_some_and(|(_, renamed)| nomo::check_source_text(path, renamed).is_ok())
}

fn renamed_source_overrides(
    path: &Path,
    text: &str,
    current_uri: &Url,
    changes: &HashMap<Url, Vec<TextEdit>>,
    source_overrides: &[(PathBuf, String)],
) -> Option<Vec<(PathBuf, String)>> {
    let mut sources = source_overrides.to_vec();
    replace_source_override(&mut sources, path, text.to_string());

    for (uri, edits) in changes {
        let source_path = if uri == current_uri {
            path.to_path_buf()
        } else {
            uri.to_file_path().ok()?
        };
        let source = sources
            .iter()
            .find(|(candidate, _)| candidate == &source_path)
            .map(|(_, source)| source.clone())
            .or_else(|| fs::read_to_string(&source_path).ok())?;
        let renamed = apply_text_edits(&source, edits)?;
        replace_source_override(&mut sources, &source_path, renamed);
    }
    Some(sources)
}

fn replace_source_override(sources: &mut Vec<(PathBuf, String)>, path: &Path, source: String) {
    if let Some((_, existing)) = sources.iter_mut().find(|(candidate, _)| candidate == path) {
        *existing = source;
    } else {
        sources.push((path.to_path_buf(), source));
    }
}

fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Option<String> {
    let mut byte_edits = edits
        .iter()
        .map(|edit| {
            Some((
                position_to_byte_offset(source, edit.range.start)?,
                position_to_byte_offset(source, edit.range.end)?,
                edit.new_text.as_str(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    byte_edits.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.cmp(&left.1)));

    let mut renamed = source.to_string();
    let mut previous_start = source.len();
    for (start, end, replacement) in byte_edits {
        if start > end || end > previous_start {
            return None;
        }
        renamed.replace_range(start..end, replacement);
        previous_start = start;
    }
    Some(renamed)
}

fn position_to_byte_offset(text: &str, position: Position) -> Option<usize> {
    let mut line_start = 0usize;
    for _ in 0..position.line {
        let newline = text[line_start..].find('\n')?;
        line_start += newline + 1;
    }
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |newline| line_start + newline);
    let line = &text[line_start..line_end];
    if position.character > line.encode_utf16().count() as u32 {
        return None;
    }
    Some(line_start + utf16_character_to_byte_index(line, position.character))
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
    use super::*;
    use tower_lsp::lsp_types::Range;

    #[test]
    fn text_edit_application_uses_utf16_columns() {
        let source = "io.println(\"你\") value\n";
        let renamed = apply_text_edits(
            source,
            &[TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 16,
                    },
                    end: Position {
                        line: 0,
                        character: 21,
                    },
                },
                new_text: "answer".to_string(),
            }],
        )
        .unwrap();

        assert_eq!(renamed, "io.println(\"你\") answer\n");
    }
}
