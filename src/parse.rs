//! JSON documents as trees of values with where each is, from a parser of
//! its own that accepts comments and trailing commas (JSONC), and recovers
//! from syntax errors, keeping what it could read.

use flareite_extension_schemas::document::{
  Document, Node, NodeKind, Part, Problem, Scalar, Severity, Span,
};
use flareite_extension_schemas::widen;
use serde_json::{Map, Number, Value as Json};

/// A syntax error.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
  pub span: Span,
  pub message: String,
}

/// Parses a JSON document for Flareite's schemas.
pub fn parse(text: &str) -> Document {
  let parsed = parse_json(text);
  let mut problems: Vec<Problem> = parsed
    .errors
    .into_iter()
    .map(|error| Problem {
      span: error.span,
      severity: Severity::Error,
      message: error.message,
    })
    .collect();
  problems.extend(parsed.duplicates.into_iter().map(|span| Problem {
    span,
    severity: Severity::Warning,
    message: "Duplicate key: the last one is used.".to_owned(),
  }));
  problems.sort_by_key(|problem| problem.span.start);
  let named_schema = match &parsed.tree.kind {
    NodeKind::Table(_) => parsed
      .tree
      .get("$schema")
      .and_then(Node::value)
      .and_then(|value| match value {
        Scalar::String(address) => Some(address.clone()),
        _ => None,
      }),
    _ => None,
  };
  Document {
    parts: vec![Part {
      tree: parsed.tree,
      span: 0..text.len(),
    }],
    named_schema,
    problems,
  }
}

/// A parsed JSON document.
pub struct JsonDocument {
  /// The value, which Flareite reads from the tree; `None` when the text
  /// has syntax errors.
  #[cfg_attr(not(test), allow(dead_code))]
  pub value: Option<Json>,
  pub tree: Node,
  pub errors: Vec<SyntaxError>,
  /// Keys given twice in an object: the later one is used.
  pub duplicates: Vec<Span>,
}

/// Most nested arrays and objects.
const MAX_DEPTH: usize = 512;

/// Parses JSON, allowing comments and trailing commas (JSONC).
pub fn parse_json(text: &str) -> JsonDocument {
  let mut parser = JsonParser {
    text,
    bytes: text.as_bytes(),
    at: 0,
    errors: Vec::new(),
    duplicates: Vec::new(),
    depth: 0,
  };
  parser.skip_trivia();
  let (value, tree) = if parser.at >= parser.bytes.len() {
    parser.error(0..0, "Expected a value; the document is empty");
    (Json::Null, Node::scalar(0..0, None))
  } else {
    let parsed = parser.value();
    parser.skip_trivia();
    if parser.at < parser.bytes.len() {
      let at = parser.at;
      parser.error(at..at, "Expected the end of the document");
    }
    parsed
  };
  let failed = !parser.errors.is_empty();
  JsonDocument {
    value: (!failed).then_some(value),
    tree,
    errors: parser.errors,
    duplicates: parser.duplicates,
  }
}

struct JsonParser<'a> {
  text: &'a str,
  bytes: &'a [u8],
  at: usize,
  errors: Vec<SyntaxError>,
  duplicates: Vec<Span>,
  depth: usize,
}

