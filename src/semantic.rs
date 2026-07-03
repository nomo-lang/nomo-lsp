use std::path::Path;

use nomo::{TokenKind, lex};
use tower_lsp::lsp_types::{SemanticToken, SemanticTokenType};

// Indices into the legend below. Keep these in sync with `token_types`.
const KEYWORD: u32 = 0;
const TYPE: u32 = 1;
const VARIABLE: u32 = 2;
const STRING: u32 = 3;
const NUMBER: u32 = 4;
const OPERATOR: u32 = 5;

pub fn token_types() -> Vec<SemanticTokenType> {
    vec![
        SemanticTokenType::KEYWORD,
        SemanticTokenType::TYPE,
        SemanticTokenType::VARIABLE,
        SemanticTokenType::STRING,
        SemanticTokenType::NUMBER,
        SemanticTokenType::OPERATOR,
    ]
}

/// Lex the source and emit delta-encoded semantic tokens. Lexing errors yield no
/// tokens (the diagnostic pipeline reports the problem separately).
pub fn tokens(path: &Path, source: &str) -> Vec<SemanticToken> {
    let Ok(raw) = lex(path, source) else {
        return Vec::new();
    };

    let mut result = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    for token in raw {
        let Some((token_type, length)) = classify(&token.kind) else {
            continue;
        };
        if length == 0 {
            continue;
        }

        // Lexer positions are 1-based; LSP wants 0-based.
        let line = token.line.saturating_sub(1) as u32;
        let start = token.column.saturating_sub(1) as u32;

        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            start.saturating_sub(prev_start)
        } else {
            start
        };

        result.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });

        prev_line = line;
        prev_start = start;
    }

    result
}

/// Map a token kind to its semantic token type and on-screen length. Returns
/// `None` for trivia and punctuation that should not be highlighted.
fn classify(kind: &TokenKind) -> Option<(u32, u32)> {
    let keyword = |text: &str| Some((KEYWORD, text.chars().count() as u32));

    match kind {
        TokenKind::Package => keyword("package"),
        TokenKind::Import => keyword("import"),
        TokenKind::Pub => keyword("pub"),
        TokenKind::Impl => keyword("impl"),
        TokenKind::Fn => keyword("fn"),
        TokenKind::Struct => keyword("struct"),
        TokenKind::Enum => keyword("enum"),
        TokenKind::Const => keyword("const"),
        TokenKind::If => keyword("if"),
        TokenKind::Else => keyword("else"),
        TokenKind::Match => keyword("match"),
        TokenKind::Panic => keyword("panic"),
        TokenKind::As => keyword("as"),
        TokenKind::Let => keyword("let"),
        TokenKind::Mut => keyword("mut"),
        TokenKind::Return => keyword("return"),
        TokenKind::For => keyword("for"),
        TokenKind::In => keyword("in"),
        TokenKind::Break => keyword("break"),
        TokenKind::Continue => keyword("continue"),
        TokenKind::Defer => keyword("defer"),
        TokenKind::Void => keyword("void"),
        TokenKind::True => keyword("true"),
        TokenKind::False => keyword("false"),
        TokenKind::Ident(name) => {
            let len = name.chars().count() as u32;
            // Treat capitalized identifiers as types (structs, enums, generics).
            let starts_upper = name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
            if starts_upper {
                Some((TYPE, len))
            } else {
                Some((VARIABLE, len))
            }
        }
        TokenKind::String(value) => Some((STRING, value.chars().count() as u32 + 2)),
        TokenKind::Char(_) => Some((STRING, 3)),
        TokenKind::Int(value) => Some((NUMBER, value.to_string().chars().count() as u32)),
        TokenKind::Float(value) => Some((NUMBER, value.chars().count() as u32)),
        TokenKind::Plus
        | TokenKind::Equal
        | TokenKind::EqualEqual
        | TokenKind::BangEqual
        | TokenKind::Star
        | TokenKind::Less
        | TokenKind::Greater
        | TokenKind::Question => Some((OPERATOR, 1)),
        TokenKind::LessEqual | TokenKind::GreaterEqual | TokenKind::Arrow | TokenKind::FatArrow => {
            Some((OPERATOR, 2))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_current_v0_1_keywords_and_operators() {
        for (kind, text) in [
            (TokenKind::Impl, "impl"),
            (TokenKind::Const, "const"),
            (TokenKind::For, "for"),
            (TokenKind::In, "in"),
            (TokenKind::Break, "break"),
            (TokenKind::Continue, "continue"),
            (TokenKind::Defer, "defer"),
        ] {
            assert_eq!(classify(&kind), Some((KEYWORD, text.len() as u32)));
        }

        assert_eq!(classify(&TokenKind::Star), Some((OPERATOR, 1)));
    }
}
