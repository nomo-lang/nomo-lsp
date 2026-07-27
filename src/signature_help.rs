use std::path::{Path, PathBuf};

use nomo::TokenKind;
use nomo::semantic as compiler_semantic;
use nomo_lsp_bridge::{SemanticSymbol, SemanticSymbolKind, TextPosition};
use tower_lsp::lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position,
    SignatureHelp, SignatureInformation,
};

pub(crate) fn signature_help_for_document(
    path: &Path,
    text: &str,
    position: Position,
    source_overrides: &[(PathBuf, String)],
) -> Option<SignatureHelp> {
    let tokens = nomo::lex(path, text).ok()?;
    let call = call_at_position(&tokens, position)?;
    let callee = &tokens[call.callee_index];
    let compiler_position = TextPosition {
        line: callee.line.saturating_sub(1) as u32,
        character: callee.column.saturating_sub(1) as u32,
    };
    let symbol = if let Ok(project) = nomo::project::discover_project(path) {
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

    signature_help_for_symbol(&symbol, call.active_parameter)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveCall {
    callee_index: usize,
    active_parameter: u32,
}

#[derive(Debug, Clone, Copy)]
struct ParenFrame {
    lparen_index: usize,
    active_parameter: u32,
    bracket_depth: usize,
    brace_depth: usize,
}

fn call_at_position(tokens: &[nomo::Token], position: Position) -> Option<ActiveCall> {
    let mut frames = Vec::<ParenFrame>::new();

    for (index, token) in tokens.iter().enumerate() {
        if token_starts_at_or_after(token, position) {
            break;
        }
        match token.kind {
            TokenKind::LParen => frames.push(ParenFrame {
                lparen_index: index,
                active_parameter: 0,
                bracket_depth: 0,
                brace_depth: 0,
            }),
            TokenKind::RParen => {
                frames.pop();
            }
            TokenKind::LBracket => {
                if let Some(frame) = frames.last_mut() {
                    frame.bracket_depth += 1;
                }
            }
            TokenKind::RBracket => {
                if let Some(frame) = frames.last_mut() {
                    frame.bracket_depth = frame.bracket_depth.saturating_sub(1);
                }
            }
            TokenKind::LBrace => {
                if let Some(frame) = frames.last_mut() {
                    frame.brace_depth += 1;
                }
            }
            TokenKind::RBrace => {
                if let Some(frame) = frames.last_mut() {
                    frame.brace_depth = frame.brace_depth.saturating_sub(1);
                }
            }
            TokenKind::Comma => {
                if let Some(frame) = frames.last_mut()
                    && frame.bracket_depth == 0
                    && frame.brace_depth == 0
                {
                    frame.active_parameter += 1;
                }
            }
            _ => {}
        }
    }

    let frame = frames.last()?;
    let callee_index = callee_before_lparen(tokens, frame.lparen_index)?;
    let previous = previous_significant(tokens, callee_index)?;
    if matches!(tokens[previous].kind, TokenKind::Fn) {
        return None;
    }

    Some(ActiveCall {
        callee_index,
        active_parameter: frame.active_parameter,
    })
}

fn token_starts_at_or_after(token: &nomo::Token, position: Position) -> bool {
    let line = token.line.saturating_sub(1) as u32;
    let character = token.column.saturating_sub(1) as u32;
    line > position.line || (line == position.line && character >= position.character)
}

fn callee_before_lparen(tokens: &[nomo::Token], lparen_index: usize) -> Option<usize> {
    let mut index = previous_significant(tokens, lparen_index)?;
    if matches!(tokens[index].kind, TokenKind::Ident(_)) {
        return Some(index);
    }
    if !matches!(tokens[index].kind, TokenKind::Greater) {
        return None;
    }

    let mut depth = 0usize;
    loop {
        match tokens[index].kind {
            TokenKind::Greater => depth += 1,
            TokenKind::Less => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return previous_significant(tokens, index).filter(|candidate| {
                        matches!(tokens[*candidate].kind, TokenKind::Ident(_))
                    });
                }
            }
            _ => {}
        }
        index = previous_significant(tokens, index)?;
    }
}

fn previous_significant(tokens: &[nomo::Token], index: usize) -> Option<usize> {
    (0..index)
        .rev()
        .find(|candidate| !matches!(tokens[*candidate].kind, TokenKind::Newline))
}