impl JsonParser<'_> {
  fn error(&mut self, span: Span, message: &str) {
    // One error per place: recovery can stumble over the same spot.
    if self
      .errors
      .last()
      .is_some_and(|last| last.span.start == span.start)
    {
      return;
    }
    self.errors.push(SyntaxError {
      span: widen(self.text, span),
      message: message.to_owned(),
    });
  }

  fn peek(&self) -> Option<u8> {
    self.bytes.get(self.at).copied()
  }

  fn skip_trivia(&mut self) {
    loop {
      match self.peek() {
        Some(b' ' | b'\t' | b'\n' | b'\r') => self.at += 1,
        Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'/') => {
          while self.peek().is_some_and(|b| b != b'\n') {
            self.at += 1;
          }
        }
        Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'*') => {
          let start = self.at;
          match self.text[self.at + 2..].find("*/") {
            Some(end) => self.at += 2 + end + 2,
            None => {
              self.at = self.bytes.len();
              self.error(start..start + 2, "Unterminated comment");
            }
          }
        }
        // A byte order mark at the start.
        Some(0xEF) if self.at == 0 && self.text.starts_with('\u{feff}') => self.at += 3,
        _ => return,
      }
    }
  }

  fn value(&mut self) -> (Json, Node) {
    let start = self.at;
    match self.peek() {
      Some(b'{') => self.object(),
      Some(b'[') => self.array(),
      Some(b'"') => {
        let string = self.string();
        let node = Node::scalar(start..self.at, Some(Scalar::String(string.clone())));
        (Json::String(string), node)
      }
      Some(b'-' | b'0'..=b'9') => self.number(),
      Some(b't') if self.text[self.at..].starts_with("true") => {
        self.at += 4;
        (
          Json::Bool(true),
          Node::scalar(start..self.at, Some(Scalar::Bool(true))),
        )
      }
      Some(b'f') if self.text[self.at..].starts_with("false") => {
        self.at += 5;
        (
          Json::Bool(false),
          Node::scalar(start..self.at, Some(Scalar::Bool(false))),
        )
      }
      Some(b'n') if self.text[self.at..].starts_with("null") => {
        self.at += 4;
        (Json::Null, Node::scalar(start..self.at, Some(Scalar::Null)))
      }
      _ => {
        // Skip the word that isn't a value.
        while self
          .peek()
          .is_some_and(|b| !matches!(b, b',' | b'}' | b']' | b':' | b' ' | b'\t' | b'\r' | b'\n'))
        {
          self.at += 1;
        }
        if self.at == start {
          self.error(start..start, "Expected a value");
        } else {
          self.error(start..self.at, "Expected a value");
        }
        // Keep `at` in a char boundary.
        while !self.text.is_char_boundary(self.at) {
          self.at += 1;
        }
        (Json::Null, Node::scalar(start..self.at, None))
      }
    }
  }

  fn nested(&mut self) -> bool {
    if self.depth >= MAX_DEPTH {
      let at = self.at;
      self.error(at..at + 1, "Too deeply nested");
      self.at = self.bytes.len();
      return false;
    }
    true
  }

  fn object(&mut self) -> (Json, Node) {
    let start = self.at;
    self.at += 1;
    let mut map = Map::new();
    let mut entries: Vec<(String, Node)> = Vec::new();
    if !self.nested() {
      return (Json::Object(map), Node::scalar(start..self.at, None));
    }
    self.depth += 1;
    loop {
      self.skip_trivia();
      match self.peek() {
        Some(b'}') => {
          self.at += 1;
          break;
        }
        None => {
          self.error(start..start + 1, "Unclosed object: expected '}'");
          break;
        }
        Some(b'"') => {
          let key_start = self.at;
          let key = self.string();
          let key_span = key_start..self.at;
          self.skip_trivia();
          if self.peek() == Some(b':') {
            self.at += 1;
          } else {
            let at = self.at;
            self.error(at..at, "Expected ':'");
          }
          self.skip_trivia();
          let (value, mut node) = if matches!(self.peek(), Some(b',' | b'}') | None) {
            let at = self.at;
            self.error(at..at, "Expected a value");
            (Json::Null, Node::scalar(at..at, None))
          } else {
            self.value()
          };
          node.key_span = Some(key_span.clone());
          if let Some(existing) = entries.iter().position(|(name, _)| *name == key) {
            self.duplicates.push(key_span);
            entries.remove(existing);
          }
          map.insert(key.clone(), value);
          entries.push((key, node));
          self.skip_trivia();
          match self.peek() {
            Some(b',') => self.at += 1,
            Some(b'}') | None => {}
            // A missing comma: read on.
            Some(b'"') => {
              let at = self.at;
              self.error(at..at, "Expected ',' or '}'");
            }
            _ => {
              let at = self.at;
              self.error(at..at, "Expected ',' or '}'");
              self.recover();
            }
          }
        }
        Some(_) => {
          let at = self.at;
          self.error(at..at, "Expected a property name in double quotes");
          self.recover();
          if self.peek() == Some(b',') {
            self.at += 1;
          }
        }
      }
    }
    self.depth -= 1;
    (
      Json::Object(map),
      Node {
        span: start..self.at,
        key_span: None,
        kind: NodeKind::Table(entries),
      },
    )
  }

  /// Skips to the next ',' or closing bracket on this level.
  fn recover(&mut self) {
    while let Some(b) = self.peek() {
      match b {
        b',' | b'}' | b']' => return,
        b'"' => {
          self.string();
        }
        b'{' | b'[' => {
          self.value();
        }
        _ => {
          self.at += 1;
          while !self.text.is_char_boundary(self.at) {
            self.at += 1;
          }
        }
      }
    }
  }

  fn array(&mut self) -> (Json, Node) {
    let start = self.at;
    self.at += 1;
    let mut values = Vec::new();
    let mut nodes = Vec::new();
    if !self.nested() {
      return (Json::Array(values), Node::scalar(start..self.at, None));
    }
    self.depth += 1;
    loop {
      self.skip_trivia();
      match self.peek() {
        Some(b']') => {
          self.at += 1;
          break;
        }
        None => {
          self.error(start..start + 1, "Unclosed array: expected ']'");
          break;
        }
        Some(_) => {
          let (value, node) = self.value();
          values.push(value);
          nodes.push(node);
          self.skip_trivia();
          match self.peek() {
            Some(b',') => self.at += 1,
            Some(b']') | None => {}
            Some(b'"' | b'{' | b'[' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n') => {
              let at = self.at;
              self.error(at..at, "Expected ',' or ']'");
            }
            _ => {
              let at = self.at;
              self.error(at..at, "Expected ',' or ']'");
              self.recover();
              if self.peek() == Some(b'}') {
                // A stray brace: leave the array.
                self.at += 1;
              }
            }
          }
        }
      }
    }
    self.depth -= 1;
    (
      Json::Array(values),
      Node {
        span: start..self.at,
        key_span: None,
        kind: NodeKind::Array(nodes),
      },
    )
  }

  /// Reads a string from its opening quote; returns its value.
  fn string(&mut self) -> String {
    let start = self.at;
    self.at += 1;
    let mut value = String::new();
    while let Some(rest) = self.text.get(self.at..) {
      let Some(c) = rest.chars().next() else {
        self.error(start..start + 1, "Unterminated string");
        return value;
      };
      match c {
        '"' => {
          self.at += 1;
          return value;
        }
        '\n' => {
          self.error(start..self.at, "Unterminated string");
          return value;
        }
        '\\' => {
          let escape_start = self.at;
          self.at += 1;
          match self.peek() {
            Some(b'"') => value.push('"'),
            Some(b'\\') => value.push('\\'),
            Some(b'/') => value.push('/'),
            Some(b'b') => value.push('\u{8}'),
            Some(b'f') => value.push('\u{c}'),
            Some(b'n') => value.push('\n'),
            Some(b'r') => value.push('\r'),
            Some(b't') => value.push('\t'),
            Some(b'u') => {
              let unit = |at: usize| {
                self
                  .text
                  .get(at..at + 4)
                  .and_then(|hex| u16::from_str_radix(hex, 16).ok())
              };
              match unit(self.at + 1) {
                Some(high) if (0xD800..0xDC00).contains(&high) => {
                  let low = (self.text.get(self.at + 5..self.at + 7) == Some("\\u"))
                    .then(|| unit(self.at + 7))
                    .flatten()
                    .filter(|low| (0xDC00..0xE000).contains(low));
                  match low {
                    Some(low) => {
                      let code =
                        0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                      value.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                      self.at += 6;
                    }
                    None => value.push('\u{fffd}'),
                  }
                  self.at += 4;
                }
                Some(unit) => {
                  value.push(char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'));
                  self.at += 4;
                }
                None => self.error(escape_start..self.at + 1, "Invalid unicode escape"),
              }
            }
            _ => self.error(escape_start..self.at + 1, "Invalid escape"),
          }
          if self.at < self.bytes.len() {
            self.at += 1;
          }
          while !self.text.is_char_boundary(self.at) {
            self.at += 1;
          }
        }
        c if u32::from(c) < 0x20 => {
          let at = self.at;
          self.error(at..at + 1, "Control characters must be escaped");
          self.at += 1;
        }
        c => {
          value.push(c);
          self.at += c.len_utf8();
        }
      }
    }
    value
  }

  fn number(&mut self) -> (Json, Node) {
    let start = self.at;
    while self
      .peek()
      .is_some_and(|b| matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
    {
      self.at += 1;
    }
    let raw = &self.text[start..self.at];
    let valid = is_json_number(raw);
    let value = valid
      .then(|| serde_json::from_str::<Number>(raw).ok())
      .flatten();
    match value {
      Some(number) => {
        let value = Json::Number(number);
        let node = Node::scalar(start..self.at, Scalar::from_json(&value));
        (value, node)
      }
      None => {
        self.error(start..self.at, "Invalid number");
        (Json::Null, Node::scalar(start..self.at, None))
      }
    }
  }
}

fn is_json_number(raw: &str) -> bool {
  let bytes = raw.as_bytes();
  let mut i = 0;
  if bytes.get(i) == Some(&b'-') {
    i += 1;
  }
  match bytes.get(i) {
    Some(b'0') => i += 1,
    Some(b'1'..=b'9') => {
      while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
      }
    }
    _ => return false,
  }
  if bytes.get(i) == Some(&b'.') {
    i += 1;
    if !bytes.get(i).is_some_and(u8::is_ascii_digit) {
      return false;
    }
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
      i += 1;
    }
  }
  if matches!(bytes.get(i), Some(b'e' | b'E')) {
    i += 1;
    if matches!(bytes.get(i), Some(b'+' | b'-')) {
      i += 1;
    }
    if !bytes.get(i).is_some_and(u8::is_ascii_digit) {
      return false;
    }
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
      i += 1;
    }
  }
  i == bytes.len()
}
#[cfg(test)]
mod tests {
  use flareite_extension_schemas::document::Segment;

