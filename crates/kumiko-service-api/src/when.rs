//! When-expressions over context keys (spec, section 11): `&&`, `||`, `!`, `==`, `!=`,
//! `=~` (regex), `in`, `not in` and parentheses.
//!
//! As in VS Code, the right side of `==`/`!=` is a literal: `editorLangId == rust` compares
//! with the string `rust`; `true`, `false` and numbers compare as such. A bare key is true
//! when its value is set and not `false`, `null`, `0` or `""`. `a in b` checks that the
//! value of key `a` is an element (array) or a key (object) of the value of key `b`.

use std::collections::HashMap;
use std::fmt;

use regex::{Regex, RegexBuilder};
use serde_json::Value;

/// Source of context key values.
pub trait Context {
    fn get(&self, key: &str) -> Option<Value>;
}

impl Context for HashMap<String, Value> {
    fn get(&self, key: &str) -> Option<Value> {
        HashMap::get(self, key).cloned()
    }
}

impl<F: Fn(&str) -> Option<Value>> Context for F {
    fn get(&self, key: &str) -> Option<Value> {
        self(key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid when-expression at {position}: {message}")]
pub struct WhenError {
    pub position: usize,
    pub message: String,
}

/// A parsed when-expression.
#[derive(Debug, Clone)]
pub struct WhenExpr {
    source: String,
    root: Node,
}

#[derive(Debug, Clone)]
enum Node {
    Constant(bool),
    Key(String),
    Not(Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Equals {
        key: String,
        value: Literal,
        negate: bool,
    },
    Matches {
        key: String,
        regex: Regex,
    },
    In {
        key: String,
        container: String,
        negate: bool,
    },
}

#[derive(Debug, Clone)]
enum Literal {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl WhenExpr {
    pub fn parse(source: &str) -> Result<Self, WhenError> {
        let tokens = tokenize(source)?;
        let mut parser = Parser {
            tokens,
            index: 0,
            end: source.len(),
        };
        let root = parser.or()?;
        if let Some((position, token)) = parser.tokens.get(parser.index) {
            return Err(WhenError {
                position: *position,
                message: format!("unexpected {token}"),
            });
        }
        Ok(Self {
            source: source.to_owned(),
            root,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn eval(&self, context: &dyn Context) -> bool {
        eval(&self.root, context)
    }
}

impl fmt::Display for WhenExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn equals(value: Option<&Value>, literal: &Literal) -> bool {
    let Some(value) = value else {
        return false;
    };
    match literal {
        Literal::Str(s) => as_text(value).as_deref() == Some(s),
        Literal::Num(n) => match value {
            Value::Number(v) => v.as_f64() == Some(*n),
            Value::String(v) => v.parse::<f64>().ok() == Some(*n),
            _ => false,
        },
        Literal::Bool(b) => match value {
            Value::Bool(v) => v == b,
            Value::String(v) => v == &b.to_string(),
            _ => false,
        },
    }
}

fn eval(node: &Node, context: &dyn Context) -> bool {
    match node {
        Node::Constant(value) => *value,
        Node::Key(key) => context.get(key).is_some_and(|v| truthy(&v)),
        Node::Not(inner) => !eval(inner, context),
        Node::And(a, b) => eval(a, context) && eval(b, context),
        Node::Or(a, b) => eval(a, context) || eval(b, context),
        Node::Equals { key, value, negate } => equals(context.get(key).as_ref(), value) != *negate,
        Node::Matches { key, regex } => context
            .get(key)
            .and_then(|v| as_text(&v))
            .is_some_and(|text| regex.is_match(&text)),
        Node::In {
            key,
            container,
            negate,
        } => {
            let found = match (context.get(key), context.get(container)) {
                (Some(item), Some(Value::Array(items))) => items.iter().any(|candidate| {
                    candidate == &item
                        || as_text(candidate).is_some_and(|c| Some(c) == as_text(&item))
                }),
                (Some(item), Some(Value::Object(map))) => {
                    as_text(&item).is_some_and(|name| map.contains_key(&name))
                }
                _ => false,
            };
            found != *negate
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Str(String),
    Regex { pattern: String, flags: String },
    And,
    Or,
    Not,
    Eq,
    Ne,
    Match,
    Open,
    Close,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Word(w) => write!(f, "`{w}`"),
            Self::Str(s) => write!(f, "string {s:?}"),
            Self::Regex { pattern, .. } => write!(f, "regex /{pattern}/"),
            Self::And => f.write_str("`&&`"),
            Self::Or => f.write_str("`||`"),
            Self::Not => f.write_str("`!`"),
            Self::Eq => f.write_str("`==`"),
            Self::Ne => f.write_str("`!=`"),
            Self::Match => f.write_str("`=~`"),
            Self::Open => f.write_str("`(`"),
            Self::Close => f.write_str("`)`"),
        }
    }
}

fn tokenize(source: &str) -> Result<Vec<(usize, Token)>, WhenError> {
    let chars: Vec<(usize, char)> = source.char_indices().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let error = |position: usize, message: &str| WhenError {
        position,
        message: message.to_owned(),
    };
    while i < chars.len() {
        let (pos, c) = chars[i];
        let next = chars.get(i + 1).map(|(_, c)| *c);
        let two = |token: Token| (pos, token);
        match c {
            c if c.is_whitespace() => i += 1,
            '&' if next == Some('&') => {
                tokens.push(two(Token::And));
                i += 2;
            }
            '|' if next == Some('|') => {
                tokens.push(two(Token::Or));
                i += 2;
            }
            '=' if next == Some('=') => {
                tokens.push(two(Token::Eq));
                i += 2;
            }
            '!' if next == Some('=') => {
                tokens.push(two(Token::Ne));
                i += 2;
            }
            '=' if next == Some('~') => {
                tokens.push(two(Token::Match));
                i += 2;
                // A regex literal may follow.
                while chars.get(i).is_some_and(|(_, c)| c.is_whitespace()) {
                    i += 1;
                }
                if let Some(&(start, '/')) = chars.get(i) {
                    let mut pattern = String::new();
                    i += 1;
                    loop {
                        match chars.get(i) {
                            None => return Err(error(start, "unterminated regex")),
                            Some((_, '\\')) if chars.get(i + 1).is_some_and(|(_, c)| *c == '/') => {
                                pattern.push('/');
                                i += 2;
                            }
                            Some((_, '/')) => {
                                i += 1;
                                break;
                            }
                            Some((_, c)) => {
                                pattern.push(*c);
                                i += 1;
                            }
                        }
                    }
                    let mut flags = String::new();
                    while let Some((_, c)) = chars.get(i).filter(|(_, c)| c.is_ascii_alphabetic()) {
                        flags.push(*c);
                        i += 1;
                    }
                    tokens.push((start, Token::Regex { pattern, flags }));
                }
            }
            '!' => {
                tokens.push(two(Token::Not));
                i += 1;
            }
            '(' => {
                tokens.push(two(Token::Open));
                i += 1;
            }
            ')' => {
                tokens.push(two(Token::Close));
                i += 1;
            }
            '\'' | '"' => {
                let quote = c;
                let mut text = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err(error(pos, "unterminated string")),
                        Some((_, '\\')) => {
                            if let Some((_, escaped)) = chars.get(i + 1) {
                                text.push(*escaped);
                            }
                            i += 2;
                        }
                        Some((_, c)) if *c == quote => {
                            i += 1;
                            break;
                        }
                        Some((_, c)) => {
                            text.push(*c);
                            i += 1;
                        }
                    }
                }
                tokens.push((pos, Token::Str(text)));
            }
            '&' | '|' | '=' => return Err(error(pos, &format!("unexpected `{c}`"))),
            _ => {
                let mut word = String::new();
                while let Some((_, c)) = chars.get(i) {
                    if c.is_whitespace() || "()!&|='\"".contains(*c) {
                        break;
                    }
                    word.push(*c);
                    i += 1;
                }
                tokens.push((pos, Token::Word(word)));
            }
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<(usize, Token)>,
    index: usize,
    end: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index).map(|(_, t)| t)
    }

    fn position(&self) -> usize {
        self.tokens.get(self.index).map_or(self.end, |(p, _)| *p)
    }

    fn fail<T>(&self, message: impl Into<String>) -> Result<T, WhenError> {
        Err(WhenError {
            position: self.position(),
            message: message.into(),
        })
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.index).map(|(_, t)| t.clone());
        self.index += 1;
        token
    }

    fn or(&mut self) -> Result<Node, WhenError> {
        let mut node = self.and()?;
        while self.peek() == Some(&Token::Or) {
            self.index += 1;
            node = Node::Or(Box::new(node), Box::new(self.and()?));
        }
        Ok(node)
    }

    fn and(&mut self) -> Result<Node, WhenError> {
        let mut node = self.unary()?;
        while self.peek() == Some(&Token::And) {
            self.index += 1;
            node = Node::And(Box::new(node), Box::new(self.unary()?));
        }
        Ok(node)
    }

    fn unary(&mut self) -> Result<Node, WhenError> {
        match self.peek() {
            Some(Token::Not) => {
                self.index += 1;
                Ok(Node::Not(Box::new(self.unary()?)))
            }
            Some(Token::Open) => {
                self.index += 1;
                let node = self.or()?;
                if self.next() != Some(Token::Close) {
                    self.index -= 1;
                    return self.fail("expected `)`");
                }
                Ok(node)
            }
            _ => self.term(),
        }
    }

    fn term(&mut self) -> Result<Node, WhenError> {
        let key = match self.next() {
            Some(Token::Word(word)) => word,
            None => return self.fail("expected a context key"),
            Some(other) => {
                self.index -= 1;
                return self.fail(format!("expected a context key, found {other}"));
            }
        };
        match self.peek() {
            Some(Token::Eq | Token::Ne) => {
                let negate = self.next() == Some(Token::Ne);
                let value = match self.next() {
                    Some(Token::Str(s)) => Literal::Str(s),
                    Some(Token::Word(w)) => literal(&w),
                    _ => {
                        self.index -= 1;
                        return self.fail("expected a value");
                    }
                };
                Ok(Node::Equals { key, value, negate })
            }
            Some(Token::Match) => {
                self.index += 1;
                let (pattern, flags) = match self.next() {
                    Some(Token::Regex { pattern, flags }) => (pattern, flags),
                    Some(Token::Str(pattern)) => (pattern, String::new()),
                    _ => {
                        self.index -= 1;
                        return self.fail("expected a regex after `=~`");
                    }
                };
                let regex = RegexBuilder::new(&pattern)
                    .case_insensitive(flags.contains('i'))
                    .multi_line(flags.contains('m'))
                    .dot_matches_new_line(flags.contains('s'))
                    .build();
                match regex {
                    Ok(regex) => Ok(Node::Matches { key, regex }),
                    Err(e) => self.fail(format!("invalid regex: {e}")),
                }
            }
            Some(Token::Word(w)) if w == "in" || w == "not" => {
                let negate = w == "not";
                self.index += 1;
                if negate && self.next() != Some(Token::Word("in".into())) {
                    self.index -= 1;
                    return self.fail("expected `in` after `not`");
                }
                match self.next() {
                    Some(Token::Word(container)) => Ok(Node::In {
                        key,
                        container,
                        negate,
                    }),
                    _ => {
                        self.index -= 1;
                        self.fail("expected a context key after `in`")
                    }
                }
            }
            _ => Ok(match key.as_str() {
                "true" => Node::Constant(true),
                "false" => Node::Constant(false),
                _ => Node::Key(key),
            }),
        }
    }
}

fn literal(word: &str) -> Literal {
    match word {
        "true" => Literal::Bool(true),
        "false" => Literal::Bool(false),
        _ => match word.parse::<f64>() {
            Ok(n)
                if word
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '-') =>
            {
                Literal::Num(n)
            }
            _ => Literal::Str(word.to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn context() -> HashMap<String, Value> {
        [
            ("editorFocus", json!(true)),
            ("editorLangId", json!("rust")),
            ("resourceExtname", json!(".rs")),
            ("workspaceFolderCount", json!(2)),
            ("emptyText", json!("")),
            ("selected", json!("b")),
            ("list", json!(["a", "b"])),
            ("map", json!({"x": 1})),
            ("resourcePath", json!("/w/src/Main.RS")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }

    fn check(source: &str) -> bool {
        WhenExpr::parse(source)
            .unwrap_or_else(|e| panic!("{source}: {e}"))
            .eval(&context())
    }

    #[test]
    fn evaluates_like_vscode() {
        let cases = [
            ("editorFocus", true),
            ("!editorFocus", false),
            ("missing", false),
            ("emptyText", false),
            ("editorLangId == rust", true),
            ("editorLangId == 'rust'", true),
            ("editorLangId != rust", false),
            ("resourceExtname == .rs", true),
            ("workspaceFolderCount == 2", true),
            ("workspaceFolderCount == '2'", true),
            ("missing != x", true),
            ("editorFocus == true", true),
            ("editorFocus && editorLangId == rust", true),
            ("!editorFocus || workspaceFolderCount == 1", false),
            ("editorFocus && (missing || editorLangId == rust)", true),
            ("!(editorFocus && missing)", true),
            ("resourcePath =~ /\\.rs$/i", true),
            ("resourcePath =~ /\\.rs$/", false),
            ("resourcePath =~ /src\\/Main/", true),
            ("selected in list", true),
            ("editorLangId in list", false),
            ("editorLangId not in list", true),
            ("selected in map", false),
            ("true", true),
            ("false || editorFocus", true),
        ];
        for (source, expected) in cases {
            assert_eq!(check(source), expected, "{source}");
        }
    }

    #[test]
    fn precedence_of_and_over_or() {
        // a || b && c == a || (b && c)
        assert!(check("editorFocus || missing && missing"));
        assert!(!check("(editorFocus || missing) && missing"));
    }

    #[test]
    fn syntax_errors_have_positions() {
        for (source, position) in [
            ("", 0),
            ("a &&", 4),
            ("(a", 2),
            ("a == ", 5),
            ("a b", 2),
            ("a =~ /x", 5),
            ("a = b", 2),
            ("a == 'x", 5),
            ("a not x", 6),
            ("a =~ /(/", 8),
        ] {
            let error = WhenExpr::parse(source).unwrap_err();
            assert_eq!(error.position, position, "{source}: {error}");
        }
    }

    #[test]
    fn closures_are_contexts() {
        let expr = WhenExpr::parse("plugin.kumiko.sample.running").unwrap();
        assert!(
            expr.eval(&|key: &str| (key == "plugin.kumiko.sample.running").then_some(json!(true)))
        );
        assert_eq!(expr.to_string(), "plugin.kumiko.sample.running");
    }
}
