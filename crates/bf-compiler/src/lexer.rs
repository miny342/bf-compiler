use crate::frontend::FrontendError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Cell,
    Void,
    Return,
    If,
    Else,
    While,
    Input,
    Output,
    Identifier(String),
    Number(u8),
    LeftBrace,
    RightBrace,
    LeftParen,
    RightParen,
    Semicolon,
    Assign,
    Plus,
    Minus,
    Bang,
    PlusAssign,
    MinusAssign,
    BangEqual,
    EqualEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    AmpAmp,
    PipePipe,
    LeftBracket,
    RightBracket,
    Eof,
}

pub(crate) fn lex(source: &str) -> Result<Vec<Token>, FrontendError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut position = 0;

    while position < bytes.len() {
        match bytes[position] {
            byte if byte.is_ascii_whitespace() => position += 1,
            b'/' if bytes.get(position + 1) == Some(&b'/') => {
                position += 2;
                while position < bytes.len() && !matches!(bytes[position], b'\n' | b'\r') {
                    position += 1;
                }
            }
            b'/' if bytes.get(position + 1) == Some(&b'*') => {
                let start = position;
                position += 2;
                while position + 1 < bytes.len()
                    && (bytes[position] != b'*' || bytes[position + 1] != b'/')
                {
                    position += 1;
                }
                if position + 1 >= bytes.len() {
                    return Err(FrontendError::at(start, "unterminated block comment"));
                }
                position += 2;
            }
            byte if is_identifier_start(byte) => {
                let start = position;
                position += 1;
                while position < bytes.len() && is_identifier_continue(bytes[position]) {
                    position += 1;
                }
                let text = &source[start..position];
                let kind = match text {
                    "cell" => TokenKind::Cell,
                    "void" => TokenKind::Void,
                    "return" => TokenKind::Return,
                    "if" => TokenKind::If,
                    "else" => TokenKind::Else,
                    "while" => TokenKind::While,
                    "input" => TokenKind::Input,
                    "output" => TokenKind::Output,
                    _ => TokenKind::Identifier(text.to_owned()),
                };
                tokens.push(Token {
                    kind,
                    offset: start,
                });
            }
            byte if byte.is_ascii_digit() => {
                let start = position;
                let (radix, digits_start) =
                    if byte == b'0' && matches!(bytes.get(position + 1), Some(b'x' | b'X')) {
                        position += 2;
                        (16, position)
                    } else {
                        (10, position)
                    };

                while position < bytes.len()
                    && if radix == 16 {
                        bytes[position].is_ascii_hexdigit()
                    } else {
                        bytes[position].is_ascii_digit()
                    }
                {
                    position += 1;
                }
                if position == digits_start {
                    return Err(FrontendError::at(start, "expected hexadecimal digits"));
                }
                if position < bytes.len() && is_identifier_continue(bytes[position]) {
                    return Err(FrontendError::at(start, "invalid integer literal"));
                }
                let digits = &source[digits_start..position];
                let value = u16::from_str_radix(digits, radix)
                    .map_err(|_| FrontendError::at(start, "integer literal is too large"))?;
                let value = u8::try_from(value).map_err(|_| {
                    FrontendError::at(start, "integer literal must be between 0 and 255")
                })?;
                tokens.push(Token {
                    kind: TokenKind::Number(value),
                    offset: start,
                });
            }
            b'\'' => {
                let start = position;
                let (value, next) = lex_character(bytes, position)?;
                position = next;
                tokens.push(Token {
                    kind: TokenKind::Number(value),
                    offset: start,
                });
            }
            byte => {
                let start = position;
                if !byte.is_ascii() {
                    return Err(FrontendError::at(
                        start,
                        "non-ASCII text is only allowed inside comments",
                    ));
                }
                position += 1;
                let kind = match byte {
                    b'{' => TokenKind::LeftBrace,
                    b'}' => TokenKind::RightBrace,
                    b'(' => TokenKind::LeftParen,
                    b')' => TokenKind::RightParen,
                    b'[' => TokenKind::LeftBracket,
                    b']' => TokenKind::RightBracket,
                    b';' => TokenKind::Semicolon,
                    b'=' if bytes.get(position) == Some(&b'=') => {
                        position += 1;
                        TokenKind::EqualEqual
                    }
                    b'=' => TokenKind::Assign,
                    b'+' if bytes.get(position) == Some(&b'=') => {
                        position += 1;
                        TokenKind::PlusAssign
                    }
                    b'+' => TokenKind::Plus,
                    b'-' if bytes.get(position) == Some(&b'=') => {
                        position += 1;
                        TokenKind::MinusAssign
                    }
                    b'-' => TokenKind::Minus,
                    b'!' if bytes.get(position) == Some(&b'=') => {
                        position += 1;
                        TokenKind::BangEqual
                    }
                    b'!' => TokenKind::Bang,
                    b'<' if bytes.get(position) == Some(&b'=') => {
                        position += 1;
                        TokenKind::LessEqual
                    }
                    b'<' => TokenKind::Less,
                    b'>' if bytes.get(position) == Some(&b'=') => {
                        position += 1;
                        TokenKind::GreaterEqual
                    }
                    b'>' => TokenKind::Greater,
                    b'&' if bytes.get(position) == Some(&b'&') => {
                        position += 1;
                        TokenKind::AmpAmp
                    }
                    b'|' if bytes.get(position) == Some(&b'|') => {
                        position += 1;
                        TokenKind::PipePipe
                    }
                    _ => {
                        return Err(FrontendError::at(
                            start,
                            format!("unexpected byte {:?}", char::from(byte)),
                        ));
                    }
                };
                tokens.push(Token {
                    kind,
                    offset: start,
                });
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        offset: bytes.len(),
    });
    Ok(tokens)
}

fn lex_character(bytes: &[u8], start: usize) -> Result<(u8, usize), FrontendError> {
    let mut position = start + 1;
    let Some(&first) = bytes.get(position) else {
        return Err(FrontendError::at(start, "unterminated character literal"));
    };

    let value = if first == b'\\' {
        position += 1;
        let Some(&escape) = bytes.get(position) else {
            return Err(FrontendError::at(start, "unterminated character escape"));
        };
        match escape {
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'0' => 0,
            b'\\' => b'\\',
            b'\'' => b'\'',
            b'x' => {
                let Some(digits) = bytes.get(position + 1..position + 3) else {
                    return Err(FrontendError::at(start, "expected two hexadecimal digits"));
                };
                if !digits.iter().all(u8::is_ascii_hexdigit) {
                    return Err(FrontendError::at(start, "expected two hexadecimal digits"));
                }
                position += 2;
                hexadecimal_value(digits[0]) * 16 + hexadecimal_value(digits[1])
            }
            _ => return Err(FrontendError::at(start, "unknown character escape")),
        }
    } else {
        if first == b'\'' || first.is_ascii_control() {
            return Err(FrontendError::at(start, "invalid character literal"));
        }
        first
    };

    position += 1;
    if bytes.get(position) != Some(&b'\'') {
        return Err(FrontendError::at(
            start,
            "character literal must contain exactly one byte",
        ));
    }
    Ok((value, position + 1))
}

fn hexadecimal_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => unreachable!(),
    }
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
