use super::lexer::{tokenize, LexError, Token};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub name: String,
    pub args: Vec<String>,

    pub block: Option<Vec<Directive>>,
    pub line: usize,
}

impl Directive {
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.args.get(index).map(|s| s.as_str())
    }

    pub fn source(&self) -> String {
        if self.args.is_empty() {
            format!("{};", self.name)
        } else {
            format!("{} {};", self.name, self.args.join(" "))
        }
    }

    pub fn children(&self, name: &str) -> Vec<&Directive> {
        match &self.block {
            Some(items) => items.iter().filter(|d| d.name == name).collect(),
            None => Vec::new(),
        }
    }

    pub fn child(&self, name: &str) -> Option<&Directive> {
        self.block
            .as_ref()
            .and_then(|items| items.iter().find(|d| d.name == name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "linha {}: {}", self.line, self.message)
    }
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError {
            line: e.line,
            message: e.message,
        }
    }
}

pub fn parse(input: &str) -> Result<Vec<Directive>, ParseError> {
    let tokens = tokenize(input)?;
    let mut cursor = 0;
    let directives = parse_block(&tokens, &mut cursor, 0)?;

    if cursor < tokens.len() {
        return Err(ParseError {
            line: tokens[cursor].line,
            message: "'}' sem bloco correspondente".to_string(),
        });
    }
    Ok(directives)
}

fn parse_block(
    tokens: &[super::lexer::SpannedToken],
    cursor: &mut usize,
    depth: usize,
) -> Result<Vec<Directive>, ParseError> {
    if depth > 64 {
        return Err(ParseError {
            line: tokens.get(*cursor).map(|t| t.line).unwrap_or(0),
            message: "blocos aninhados demais".to_string(),
        });
    }

    let mut directives = Vec::new();

    while *cursor < tokens.len() {
        let spanned = &tokens[*cursor];

        match &spanned.token {
            Token::CloseBrace => {
                if depth == 0 {
                    return Ok(directives);
                }
                *cursor += 1;
                return Ok(directives);
            }
            Token::Semicolon => {
                *cursor += 1;
            }
            Token::OpenBrace => {
                return Err(ParseError {
                    line: spanned.line,
                    message: "'{' sem uma diretiva antes".to_string(),
                });
            }
            Token::Word(name) => {
                let line = spanned.line;
                let name = name.clone();
                *cursor += 1;

                let mut args = Vec::new();
                loop {
                    let Some(next) = tokens.get(*cursor) else {
                        return Err(ParseError {
                            line,
                            message: format!("a diretiva '{}' termina sem ';'", name),
                        });
                    };

                    match &next.token {
                        Token::Word(w) => {
                            args.push(w.clone());
                            *cursor += 1;
                        }
                        Token::Semicolon => {
                            *cursor += 1;
                            directives.push(Directive {
                                name,
                                args,
                                block: None,
                                line,
                            });
                            break;
                        }
                        Token::OpenBrace => {
                            *cursor += 1;
                            let inner = parse_block(tokens, cursor, depth + 1)?;
                            directives.push(Directive {
                                name,
                                args,
                                block: Some(inner),
                                line,
                            });
                            break;
                        }
                        Token::CloseBrace => {
                            return Err(ParseError {
                                line: next.line,
                                message: format!("a diretiva '{}' termina sem ';'", name),
                            });
                        }
                    }
                }
            }
        }
    }

    if depth > 0 {
        return Err(ParseError {
            line: tokens.last().map(|t| t.line).unwrap_or(0),
            message: "bloco aberto sem '}'".to_string(),
        });
    }

    Ok(directives)
}

