//! Tokenizador do formato de configuração do nginx.
//!
//! Trabalha só no nível léxico: separa palavras, blocos e terminadores, sem saber o que
//! qualquer diretiva significa. Isso mantém a interpretação semântica num lugar só e
//! permite que o relatório de migração aponte linha exata do que não foi convertido.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Uma palavra: nome de diretiva, argumento, padrão. Aspas já removidas.
    Word(String),
    Semicolon,
    OpenBrace,
    CloseBrace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpannedToken {
    pub token: Token,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "linha {}: {}", self.line, self.message)
    }
}

pub fn tokenize(input: &str) -> Result<Vec<SpannedToken>, LexError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1;

    while i < chars.len() {
        let c = chars[i];

        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Comentário: vai até o fim da linha.
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        match c {
            ';' => {
                tokens.push(SpannedToken {
                    token: Token::Semicolon,
                    line,
                });
                i += 1;
            }
            '{' => {
                tokens.push(SpannedToken {
                    token: Token::OpenBrace,
                    line,
                });
                i += 1;
            }
            '}' => {
                tokens.push(SpannedToken {
                    token: Token::CloseBrace,
                    line,
                });
                i += 1;
            }
            '"' | '\'' => {
                let quote = c;
                let start_line = line;
                i += 1;
                let mut value = String::new();
                let mut closed = false;

                while i < chars.len() {
                    let ch = chars[i];
                    if ch == '\\' && i + 1 < chars.len() {
                        // A barra invertida protege o próximo caractere, inclusive a aspa.
                        value.push(chars[i + 1]);
                        if chars[i + 1] == '\n' {
                            line += 1;
                        }
                        i += 2;
                        continue;
                    }
                    if ch == quote {
                        closed = true;
                        i += 1;
                        break;
                    }
                    if ch == '\n' {
                        line += 1;
                    }
                    value.push(ch);
                    i += 1;
                }

                if !closed {
                    return Err(LexError {
                        line: start_line,
                        message: format!("aspa {} sem fechamento", quote),
                    });
                }
                tokens.push(SpannedToken {
                    token: Token::Word(value),
                    line: start_line,
                });
            }
            _ => {
                let start_line = line;
                let mut value = String::new();
                while i < chars.len() {
                    let ch = chars[i];
                    if ch.is_whitespace() || ch == ';' || ch == '{' || ch == '}' || ch == '#' {
                        break;
                    }
                    // Uma aspa colada a texto ainda agrupa, como em `zone=a:10m"x"`.
                    if ch == '"' || ch == '\'' {
                        break;
                    }
                    value.push(ch);
                    i += 1;
                }
                if value.is_empty() {
                    // Nenhum caractere consumido significaria laço infinito.
                    i += 1;
                    continue;
                }
                tokens.push(SpannedToken {
                    token: Token::Word(value),
                    line: start_line,
                });
            }
        }
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(input: &str) -> Vec<String> {
        tokenize(input)
            .unwrap()
            .into_iter()
            .filter_map(|t| match t.token {
                Token::Word(w) => Some(w),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn simple_directive_is_tokenized() {
        let tokens = tokenize("listen 80;").unwrap();
        assert_eq!(
            tokens.iter().map(|t| t.token.clone()).collect::<Vec<_>>(),
            vec![
                Token::Word("listen".into()),
                Token::Word("80".into()),
                Token::Semicolon
            ]
        );
    }

    #[test]
    fn blocks_produce_braces() {
        let tokens = tokenize("server { listen 80; }").unwrap();
        assert!(tokens.iter().any(|t| t.token == Token::OpenBrace));
        assert!(tokens.iter().any(|t| t.token == Token::CloseBrace));
    }

    #[test]
    fn comments_are_ignored() {
        assert_eq!(words("# tudo comentado\nlisten 80;"), vec!["listen", "80"]);
        assert_eq!(words("listen 80; # à direita"), vec!["listen", "80"]);
    }

    #[test]
    fn quoted_strings_keep_their_spaces() {
        assert_eq!(
            words(r#"add_header X-Teste "valor com espaco";"#),
            vec!["add_header", "X-Teste", "valor com espaco"]
        );
    }

    #[test]
    fn single_quotes_work_too() {
        assert_eq!(
            words("log_format x 'a b c';"),
            vec!["log_format", "x", "a b c"]
        );
    }

    #[test]
    fn escaped_quote_stays_in_the_value() {
        assert_eq!(words(r#"x "diz \"oi\"";"#), vec!["x", r#"diz "oi""#]);
    }

    #[test]
    fn unterminated_quote_is_reported_with_its_line() {
        let err = tokenize("linha1 ok;\nadd_header X \"sem fim;\n").unwrap_err();
        assert_eq!(err.line, 2);
    }

    #[test]
    fn line_numbers_track_newlines() {
        let tokens = tokenize("a;\n\nb;\n").unwrap();
        let b = tokens
            .iter()
            .find(|t| t.token == Token::Word("b".into()))
            .unwrap();
        assert_eq!(b.line, 3);
    }

    #[test]
    fn regex_patterns_survive_intact() {
        assert_eq!(
            words(r"location ~* \.(png|jpg)$ { }"),
            vec!["location", "~*", r"\.(png|jpg)$"]
        );
    }

    #[test]
    fn variables_are_plain_words() {
        assert_eq!(
            words("proxy_set_header Host $host;"),
            vec!["proxy_set_header", "Host", "$host"]
        );
    }

    #[test]
    fn braces_glued_to_words_still_separate() {
        let tokens = tokenize("events{worker_connections 1024;}").unwrap();
        assert_eq!(tokens[0].token, Token::Word("events".into()));
        assert_eq!(tokens[1].token, Token::OpenBrace);
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        assert!(tokenize("").unwrap().is_empty());
        assert!(tokenize("   \n\n  # só comentário\n").unwrap().is_empty());
    }
}
