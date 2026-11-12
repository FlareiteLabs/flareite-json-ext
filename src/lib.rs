//! JSON for Flareite: JSON documents (with comments and trailing commas, as
//! JSONC allows) parsed for Flareite's schemas, which check them, and their
//! keys and values completed and described from what the schemas say.

mod parse;
mod place;

use flareite_extension_api::events::Event;
use flareite_extension_api::language_features::{CompletionItem, CompletionRequest, HoverRequest};
use flareite_extension_api::types::Activation;
use flareite_extension_api::{Extension, register};
use flareite_extension_schemas::document::Document;
use flareite_extension_schemas::{Documents, Header, Language, Place, Property};
use serde_json::Value as Json;

/// JSON, as Flareite's schemas see it.
pub struct JsonLanguage;

impl Language for JsonLanguage {
  fn languages(&self) -> &[&str] {
    &["json", "jsonc"]
  }

  fn parse(&self, text: &str) -> Document {
    parse::parse(text)
  }

  fn place(&self, text: &str, offset: usize) -> Place {
    place::json_place(text, offset)
  }

  fn decode_key(&self, raw: &str) -> String {
    // A key being typed may lack its closing quote.
    serde_json::from_str::<String>(raw)
      .or_else(|_| serde_json::from_str::<String>(&format!("{raw}\"")))
      .unwrap_or_else(|_| raw.trim_matches('"').to_owned())
  }

  fn key(&self, name: &str) -> String {
    serde_json::to_string(name).unwrap_or_default()
  }

  fn insert_property(
    &self,
    key: &str,
    _property: &Property,
    _header: Header,
    _text: &str,
    _start: usize,
  ) -> String {
    format!("{key}: ")
  }

  fn render(&self, value: &Json) -> String {
    serde_json::to_string(value).unwrap_or_default()
  }
}

/// The extension: its open documents, checked against their schemas.
struct JsonLanguageExtension(Documents<JsonLanguage>);

impl Extension for JsonLanguageExtension {
  fn activate(_: Activation) -> Result<Self, String> {
    Ok(Self(Documents::new(JsonLanguage)))
  }

  fn handle_event(&mut self, event: Event) -> Result<(), String> {
    self.0.handle_event(event);
    Ok(())
  }

  fn provide_completions(
    &mut self,
    request: CompletionRequest,
  ) -> Result<Vec<CompletionItem>, String> {
    Ok(self.0.completions(request.document, request.position))
  }

  fn provide_hover(&mut self, request: HoverRequest) -> Result<Option<String>, String> {
    Ok(self.0.hover(request.document, request.position))
  }
}

register!(JsonLanguageExtension);

#[cfg(test)]
mod tests {
  use flareite_extension_schemas::testing;
  use serde_json::json;

  use super::*;

  #[test]
  fn documents_are_checked_where_problems_are() {
    let schema = testing::json_schema(json!({
      "type": "object",
      "properties": { "port": { "type": "integer" } },
      "additionalProperties": false
    }));
    let text = "{ \"port\": 1, \"zzz\": 2 }";
    let problems = testing::check(&JsonLanguage, text, &[&schema]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(&text[problems[0].range.clone()], "\"zzz\"");
    // Duplicate keys are warnings, and don't stop checking.
    let text = "{ \"port\": 1, \"port\": \"x\" }";
    let problems = testing::check(&JsonLanguage, text, &[&schema]);
    assert_eq!(problems.len(), 2, "{problems:?}");
    // TOML Schemas check JSON too.
    let tosd = testing::toml_schema(
      "[toml-schema]\nversion = \"1.0.0\"\n[elements.port]\ntype = \"integer\"\n",
    );
    let text = "{ \"port\": \"x\" }";
    let problems = testing::check(&JsonLanguage, text, &[&tosd]);
    assert_eq!(&text[problems[0].range.clone()], "\"x\"", "{problems:?}");
  }

  #[test]
  fn completes_keys_and_values() {
    let schema = testing::json_schema(json!({ "properties": { "a b": { "enum": [1, 2] } } }));
    let complete = |text: &str| {
      let offset = text.find('|').expect("cursor");
      let text = text.replacen('|', "", 1);
      testing::complete(&JsonLanguage, &text, offset, &[&schema])
    };
    let keys = complete("{ \"a|");
    assert_eq!(keys[0].insert, "\"a b\": ");
    assert_eq!(keys[0].start, 2);
    assert_eq!(keys[0].filter, "\"a b\"");
    let values = complete("{ \"a b\": |");
    assert_eq!(values.len(), 2);
  }

  /// Completion follows an `if` on a sibling's value to its branch, while
  /// the document is still being written.
  #[test]
  fn completion_follows_conditions_on_the_document() {
    let props = |names: &[&str]| {
      let properties: serde_json::Map<String, Json> = names
        .iter()
        .map(|name| ((*name).to_owned(), json!({ "type": "number" })))
        .collect();
      json!({ "properties": { "props": { "properties": properties } } })
    };
    let schema = testing::json_schema(json!({
      "type": "object",
      "properties": { "class": { "enum": ["Part", "Model"] } },
      "allOf": [
        { "if": { "properties": { "class": { "const": "Part" } }, "required": ["class"] },
          "then": props(&["Size", "Color"]) },
        { "if": { "properties": { "class": { "const": "Model" } }, "required": ["class"] },
          "then": props(&["Scale"]),
          "else": props(&["Name"]) }
      ]
    }));
    let labels = |text: &str| -> Vec<String> {
      let offset = text.find('|').expect("cursor");
      let text = text.replacen('|', "", 1);
      let mut labels: Vec<String> = testing::complete(&JsonLanguage, &text, offset, &[&schema])
        .into_iter()
        .map(|item| item.label)
        .collect();
      labels.sort();
      labels
    };
    // Unfinished: the objects aren't closed, the key is being typed (the
    // editor closes the quote).
    assert_eq!(
      labels("{ \"class\": \"Part\", \"props\": { \"|\""),
      ["Color", "Name", "Size"]
    );
    assert_eq!(labels("{ \"class\": \"Model\", \"props\": { |"), ["Scale"]);
    // Without a class, neither `if` holds (both require it): the `else`.
    assert_eq!(labels("{ \"props\": { |"), ["Name"]);
    // A class that isn't a value yet can't be told: both branches.
    assert_eq!(
      labels("{ \"class\": Par, \"props\": { |").len(),
      4,
      "Size, Color, Scale, Name"
    );
  }

  #[test]
  fn hovers_describe_keys() {
    let schema = testing::json_schema(json!({
      "properties": { "port": { "type": "integer", "description": "Where it listens." } }
    }));
    let text = "{ \"port\": 80 }";
    let (markdown, span) = testing::hover(&JsonLanguage, text, 4, &[&schema]).expect("hover");
    assert_eq!(&text[span], "\"port\"");
    assert!(markdown.starts_with("Where it listens."), "{markdown}");
  }
}