  use super::*;

  /// Parses a JSON pointer (`/a/0`): numbers are indexes.
  fn pointer(pointer: &str) -> Vec<Segment> {
    pointer
      .trim_start_matches('/')
      .split('/')
      .map(|part| match part.parse::<usize>() {
        Ok(index) => Segment::Index(index),
        Err(_) => Segment::key(part),
      })
      .collect()
  }

  #[test]
  fn json_with_comments_and_trailing_commas_parses() {
    let text = "// settings\n{\n  \"a\": [1, 2,],\n  /* b */ \"b\": {\"c\": \"\\u00e9\\ud83d\\ude00\"},\n}\n";
    let document = parse_json(text);
    assert!(document.errors.is_empty(), "{:?}", document.errors);
    let value = document.value.expect("value");
    assert_eq!(value["a"][1], 2);
    assert_eq!(value["b"]["c"], "é😀");
    let (c, _) = document.tree.find(&pointer("/b/c"));
    assert_eq!(&text[c.key_span.clone().expect("key")], "\"c\"");
    assert_eq!(c.value(), Some(&Scalar::String("é😀".into())));
    let (two, _) = document.tree.find(&pointer("/a/1"));
    assert_eq!(&text[two.span.clone()], "2");
    assert_eq!(two.value(), Some(&Scalar::Integer(2)));
  }

