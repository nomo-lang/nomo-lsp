use std::collections::HashMap;
use std::path::Path;

use nomo::ast::{BinaryOp, Expr, ForVariant, SourceFile, Span, Stmt, TypeRef};
use nomo::lexer::{Token, TokenKind};
use tower_lsp::lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};

pub(crate) fn inlay_hints_for_text(path: &Path, text: &str, range: Range) -> Vec<InlayHint> {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tower_lsp::lsp_types::{InlayHintKind, InlayHintLabel, Position, Range};

    use super::inlay_hints_for_text;

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
}