pub fn walk<'a>(directives: &'a [Directive], visit: &mut impl FnMut(&'a Directive)) {
    for d in directives {
        visit(d);
        if let Some(block) = &d.block {
            walk(block, visit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_directive_is_parsed() {
        let d = parse("listen 80;").unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "listen");
        assert_eq!(d[0].args, vec!["80"]);
        assert!(d[0].block.is_none());
    }

    #[test]
    fn block_directive_holds_children() {
        let d = parse("server { listen 80; server_name a.com; }").unwrap();
        assert_eq!(d[0].name, "server");
        let block = d[0].block.as_ref().unwrap();
        assert_eq!(block.len(), 2);
        assert_eq!(block[1].args, vec!["a.com"]);
    }

    #[test]
    fn block_directive_keeps_its_arguments() {
        let d = parse("upstream backend { server a:3000; }").unwrap();
        assert_eq!(d[0].args, vec!["backend"]);
        assert_eq!(d[0].block.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn nesting_is_preserved() {
        let src = "http { server { location / { proxy_pass http://b; } } }";
        let d = parse(src).unwrap();
        let http = &d[0];
        let server = &http.block.as_ref().unwrap()[0];
        let location = &server.block.as_ref().unwrap()[0];
        assert_eq!(location.name, "location");
        assert_eq!(location.args, vec!["/"]);
        assert_eq!(
            location.block.as_ref().unwrap()[0].name,
            "proxy_pass".to_string()
        );
    }

    #[test]
    fn location_modifiers_are_separate_arguments() {
        let d = parse("location = /exato { }").unwrap();
        assert_eq!(d[0].args, vec!["=", "/exato"]);

        let d = parse(r"location ~* \.png$ { }").unwrap();
        assert_eq!(d[0].args, vec!["~*", r"\.png$"]);
    }

    #[test]
    fn line_numbers_are_recorded() {
        let d = parse("a;\n\n\nb;").unwrap();
        assert_eq!(d[0].line, 1);
        assert_eq!(d[1].line, 4);
    }

    #[test]
    fn missing_semicolon_is_an_error() {
        let err = parse("listen 80").unwrap_err();
        assert!(err.message.contains("sem ';'"));
    }

    #[test]
    fn unclosed_block_is_an_error() {
        assert!(parse("server { listen 80;").is_err());
    }

    #[test]
    fn extra_closing_brace_is_an_error() {
        assert!(parse("server { } }").is_err());
    }

    #[test]
    fn empty_block_is_valid() {
        let d = parse("location / { }").unwrap();
        assert_eq!(d[0].block.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn helpers_find_children() {
        let d = parse("server { listen 80; listen 443; server_name a; }").unwrap();
        assert_eq!(d[0].children("listen").len(), 2);
        assert_eq!(d[0].child("server_name").unwrap().args, vec!["a"]);
        assert!(d[0].child("root").is_none());
    }

    #[test]
    fn walk_visits_every_nested_directive() {
        let d = parse("http { server { location / { proxy_pass http://b; } } }").unwrap();
        let mut names = Vec::new();
        walk(&d, &mut |dir| names.push(dir.name.clone()));
        assert_eq!(names, vec!["http", "server", "location", "proxy_pass"]);
    }

    #[test]
    fn source_reconstructs_readable_text() {
        let d = parse("proxy_set_header Host $host;").unwrap();
        assert_eq!(d[0].source(), "proxy_set_header Host $host;");
    }

    #[test]
    fn a_real_config_shape_parses() {
        let src = r#"
            events { worker_connections 1024; }
            http {
                upstream backend {
                    least_conn;
                    server app1:3000 weight=3 max_fails=2 fail_timeout=10s;
                    server app2:3000 backup;
                }
                server {
                    listen 80;
                    server_name localhost;
                    location /api/ {
                        proxy_pass http://backend/;
                        proxy_set_header Host $host;
                    }
                    location = /health { return 200 "ok"; }
                }
            }
        "#;
        let d = parse(src).unwrap();
        assert_eq!(d.len(), 2);
        let http = d.iter().find(|x| x.name == "http").unwrap();
        assert_eq!(http.children("upstream").len(), 1);
        assert_eq!(http.children("server").len(), 1);
    }
}