  #[test]
  fn json_errors_are_reported_and_recovered_from() {
    let document = parse_json("{\"a\": , \"b\": tru, \"c\": 1 \"d\": 2}");
    assert!(document.value.is_none());
    assert!(document.errors.len() >= 2, "{:?}", document.errors);
    assert!(document.tree.get("d").is_some(), "{:?}", document.tree);
    // A word that isn't a value has none.
    assert_eq!(document.tree.get("b").and_then(Node::value), None);
    let document = parse_json("{\"a\": 1, \"a\": 2}");
    assert_eq!(document.duplicates, vec![9..12]);
    assert_eq!(document.value.expect("value")["a"], 2);
    assert!(!parse_json("").errors.is_empty());
    assert!(!parse_json("[01]").errors.is_empty());
    assert!(!parse_json("[\"a\n\"]").errors.is_empty());
    assert!(parse_json("[1e5, -0.5, 0]").errors.is_empty());
  }

  #[test]
  fn documents_name_their_schemas_and_warn_of_duplicates() {
    let document = parse("{ \"$schema\": \"https://x/s.json\", \"a\": 1, \"a\": 2 }");
    assert_eq!(document.named_schema.as_deref(), Some("https://x/s.json"));
    assert_eq!(document.problems.len(), 1);
    assert_eq!(document.problems[0].severity, Severity::Warning);
    assert!(!document.has_errors());
    assert_eq!(parse("[{ \"$schema\": \"x\" }]").named_schema, None);
  }
}
