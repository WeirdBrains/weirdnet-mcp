//! Tool results and resources: [`CallToolResult`], [`Content`], [`Resource`], [`ResourceTemplate`]
//! and [`ResourceContents`].
//!
//! Images and audio are base64 `data` plus a `mimeType`, as MCP sends them. They convert to and
//! from `data:` URLs ([`Content::from_data_url`], [`Content::data_url`]) and audio formats
//! ([`Content::audio_format`], [`Content::format`]), which is how weirdnet-agent 0.5 `ContentPart`s
//! carry them (`ContentPart::image(url)`, `ContentPart::audio(data, format)`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

/// One item of a tool result's `content`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Content {
    /// `{"type": "text", "text": ...}`
    Text(String),
    /// `{"type": "image", "data": <base64>, "mimeType": ...}`
    Image {
        /// Base64 data.
        data: String,
        /// For example `image/png`.
        mime_type: String,
    },
    /// `{"type": "audio", "data": <base64>, "mimeType": ...}`
    Audio {
        /// Base64 data.
        data: String,
        /// For example `audio/wav`.
        mime_type: String,
    },
    /// `{"type": "resource_link", "uri": ..., "name": ..., ...}`: a resource the client can read.
    ResourceLink(Resource),
    /// `{"type": "resource", "resource": {...}}`: an embedded resource.
    Resource(ResourceContents),
}

impl Content {
    /// Text.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// An image from base64 `data`.
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Image { data: data.into(), mime_type: mime_type.into() }
    }

    /// An image from raw bytes (base64-encoded here).
    pub fn image_bytes(bytes: &[u8], mime_type: impl Into<String>) -> Self {
        Self::image(STANDARD.encode(bytes), mime_type)
    }

    /// Audio from base64 `data`.
    pub fn audio(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Audio { data: data.into(), mime_type: mime_type.into() }
    }

    /// Audio from raw bytes (base64-encoded here).
    pub fn audio_bytes(bytes: &[u8], mime_type: impl Into<String>) -> Self {
        Self::audio(STANDARD.encode(bytes), mime_type)
    }

    /// Audio from base64 `data` and a short format name as OpenAI-style `input_audio` parts use
    /// (`wav`, `mp3`, `flac`, `ogg`, `webm`, `m4a`); other names become `audio/<format>`.
    pub fn audio_format(data: impl Into<String>, format: &str) -> Self {
        let mime = match format.to_ascii_lowercase().as_str() {
            "mp3" | "mpeg" => "audio/mpeg".to_string(),
            "m4a" | "mp4" => "audio/mp4".to_string(),
            other => format!("audio/{other}"),
        };
        Self::audio(data, mime)
    }

    /// An image or audio item from a base64 `data:` URL (`data:image/png;base64,...`), as
    /// weirdnet-agent image parts carry them. `None` for anything else.
    pub fn from_data_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("data:")?;
        let (mime, data) = rest.split_once(";base64,")?;
        if data.is_empty() || mime.contains(',') {
            return None;
        }
        if mime.starts_with("image/") {
            Some(Self::image(data, mime))
        } else if mime.starts_with("audio/") {
            Some(Self::audio(data, mime))
        } else {
            None
        }
    }

    /// A `data:` URL for an image or audio item.
    pub fn data_url(&self) -> Option<String> {
        match self {
            Self::Image { data, mime_type } | Self::Audio { data, mime_type } => Some(format!("data:{mime_type};base64,{data}")),
            _ => None,
        }
    }

    /// The short audio format name for an audio item (`audio/mpeg` is `mp3`, `audio/wav` and
    /// `audio/x-wav` are `wav`, otherwise the MIME subtype).
    pub fn format(&self) -> Option<String> {
        let Self::Audio { mime_type, .. } = self else { return None };
        let sub = mime_type.split_once('/').map_or(mime_type.as_str(), |(_, s)| s);
        let sub = sub.split(';').next().unwrap_or(sub).trim();
        Some(
            match sub {
                "mpeg" | "mp3" => "mp3",
                "wav" | "x-wav" | "wave" | "vnd.wave" => "wav",
                "mp4" | "x-m4a" => "m4a",
                other => other,
            }
            .to_string(),
        )
    }

    /// The MCP JSON for this item.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Text(text) => json!({"type": "text", "text": text}),
            Self::Image { data, mime_type } => json!({"type": "image", "data": data, "mimeType": mime_type}),
            Self::Audio { data, mime_type } => json!({"type": "audio", "data": data, "mimeType": mime_type}),
            Self::ResourceLink(r) => {
                let mut v = r.to_json();
                if let Some(map) = v.as_object_mut() {
                    map.insert("type".into(), json!("resource_link"));
                }
                v
            }
            Self::Resource(c) => json!({"type": "resource", "resource": c.to_json()}),
        }
    }
}

/// A tool result: content items, optional `structuredContent`, and `isError`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CallToolResult {
    /// The `content` items.
    pub content: Vec<Content>,
    /// `structuredContent`.
    pub structured: Option<Value>,
    /// `isError`.
    pub is_error: bool,
}

impl CallToolResult {
    /// A result with these content items.
    pub fn new(content: Vec<Content>) -> Self {
        Self { content, structured: None, is_error: false }
    }

    /// A JSON value as pretty text plus `structuredContent` (non-objects are wrapped as
    /// `{"result": v}`). This is what a sync [`Tools::call`](crate::Tools::call) result becomes.
    pub fn json(v: &Value) -> Self {
        Self {
            content: vec![Content::Text(serde_json::to_string_pretty(v).unwrap_or_default())],
            structured: Some(if v.is_object() { v.clone() } else { json!({"result": v}) }),
            is_error: false,
        }
    }

