//! Prompts: [`PromptSpec`] / [`PromptArg`] tables for `prompts/list`, and [`GetPromptResult`] /
//! [`PromptMessage`] for `prompts/get`.
//!
//! Prompts are templates the user picks (slash commands and the like). The app lists them with
//! [`Tools::prompts`](crate::Tools::prompts) and fills them in with
//! [`Tools::get_prompt`](crate::Tools::get_prompt). The server advertises the `prompts`
//! capability only when the table is not empty, and checks the prompt name and its required
//! arguments before calling the app.

use serde_json::{Value, json};

use crate::Content;

/// One prompt argument. Arguments are strings on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptArg {
    /// Name.
    pub name: &'static str,
    /// Description (omitted from the listing when empty).
    pub description: &'static str,
    /// Required.
    pub required: bool,
}

impl PromptArg {
    /// A required argument.
    pub const fn required(name: &'static str, description: &'static str) -> Self {
        Self { name, description, required: true }
    }

    /// An optional argument.
    pub const fn optional(name: &'static str, description: &'static str) -> Self {
        Self { name, description, required: false }
    }
}

/// A prompt, for `prompts/list`.
///
/// ```
/// use weirdnet_mcp::{PromptArg, PromptSpec};
///
/// const PROMPTS: &[PromptSpec] = &[
///     PromptSpec::new("review", "Reviews a piece of code.", &[PromptArg::required("code", "The code to review")])
///         .with_title("Code review"),
/// ];
/// assert!(PromptSpec::check(PROMPTS).is_ok());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptSpec {
    /// Name, unique in the table.
    pub name: &'static str,
    /// Display name (omitted when empty).
    pub title: &'static str,
    /// Description (omitted when empty).
    pub description: &'static str,
    /// Arguments.
    pub arguments: &'static [PromptArg],
}

impl PromptSpec {
    /// A prompt without a title.
    pub const fn new(name: &'static str, description: &'static str, arguments: &'static [PromptArg]) -> Self {
        Self { name, title: "", description, arguments }
    }

    /// The same prompt with a display name.
    pub const fn with_title(mut self, title: &'static str) -> Self {
        self.title = title;
        self
    }

    /// The `prompts/list` entry.
    pub fn to_json(&self) -> Value {
        let mut v = json!({"name": self.name});
        if !self.title.is_empty() {
            v["title"] = json!(self.title);
        }
        if !self.description.is_empty() {
            v["description"] = json!(self.description);
        }
        if !self.arguments.is_empty() {
            v["arguments"] = Value::Array(
                self.arguments
                    .iter()
                    .map(|a| {
                        let mut arg = json!({"name": a.name, "required": a.required});
                        if !a.description.is_empty() {
                            arg["description"] = json!(a.description);
                        }
                        arg
                    })
                    .collect(),
            );
        }
        v
    }

    /// Problems with a prompt table, for a unit test: empty or duplicate prompt and argument names.
    pub fn check(specs: &[PromptSpec]) -> Result<(), Vec<String>> {
        let mut problems = vec![];
        for (i, p) in specs.iter().enumerate() {
            if p.name.is_empty() {
                problems.push("a prompt has no name".to_string());
            }
            if specs[..i].iter().any(|q| q.name == p.name) {
                problems.push(format!("{}: duplicate prompt", p.name));
            }
            for (j, a) in p.arguments.iter().enumerate() {
                if a.name.is_empty() {
                    problems.push(format!("{}: an argument has no name", p.name));
                }
                if p.arguments[..j].iter().any(|b| b.name == a.name) {
                    problems.push(format!("{}.{}: duplicate argument", p.name, a.name));
                }
            }
        }
        if problems.is_empty() { Ok(()) } else { Err(problems) }
    }

    /// Checks `prompts/get` arguments against this prompt: an object (or absent) whose values are
    /// strings, with every required argument present. Unknown arguments are passed through.
    pub fn check_arguments(&self, args: &Value) -> Result<(), String> {
        let empty = serde_json::Map::new();
        let map = match args {
            Value::Null => &empty,
            Value::Object(m) => m,
            _ => return Err("prompt arguments must be an object of strings".into()),
        };
        if let Some((k, _)) = map.iter().find(|(_, v)| !v.is_string()) {
            return Err(format!("prompt argument \"{k}\" must be a string"));
        }
        if let Some(a) = self.arguments.iter().find(|a| a.required && !map.contains_key(a.name)) {
            return Err(format!("missing required argument \"{}\"", a.name));
        }
        Ok(())
    }
}

/// Who speaks a [`PromptMessage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// `user`
    User,
    /// `assistant`
    Assistant,
}

impl Role {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One message of a filled-in prompt: a role and one content block (text, image, audio, a
/// resource link or an embedded resource).
#[derive(Clone, Debug, PartialEq)]
pub struct PromptMessage {
    /// Who speaks.
    pub role: Role,
    /// What is said.
    pub content: Content,
}

impl PromptMessage {
    /// A user message.
    pub fn user(content: impl Into<Content>) -> Self {
        Self { role: Role::User, content: content.into() }
    }

    /// An assistant message.
    pub fn assistant(content: impl Into<Content>) -> Self {
        Self { role: Role::Assistant, content: content.into() }
    }

    /// `{"role": ..., "content": {...}}`
    pub fn to_json(&self) -> Value {
        json!({"role": self.role.as_str(), "content": self.content.to_json()})
    }
}

impl From<&str> for Content {
    fn from(text: &str) -> Self {
        Content::text(text)
    }
}

impl From<String> for Content {
    fn from(text: String) -> Self {
        Content::text(text)
    }
}

/// The result of `prompts/get`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GetPromptResult {
    /// Optional description of the filled-in prompt.
    pub description: Option<String>,
    /// The messages.
    pub messages: Vec<PromptMessage>,
}

impl GetPromptResult {
    /// A result with these messages.
    pub fn new(messages: Vec<PromptMessage>) -> Self {
        Self { description: None, messages }
    }

    /// The same result with a description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The same result with one more message.
    pub fn with(mut self, message: PromptMessage) -> Self {
        self.messages.push(message);
        self
    }

    /// `{"description"?: ..., "messages": [...]}`
    pub fn to_json(&self) -> Value {
        let mut v = json!({"messages": self.messages.iter().map(PromptMessage::to_json).collect::<Vec<_>>()});
        if let Some(d) = &self.description {
            v["description"] = json!(d);
        }
        v
    }
}

/// Why a prompt could not be filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PromptError {
    /// No such prompt for this caller (`-32602`).
    NotFound,
    /// The arguments do not check out (`-32602`, with this message).
    InvalidArguments(String),
    /// Another failure, reported as an internal error (`-32603`) with this message.
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for PromptError {
    fn from(e: E) -> Self {
        Self::Failed(e.to_string())
    }
}
