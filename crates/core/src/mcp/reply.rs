//! The typed answer of one tool call: what a terminal returns and every
//! request behavior passes along ([`TerminalDispatch`](super::TerminalDispatch),
//! [`Next::run`](super::Next::run), [`RequestBehavior::handle`](super::RequestBehavior::handle)).

use base64::Engine as _;

/// One content block of a [`ToolReply`].
///
/// Proxima's own type, so the public API does not move when the MCP SDK
/// does; the transport maps each variant onto the MCP content block of the
/// same name. `data` and `blob` are base64 (standard alphabet, padded), as
/// MCP carries binary content; [`Self::image`] and [`Self::audio`] encode
/// raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolContent {
    /// A `text` block.
    Text(String),
    /// An `image` block.
    Image { data: String, mime_type: String },
    /// An `audio` block.
    Audio { data: String, mime_type: String },
    /// A `resource_link` block: a reference the client may fetch.
    ResourceLink {
        uri: String,
        name: String,
        description: Option<String>,
        mime_type: Option<String>,
    },
    /// An embedded `resource` block with text contents.
    TextResource {
        uri: String,
        mime_type: Option<String>,
        text: String,
    },
    /// An embedded `resource` block with binary contents.
    BlobResource {
        uri: String,
        mime_type: Option<String>,
        blob: String,
    },
}

impl ToolContent {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// An `image` block from raw bytes.
    #[must_use]
    pub fn image(bytes: impl AsRef<[u8]>, mime_type: impl Into<String>) -> Self {
        Self::Image {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: mime_type.into(),
        }
    }

    /// An `audio` block from raw bytes.
    #[must_use]
    pub fn audio(bytes: impl AsRef<[u8]>, mime_type: impl Into<String>) -> Self {
        Self::Audio {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: mime_type.into(),
        }
    }
}

/// The answer of one tool call.
///
/// A tool that ran answers with a variant; a call that could not run (the
/// scope gate's refusal, authentication, an unknown tool, invalid input, a
/// host's [`McpToolError`](super::McpToolError)) answers with that error,
/// which the transport reports as a protocol error. The two never convert
/// into each other: [`Self::Failure`] is built by a host terminal and
/// nothing in Proxima turns an error into one.
///
/// ```compile_fail,E0277
/// let error = proxima_core::McpToolError::NotAuthorized("tool".into());
/// let _reply: proxima_core::ToolReply = error.into();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolReply {
    /// A JSON object, sent as `structuredContent` plus its text rendering.
    /// Every registry tool answers this way, and a behavior that only
    /// post-processes JSON matches this variant and passes the others on.
    Structured(serde_json::Value),
    /// Content blocks and no `structuredContent`.
    Content(Vec<ToolContent>),
    /// The tool ran and failed in a way the caller should read and may
    /// retry: content blocks with `isError: true`. Proxima does not redact
    /// them; the host owns what it puts here.
    Failure(Vec<ToolContent>),
}

impl ToolReply {
    /// The JSON of a [`Self::Structured`] reply; any other reply comes back
    /// unchanged. For a surface that carries JSON only.
    ///
    /// # Errors
    ///
    /// The reply itself when it is [`Self::Content`] or [`Self::Failure`].
    pub fn into_structured(self) -> Result<serde_json::Value, Self> {
        match self {
            Self::Structured(value) => Ok(value),
            other @ (Self::Content(_) | Self::Failure(_)) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_blocks_carry_standard_padded_base64() {
        assert_eq!(
            ToolContent::image([0xfb, 0xff], "image/png"),
            ToolContent::Image {
                data: "+/8=".into(),
                mime_type: "image/png".into(),
            }
        );
        assert_eq!(
            ToolContent::audio(b"ab", "audio/wav"),
            ToolContent::Audio {
                data: "YWI=".into(),
                mime_type: "audio/wav".into(),
            }
        );
    }

    #[test]
    fn only_structured_replies_become_json() {
        let json = serde_json::json!({"a": 1});
        assert_eq!(
            ToolReply::Structured(json.clone()).into_structured(),
            Ok(json)
        );
        let content = ToolReply::Content(vec![ToolContent::text("x")]);
        assert_eq!(content.clone().into_structured(), Err(content));
        let failure = ToolReply::Failure(vec![ToolContent::text("x")]);
        assert_eq!(failure.clone().into_structured(), Err(failure));
    }
}