    /// A failed result with this text (shown to the model; not a JSON-RPC error).
    pub fn error(text: impl Into<String>) -> Self {
        Self { content: vec![Content::Text(text.into())], structured: None, is_error: true }
    }

    /// Adds `structuredContent`.
    pub fn with_structured(mut self, v: Value) -> Self {
        self.structured = Some(v);
        self
    }

    /// Adds a content item.
    pub fn with(mut self, item: Content) -> Self {
        self.content.push(item);
        self
    }

    /// The MCP JSON for this result.
    pub fn to_json(&self) -> Value {
        let content: Vec<Value> = self.content.iter().map(Content::to_json).collect();
        match &self.structured {
            Some(s) => json!({"content": content, "structuredContent": s, "isError": self.is_error}),
            None => json!({"content": content, "isError": self.is_error}),
        }
    }
}

impl From<Value> for CallToolResult {
    fn from(v: Value) -> Self {
        Self::json(&v)
    }
}

impl From<Content> for CallToolResult {
    fn from(c: Content) -> Self {
        Self::new(vec![c])
    }
}

fn put(map: &mut Map<String, Value>, key: &str, v: &Option<String>) {
    if let Some(v) = v {
        map.insert(key.into(), json!(v));
    }
}

/// A resource in `resources/list` (and a `resource_link` in tool results).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Resource {
    /// URI, for example `ymir://models/qwen3-8b`.
    pub uri: String,
    /// Name.
    pub name: String,
    /// Display title.
    pub title: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// MIME type.
    pub mime_type: Option<String>,
    /// Size in bytes.
    pub size: Option<u64>,
}

impl Resource {
    /// A resource.
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self { uri: uri.into(), name: name.into(), title: None, description: None, mime_type: None, size: None }
    }

    /// Sets the title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the MIME type.
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// Sets the size in bytes.
    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    /// The MCP JSON.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("uri".into(), json!(self.uri));
        m.insert("name".into(), json!(self.name));
        put(&mut m, "title", &self.title);
        put(&mut m, "description", &self.description);
        put(&mut m, "mimeType", &self.mime_type);
        if let Some(size) = self.size {
            m.insert("size".into(), json!(size));
        }
        Value::Object(m)
    }
}

/// A parameterized resource in `resources/templates/list` (an RFC 6570 URI template).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceTemplate {
    /// URI template, for example `ymir://models/{name}`.
    pub uri_template: String,
    /// Name.
    pub name: String,
    /// Display title.
    pub title: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// MIME type of the resources it names.
    pub mime_type: Option<String>,
}

impl ResourceTemplate {
    /// A template.
    pub fn new(uri_template: impl Into<String>, name: impl Into<String>) -> Self {
        Self { uri_template: uri_template.into(), name: name.into(), title: None, description: None, mime_type: None }
    }

    /// Sets the title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the MIME type.
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// The MCP JSON.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("uriTemplate".into(), json!(self.uri_template));
        m.insert("name".into(), json!(self.name));
        put(&mut m, "title", &self.title);
        put(&mut m, "description", &self.description);
        put(&mut m, "mimeType", &self.mime_type);
        Value::Object(m)
    }
}

/// The body of [`ResourceContents`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceBody {
    /// `text`
    Text(String),
    /// `blob` (base64)
    Blob(String),
}

/// One item of a `resources/read` result (or an embedded resource).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceContents {
    /// URI.
    pub uri: String,
    /// MIME type.
    pub mime_type: Option<String>,
    /// Text or base64 blob.
    pub body: ResourceBody,
}

impl ResourceContents {
    /// Text contents.
    pub fn text(uri: impl Into<String>, mime_type: Option<&str>, text: impl Into<String>) -> Self {
        Self { uri: uri.into(), mime_type: mime_type.map(String::from), body: ResourceBody::Text(text.into()) }
    }

    /// JSON contents, pretty-printed, with MIME type `application/json`.
    pub fn json(uri: impl Into<String>, v: &Value) -> Self {
        Self::text(uri, Some("application/json"), serde_json::to_string_pretty(v).unwrap_or_default())
    }

    /// Binary contents from base64.
    pub fn blob(uri: impl Into<String>, mime_type: Option<&str>, base64: impl Into<String>) -> Self {
        Self { uri: uri.into(), mime_type: mime_type.map(String::from), body: ResourceBody::Blob(base64.into()) }
    }

    /// Binary contents from raw bytes (base64-encoded here).
    pub fn bytes(uri: impl Into<String>, mime_type: Option<&str>, bytes: &[u8]) -> Self {
        Self::blob(uri, mime_type, STANDARD.encode(bytes))
    }

    /// The MCP JSON.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("uri".into(), json!(self.uri));
        put(&mut m, "mimeType", &self.mime_type);
        match &self.body {
            ResourceBody::Text(t) => m.insert("text".into(), json!(t)),
            ResourceBody::Blob(b) => m.insert("blob".into(), json!(b)),
        };
        Value::Object(m)
    }
}

/// Why a resource could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceError {
    /// No such resource (`-32602` on modern requests, `-32002` on legacy ones, with `data.uri`).
    NotFound,
    /// Another failure, reported as an internal error with this message.
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for ResourceError {
    fn from(e: E) -> Self {
        Self::Failed(e.to_string())
    }
}