fn signature_help_for_symbol(
    symbol: &SemanticSymbol,
    call_parameter: u32,
) -> Option<SignatureHelp> {
    if !matches!(
        symbol.kind,
        SemanticSymbolKind::Function
            | SemanticSymbolKind::ExternFunction
            | SemanticSymbolKind::Method
            | SemanticSymbolKind::InterfaceMethod
    ) {
        return None;
    }

    let labels = parameter_labels(&symbol.signature)?;
    let implicit_receiver = matches!(
        symbol.kind,
        SemanticSymbolKind::Method | SemanticSymbolKind::InterfaceMethod
    ) && labels.first().is_some_and(|label| {
        label == "self" || label.starts_with("self:") || label.starts_with("mut self:")
    });
    let parameter_index = call_parameter + u32::from(implicit_receiver);
    let active_parameter = (parameter_index < labels.len() as u32).then_some(parameter_index);
    let parameters = labels
        .into_iter()
        .map(|label| ParameterInformation {
            label: ParameterLabel::Simple(label),
            documentation: None,
        })
        .collect();
    let documentation = (!symbol.docs.is_empty()).then(|| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: symbol.docs.clone(),
        })
    });

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: symbol.signature.clone(),
            documentation,
            parameters: Some(parameters),
            active_parameter,
        }],
        active_signature: Some(0),
        active_parameter,
    })
}

fn parameter_labels(signature: &str) -> Option<Vec<String>> {
    let open = signature.find('(')?;
    let mut depth = 0usize;
    let mut close = None;
    for (offset, ch) in signature[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let params = &signature[open + 1..close?];
    if params.trim().is_empty() {
        return Some(Vec::new());
    }

    let mut labels = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut angle_depth = 0usize;
    let mut bracket_depth = 0usize;
    for (offset, ch) in params.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '<' => angle_depth += 1,
            '>' => angle_depth = angle_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            ',' if paren_depth == 0 && angle_depth == 0 && bracket_depth == 0 => {
                labels.push(params[start..offset].trim().to_string());
                start = offset + ch.len_utf8();
            }
            _ => {}
        }
    }
    labels.push(params[start..].trim().to_string());
    Some(labels)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tower_lsp::lsp_types::Position;

    use super::{parameter_labels, signature_help_for_document};

    #[test]
    fn signature_help_uses_canonical_signature_and_active_parameter() {
        let path = PathBuf::from("main.nomo");
        let text = "package app\n\n/// Adds two values.\nfn add(left: i64, right: i64) -> i64 {\n    return left + right\n}\n\nfn main() {\n    let total: i64 = add(1, 2)\n}\n";
        let help = signature_help_for_document(
            &path,
            text,
            Position {
                line: 8,
                character: 29,
            },
            &[],
        )
        .unwrap();

        assert_eq!(
            help.signatures[0].label,
            "fn add(left: i64, right: i64) -> i64"
        );
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(help.signatures[0].active_parameter, Some(1));
    }

    #[test]
    fn signature_help_offsets_the_implicit_method_receiver() {
        let path = PathBuf::from("main.nomo");
        let text = "package app\n\nstruct Counter {\n    value: i64\n}\n\nimpl Counter {\n    fn add(self, delta: i64) -> i64 {\n        return self.value + delta\n    }\n}\n\nfn main() {\n    let counter = Counter { value: 1 }\n    let total = counter.add(2)\n}\n";
        let help = signature_help_for_document(
            &path,
            text,
            Position {
                line: 14,
                character: 29,
            },
            &[],
        )
        .unwrap();

        assert_eq!(
            help.signatures[0].label,
            "fn Counter.add(self: Counter, delta: i64) -> i64"
        );
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn signature_help_preserves_callable_void_returns() {
        let path = PathBuf::from("main.nomo");
        let text = "package app\n\nfn install(callback: task fn(string) -> void) {\n}\n\nfn handler(message: string) {\n}\n\nfn main() {\n    install(handler)\n}\n";
        let help = signature_help_for_document(
            &path,
            text,
            Position {
                line: 9,
                character: 13,
            },
            &[],
        )
        .unwrap();

        assert_eq!(
            help.signatures[0].label,
            "fn install(callback: task fn(string) -> void)"
        );
        assert_eq!(
            parameter_labels(&help.signatures[0].label).unwrap(),
            vec!["callback: task fn(string) -> void"]
        );
    }

    #[test]
    fn signature_help_is_not_offered_for_a_declaration_parameter_list() {
        let path = PathBuf::from("main.nomo");
        let text =
            "package app\n\nfn add(left: i64, right: i64) -> i64 {\n    return left + right\n}\n";

        assert!(
            signature_help_for_document(
                &path,
                text,
                Position {
                    line: 2,
                    character: 20,
                },
                &[],
            )
            .is_none()
        );
    }
}
