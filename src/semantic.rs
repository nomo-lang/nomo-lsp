use std::path::Path;

use nomo::{Token, TokenKind, lex};
use tower_lsp::lsp_types::{SemanticToken, SemanticTokenType};

// Indices into the legend below. Keep these in sync with `token_types`.
const KEYWORD: u32 = 0;
const TYPE: u32 = 1;
const VARIABLE: u32 = 2;
const STRING: u32 = 3;
const NUMBER: u32 = 4;
const OPERATOR: u32 = 5;
const FUNCTION: u32 = 6;
const PROPERTY: u32 = 7;
const ENUM_MEMBER: u32 = 8;

pub fn token_types() -> Vec<SemanticTokenType> {
    vec![
        SemanticTokenType::KEYWORD,
        SemanticTokenType::TYPE,
        SemanticTokenType::VARIABLE,
        SemanticTokenType::STRING,
        SemanticTokenType::NUMBER,
        SemanticTokenType::OPERATOR,
        SemanticTokenType::FUNCTION,
        SemanticTokenType::PROPERTY,
        SemanticTokenType::ENUM_MEMBER,
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

    let mut context = SemanticContext::default();
    for index in 0..raw.len() {
        let token = &raw[index];
        let Some((token_type, length)) = context.classify(&raw, index) else {
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

#[derive(Debug, Default)]
struct SemanticContext {
    declarations: Vec<DeclarationScope>,
}

#[derive(Debug)]
struct DeclarationScope {
    kind: DeclarationScopeKind,
    brace_depth: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclarationScopeKind {
    Struct,
    Enum,
}

impl SemanticContext {
    fn classify(&mut self, tokens: &[Token], index: usize) -> Option<(u32, u32)> {
        self.update_declaration_scopes(tokens, index);

        let token = &tokens[index];
        let kind = match &token.kind {
            TokenKind::Ident(name) => self.classify_ident(tokens, index, name),
            other => classify(other),
        };

        self.finish_declaration_scopes(tokens, index);
        kind
    }

    fn classify_ident(&self, tokens: &[Token], index: usize, name: &str) -> Option<(u32, u32)> {
        let len = name.chars().count() as u32;
        let previous = previous_significant(tokens, index);
        let next_raw = tokens.get(index + 1);
        let next = next_significant(tokens, index);
        let starts_upper = name.chars().next().is_some_and(|c| c.is_ascii_uppercase());

        if previous.is_some_and(|token| matches!(&token.kind, TokenKind::Fn)) {
            return Some((FUNCTION, len));
        }
        if self.in_declaration(DeclarationScopeKind::Enum)
            && matches!(
                next_raw.map(|token| &token.kind),
                Some(TokenKind::LParen | TokenKind::Newline | TokenKind::RBrace)
            )
        {
            return Some((ENUM_MEMBER, len));
        }
        if previous.is_some_and(|token| matches!(&token.kind, TokenKind::Dot)) && starts_upper {
            return Some((ENUM_MEMBER, len));
        }
        if next.is_some_and(|token| matches!(&token.kind, TokenKind::LParen)) {
            return Some((FUNCTION, len));
        }
        if previous.is_some_and(|token| matches!(&token.kind, TokenKind::Dot)) {
            return Some((PROPERTY, len));
        }
        if self.in_declaration(DeclarationScopeKind::Struct)
            && next.is_some_and(|token| matches!(&token.kind, TokenKind::Colon))
        {
            return Some((PROPERTY, len));
        }
        if next.is_some_and(|token| matches!(&token.kind, TokenKind::Colon))
            && struct_literal_field_context(tokens, index)
        {
            return Some((PROPERTY, len));
        }

        if starts_upper {
            Some((TYPE, len))
        } else {
            Some((VARIABLE, len))
        }
    }

    fn update_declaration_scopes(&mut self, tokens: &[Token], index: usize) {
        let token = &tokens[index];
        if !matches!(token.kind, TokenKind::LBrace) {
            return;
        }
        let Some(previous) = previous_significant(tokens, index) else {
            return;
        };
        if !matches!(&previous.kind, TokenKind::Ident(_)) {
            return;
        }
        let Some(name_index) = previous_significant_index(tokens, previous_index(tokens, index))
        else {
            return;
        };
        let Some(declaration) =
            previous_significant_index(tokens, previous_index(tokens, name_index))
        else {
            return;
        };
        let kind = match &tokens[declaration].kind {
            TokenKind::Struct => DeclarationScopeKind::Struct,
            TokenKind::Enum => DeclarationScopeKind::Enum,
            _ => return,
        };
        self.declarations.push(DeclarationScope {
            kind,
            brace_depth: 1,
        });
    }

    fn finish_declaration_scopes(&mut self, tokens: &[Token], index: usize) {
        match tokens[index].kind {
            TokenKind::LBrace => {
                if self
                    .declarations
                    .last()
                    .is_some_and(|scope| scope.brace_depth == 1)
                    && self.opened_declaration_at(tokens, index)
                {
                    return;
                }
                if let Some(scope) = self.declarations.last_mut() {
                    scope.brace_depth += 1;
                }
            }
            TokenKind::RBrace => {
                if let Some(scope) = self.declarations.last_mut() {
                    scope.brace_depth = scope.brace_depth.saturating_sub(1);
                    if scope.brace_depth == 0 {
                        self.declarations.pop();
                    }
                }
            }
            _ => {}
        }
    }

    fn in_declaration(&self, kind: DeclarationScopeKind) -> bool {
        self.declarations
            .last()
            .is_some_and(|scope| scope.kind == kind && scope.brace_depth == 1)
    }

    fn opened_declaration_at(&self, tokens: &[Token], index: usize) -> bool {
        if !matches!(tokens[index].kind, TokenKind::LBrace) {
            return false;
        }
        let Some(previous) = previous_significant(tokens, index) else {
            return false;
        };
        if !matches!(&previous.kind, TokenKind::Ident(_)) {
            return false;
        }
        let Some(name_index) = previous_significant_index(tokens, previous_index(tokens, index))
        else {
            return false;
        };
        let Some(declaration) =
            previous_significant_index(tokens, previous_index(tokens, name_index))
        else {
            return false;
        };
        matches!(
            &tokens[declaration].kind,
            TokenKind::Struct | TokenKind::Enum
        )
    }
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
        TokenKind::Interface => keyword("interface"),
        TokenKind::Unsafe => keyword("unsafe"),
        TokenKind::Extern => keyword("extern"),
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

fn previous_significant(tokens: &[Token], index: usize) -> Option<&Token> {
    previous_significant_index(tokens, previous_index(tokens, index)).map(|index| &tokens[index])
}

fn previous_significant_index(tokens: &[Token], mut index: Option<usize>) -> Option<usize> {
    while let Some(current) = index {
        if !matches!(tokens[current].kind, TokenKind::Newline | TokenKind::Eof) {
            return Some(current);
        }
        index = previous_index(tokens, current);
    }
    None
}

fn previous_index(_tokens: &[Token], index: usize) -> Option<usize> {
    index.checked_sub(1)
}

fn next_significant(tokens: &[Token], index: usize) -> Option<&Token> {
    tokens[index + 1..]
        .iter()
        .find(|token| !matches!(token.kind, TokenKind::Newline | TokenKind::Eof))
}

fn struct_literal_field_context(tokens: &[Token], index: usize) -> bool {
    let mut depth = 0usize;
    let mut cursor = previous_index(tokens, index);
    while let Some(current) = cursor {
        match tokens[current].kind {
            TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => depth += 1,
            TokenKind::LParen | TokenKind::LBracket => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::LBrace if depth == 0 => {
                return previous_significant(tokens, current)
                    .is_some_and(|token| matches!(&token.kind, TokenKind::Ident(_)));
            }
            TokenKind::LBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
        cursor = previous_index(tokens, current);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_current_v0_1_keywords_and_operators() {
        for (kind, text) in [
            (TokenKind::Impl, "impl"),
            (TokenKind::Interface, "interface"),
            (TokenKind::Unsafe, "unsafe"),
            (TokenKind::Extern, "extern"),
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

    #[test]
    fn classifies_contextual_identifiers_for_lsp_highlighting() {
        let source = "package app.main\n\nstruct User {\n    name: string\n}\n\nenum Status {\n    Ok\n    Err(string)\n}\n\nfn greet(user: User) -> void {\n    println(user.name)\n    Status.Ok\n    let other: User = User { name: \"Ada\" }\n}\n";
        let raw = lex(Path::new("main.nomo"), source).unwrap();
        let mut context = SemanticContext::default();
        let mut classified = Vec::new();
        for index in 0..raw.len() {
            if let TokenKind::Ident(name) = &raw[index].kind {
                let token_type = context
                    .classify(&raw, index)
                    .map(|(token_type, _)| token_type);
                classified.push((name.as_str(), raw[index].line, token_type));
            } else {
                context.classify(&raw, index);
            }
        }

        assert!(classified.contains(&("greet", 12, Some(FUNCTION))));
        assert!(classified.contains(&("println", 13, Some(FUNCTION))));
        assert!(classified.contains(&("name", 4, Some(PROPERTY))));
        assert!(classified.contains(&("name", 13, Some(PROPERTY))));
        assert!(classified.contains(&("name", 15, Some(PROPERTY))));
        assert!(classified.contains(&("Ok", 8, Some(ENUM_MEMBER))));
        assert!(classified.contains(&("Ok", 14, Some(ENUM_MEMBER))));
        assert!(classified.contains(&("Err", 9, Some(ENUM_MEMBER))));
    }
}
