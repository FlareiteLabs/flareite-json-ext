//! Where the cursor is in a JSON document being typed, which may not
//! parse: typing a key (of which object) or a value (at which path). The
//! text is read from the start up to the cursor by a small lexer that
//! tolerates anything unfinished.

use flareite_extension_schemas::document::Segment;
use flareite_extension_schemas::{Header, Place};

/// Longest document read for context.
const MAX_SCAN: usize = 8 * 1024 * 1024;

/// The span of a key segment starting at `start`: a quoted string, or a
/// bare key.
fn key_end(text: &str, start: usize, json: bool) -> usize {
  let bytes = text.as_bytes();
  match bytes.get(start) {
    Some(&quote @ (b'"' | b'\'')) => {
      let mut i = start + 1;
      while i < bytes.len() && bytes[i] != b'\n' {
        if bytes[i] == b'\\' && quote == b'"' {
          i += 2;
          continue;
        }
        if bytes[i] == quote {
          return i + 1;
        }
        i += 1;
      }
      i.min(bytes.len())
    }
    _ if json => start,
    _ => {
      let mut i = start;
      while i < bytes.len() && is_bare(bytes[i]) {
        i += 1;
      }
      i
    }
  }
}

fn is_bare(b: u8) -> bool {
  b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// What's being typed at `offset` in JSON `text`.
pub fn json_place(text: &str, offset: usize) -> Place {
  let offset = offset.min(text.len()).min(MAX_SCAN);
  let bytes = text.as_bytes();
  enum Expect {
    Key,
    Colon,
    Value,
    Comma,
  }
  enum Frame {
    Object {
      path: Vec<Segment>,
      key: Option<String>,
      expect: Expect,
    },
    Array {
      path: Vec<Segment>,
      index: usize,
      expect: Expect,
    },
  }
  let mut frames: Vec<Frame> = Vec::new();
  let mut at = 0;
  let mut done = false;
  let value_path = |frames: &[Frame]| -> Option<Vec<Segment>> {
    match frames.last() {
      None => Some(Vec::new()),
      Some(Frame::Object {
        path,
        key,
        expect: Expect::Value,
      }) => {
        let mut path = path.clone();
        path.push(Segment::Key(key.clone()?));
        Some(path)
      }
      Some(Frame::Array {
        path,
        index,
        expect: Expect::Value,
      }) => {
        let mut path = path.clone();
        path.push(Segment::Index(*index));
        Some(path)
      }
      _ => None,
    }
  };
  let here = |frames: &[Frame], start: usize, end: usize| -> Place {
    match frames.last() {
      Some(Frame::Object {
        path,
        expect: Expect::Key,
        ..
      }) => Place::Key {
        table: path.clone(),
        start,
        end,
        header: Header::None,
      },
      _ => match value_path(frames) {
        Some(path) => Place::Value { path, start, end },
        None => Place::Nothing,
      },
    }
  };
  loop {
    // Whitespace and comments.
    loop {
      match bytes.get(at) {
        Some(b' ' | b'\t' | b'\n' | b'\r') if at < offset => at += 1,
        Some(b'/') if bytes.get(at + 1) == Some(&b'/') && at < offset => {
          while bytes.get(at).is_some_and(|&b| b != b'\n') {
            at += 1;
          }
          if at >= offset {
            return Place::Nothing;
          }
        }
        Some(b'/') if bytes.get(at + 1) == Some(&b'*') && at < offset => {
          match text[at + 2..].find("*/") {
            Some(end) => at += 2 + end + 2,
            None => return Place::Nothing,
          }
          if at > offset {
            return Place::Nothing;
          }
        }
        _ => break,
      }
    }
    if at >= offset {
      if done {
        return Place::Nothing;
      }
      return here(&frames, offset, offset);
    }
    let start = at;
    match bytes[at] {
      b'{' => {
        let path = value_path(&frames).unwrap_or_default();
        at += 1;
        frames.push(Frame::Object {
          path,
          key: None,
          expect: Expect::Key,
        });
        continue;
      }
      b'[' => {
        let path = value_path(&frames).unwrap_or_default();
        at += 1;
        frames.push(Frame::Array {
          path,
          index: 0,
          expect: Expect::Value,
        });
        continue;
      }
      b'}' | b']' => {
        at += 1;
        frames.pop();
        if frames.is_empty() {
          done = true;
        }
      }
      b':' => {
        at += 1;
        if let Some(Frame::Object { expect, .. }) = frames.last_mut() {
          *expect = Expect::Value;
        }
        continue;
      }
      b',' => {
        at += 1;
        match frames.last_mut() {
          Some(Frame::Object { expect, key, .. }) => {
            *expect = Expect::Key;
            *key = None;
          }
          Some(Frame::Array { expect, index, .. }) => {
            *expect = Expect::Value;
            *index += 1;
          }
          None => {}
        }
        continue;
      }
      b'"' => {
        let end = key_end(text, at, true);
        if offset > start && offset < end.max(start + 1)
          || (offset == end && !text[start..end].ends_with('"') && end > start)
        {
          return here(&frames, start, end);
        }
        let raw = &text[start..end];
        if let Some(Frame::Object {
          expect: expect @ Expect::Key,
          key,
          ..
        }) = frames.last_mut()
        {
          *key = Some(
            serde_json::from_str::<String>(raw)
              .unwrap_or_else(|_| raw.trim_matches('"').to_owned()),
          );
          *expect = Expect::Colon;
          at = end;
          continue;
        }
        at = end;
      }
      _ => {
        let mut end = at;
        while end < bytes.len()
          && !matches!(
            bytes[end],
            b',' | b']' | b'}' | b':' | b' ' | b'\t' | b'\n' | b'\r' | b'"'
          )
        {
          end += 1;
        }
        if offset <= end {
          return here(&frames, start, end);
        }
        at = end.max(at + 1);
        while !text.is_char_boundary(at) {
          at += 1;
        }
      }
    }
    // After a value.
    match frames.last_mut() {
      Some(Frame::Object { expect, .. } | Frame::Array { expect, .. }) => *expect = Expect::Comma,
      None => done = true,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn at(text: &str) -> (String, usize) {
    let offset = text.find('|').expect("cursor");
    (text.replacen('|', "", 1), offset)
  }

  fn json(text: &str) -> Place {
    let (text, offset) = at(text);
    json_place(&text, offset)
  }

  fn keys(path: &[&str]) -> Vec<Segment> {
    path.iter().map(|key| Segment::key(*key)).collect()
  }

  #[test]
  fn json_places() {
    assert!(matches!(json("|"), Place::Value { path, .. } if path.is_empty()));
    assert!(matches!(json("{|"), Place::Key { table, .. } if table.is_empty()));
    assert!(matches!(
      json("{ \"a\": { \"b|"),
      Place::Key { table, start: 9, .. } if table == keys(&["a"])
    ));
    assert!(matches!(
      json("{ \"a\": |"),
      Place::Value { path, .. } if path == keys(&["a"])
    ));
    assert!(matches!(
      json("{ \"a\": \"x|\" }"),
      Place::Value { path, start: 7, end: 10 } if path == keys(&["a"])
    ));
    assert!(matches!(
      json("{ \"a\": [1, |"),
      Place::Value { path, .. } if path == vec![Segment::key("a"), Segment::Index(1)]
    ));
    assert!(matches!(
      json("{ \"a\": 1, |"),
      Place::Key { table, .. } if table.is_empty()
    ));
    assert!(matches!(
      json("{ \"a\": tr|"),
      Place::Value { path, start: 7, .. } if path == keys(&["a"])
    ));
    assert_eq!(json("{ // note |\n}"), Place::Nothing);
    assert_eq!(json("{ \"a\": 1 |"), Place::Nothing);
  }
}
