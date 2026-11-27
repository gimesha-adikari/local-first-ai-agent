//! OpenAI-compatible Chat Completions adapter. It only builds request bodies
//! and parses response bytes; the trusted boundary owns every connection.
//! Compatible servers differ, so optional features (usage in the stream,
//! image parts) follow the provider's configuration and failures are
//! normalized rather than worked around.

use agent_core::{
    capability::ImageInput,
    chat::{MessageRole, PromptMessage},
    inference::{ErrorCode, FinishReason, InferenceError},
};
use serde::Serialize;
use serde_json::Value;

use crate::network::Route;

pub(crate) const CHAT_COMPLETIONS: Route = Route::new("/chat/completions");
pub(crate) const MODELS: Route = Route::new("/models");

/// Largest single SSE line accepted.
pub(crate) const MAX_EVENT_BYTES: usize = 1024 * 1024;
/// Largest non-streaming or error body parsed.
pub(crate) const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROVIDER_MESSAGE: usize = 200;
const MAX_MODELS: usize = 200;

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Part<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
#[serde(untagged)]
enum Content<'a> {
    Text(&'a str),
    Parts(Vec<Part<'a>>),
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'static str,
    content: Content<'a>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatBody<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    max_tokens: u32,
    temperature: f32,
}

pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub(crate) struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [PromptMessage<'a>],
    /// Attached to the final user message.
    pub images: &'a [&'a ImageInput],
    pub max_tokens: u32,
    pub temperature: f32,
    pub stream_usage: bool,
}

/// Serialize a streaming Chat Completions request.
pub(crate) fn chat_body(request: &ChatRequest<'_>) -> Vec<u8> {
    let last_user = request
        .messages
        .iter()
        .rposition(|message| message.role == MessageRole::User);
    let messages = request
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let content = if Some(index) == last_user && !request.images.is_empty() {
                let mut parts = vec![Part::Text {
                    text: message.content,
                }];
                parts.extend(request.images.iter().map(|image| Part::ImageUrl {
                    image_url: ImageUrl {
                        url: format!(
                            "data:{};base64,{}",
                            image.format().media_type(),
                            base64(image.bytes())
                        ),
                    },
                }));
                Content::Parts(parts)
            } else {
                Content::Text(message.content)
            };
            Message {
                role: message.role.as_str(),
                content,
            }
        })
        .collect();
    serde_json::to_vec(&ChatBody {
        model: request.model,
        messages,
        stream: true,
        stream_options: request.stream_usage.then_some(StreamOptions {
            include_usage: true,
        }),
        max_tokens: request.max_tokens,
        temperature: request.temperature,
    })
    .expect("a Chat request always serializes")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ReportedUsage {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Event {
    Delta(String),
    Finish(FinishReason),
    Usage(ReportedUsage),
    Done,
}

fn invalid_response() -> InferenceError {
    InferenceError::new(
        ErrorCode::ProviderInvalidResponse,
        "The provider sent a response this app could not read.",
    )
}

/// Whether a word looks like a credential a server might echo back.
fn key_like(word: &str) -> bool {
    let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
    let lower = word.to_ascii_lowercase();
    ["sk-", "key-", "bearer"]
        .iter()
        .any(|prefix| lower.starts_with(prefix) && word.len() > prefix.len() + 3)
        || (word.len() >= 20
            && word
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && word.bytes().any(|b| b.is_ascii_digit())
            && word.bytes().any(|b| b.is_ascii_alphabetic()))
}

/// Strip control characters, redact anything that looks like a credential
/// and bound a provider-supplied message. It is untrusted and only shown as
/// context in the error text.
fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_PROVIDER_MESSAGE * 2)
        .collect();
    let redacted = cleaned
        .split(' ')
        .map(|word| if key_like(word) { "[redacted]" } else { word })
        .collect::<Vec<_>>()
        .join(" ");
    let bounded: String = redacted.chars().take(MAX_PROVIDER_MESSAGE).collect();
    bounded.trim().to_owned()
}

fn finish_reason(value: &str) -> Result<Option<FinishReason>, InferenceError> {
    Ok(match value {
        "stop" | "end_turn" | "tool_calls" | "function_call" | "eos" => {
            Some(FinishReason::EndOfGeneration)
        }
        "length" | "max_tokens" => Some(FinishReason::TokenLimit),
        "content_filter" => {
            return Err(InferenceError::new(
                ErrorCode::ProviderFiltered,
                "The provider's content filter stopped this response.",
            ));
        }
        _ => None,
    })
}

fn usage(value: &Value) -> Option<ReportedUsage> {
    let usage = value.get("usage")?;
    let input = usage.get("prompt_tokens")?.as_u64()?;
    let output = usage.get("completion_tokens")?.as_u64()?;
    Some(ReportedUsage { input, output })
}

fn embedded_error(value: &Value) -> Option<InferenceError> {
    let error = value.get("error")?;
    Some(normalize(0, Some(error)))
}

/// Parse one decoded JSON chunk of a streamed response.
fn chunk_events(value: &Value, events: &mut Vec<Event>) -> Result<(), InferenceError> {
    if let Some(error) = embedded_error(value) {
        return Err(error);
    }
    if let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    {
        if let Some(text) = choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
            && !text.is_empty()
        {
            events.push(Event::Delta(text.to_owned()));
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str)
            && let Some(reason) = finish_reason(reason)?
        {
            events.push(Event::Finish(reason));
        }
    }
    if let Some(usage) = usage(value) {
        events.push(Event::Usage(usage));
    }
    Ok(())
}

/// Incremental server-sent events parser with a bounded line buffer.
#[derive(Default)]
pub(crate) struct Stream {
    line: Vec<u8>,
    data: Vec<u8>,
    done: bool,
}

impl Stream {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, InferenceError> {
        let mut events = Vec::new();
        for &byte in bytes {
            if self.done {
                break;
            }
            if byte != b'\n' {
                self.line.push(byte);
                if self.line.len() > MAX_EVENT_BYTES {
                    return Err(invalid_response());
                }
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let line = line.strip_suffix(b"\r").unwrap_or(&line);
            if line.is_empty() {
                self.dispatch(&mut events)?;
            } else if let Some(data) = line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(data);
                if self.data.len() > MAX_EVENT_BYTES {
                    return Err(invalid_response());
                }
            }
            // Comments, event names, ids and retry hints are ignored.
        }
        Ok(events)
    }

    fn dispatch(&mut self, events: &mut Vec<Event>) -> Result<(), InferenceError> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data);
        if data.as_slice() == b"[DONE]" {
            self.done = true;
            events.push(Event::Done);
            return Ok(());
        }
        let value: Value = serde_json::from_slice(&data).map_err(|_| invalid_response())?;
        chunk_events(&value, events)
    }

    /// The body ended. A final event without a blank line still counts.
    pub fn finish(&mut self) -> Result<Vec<Event>, InferenceError> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            events.extend(self.feed(b"\n")?);
        }
        self.dispatch(&mut events)?;
        Ok(events)
    }

    pub fn is_done(&self) -> bool {
        self.done
    }
}

/// Parse a complete (non-streaming) response, for servers that ignore
/// `stream: true`.
pub(crate) fn complete_events(body: &[u8]) -> Result<Vec<Event>, InferenceError> {
    if body.len() > MAX_JSON_BYTES {
        return Err(invalid_response());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid_response())?;
    if let Some(error) = embedded_error(&value) {
        return Err(error);
    }
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(invalid_response)?;
    let mut events = Vec::new();
    if let Some(text) = choice
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        && !text.is_empty()
    {
        events.push(Event::Delta(text.to_owned()));
    }
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str)
        && let Some(reason) = finish_reason(reason)?
    {
        events.push(Event::Finish(reason));
    }
    if let Some(usage) = usage(&value) {
        events.push(Event::Usage(usage));
    }
    events.push(Event::Done);
    Ok(events)
}

fn context_overflow(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "context_length",
        "context length",
        "maximum context",
        "too many tokens",
        "context window",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// Map an HTTP status and optional error object to a normalized error.
fn normalize(status: u16, error: Option<&Value>) -> InferenceError {
    let message = error
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or(error.as_str())
        })
        .map(sanitize)
        .unwrap_or_default();
    let code = error
        .and_then(|error| error.get("code").or(error.get("type")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (kind, text) = match status {
        401 | 403 => (
            ErrorCode::ProviderAuthentication,
            "The provider rejected the API key or this app's access.",
        ),
        429 => (
            ErrorCode::ProviderRateLimited,
            "The provider's rate limit or quota was reached. Try again later.",
        ),
        413 => (
            ErrorCode::ContextLimit,
            "The request is too large for this provider.",
        ),
        404 => (
            ErrorCode::ProviderUnavailable,
            "The provider could not find this endpoint or model. Check the URL and model.",
        ),
        408 | 500..=599 => (
            ErrorCode::ProviderUnavailable,
            "The provider is unavailable right now.",
        ),
        _ if context_overflow(code) || context_overflow(&message) => (
            ErrorCode::ContextLimit,
            "The conversation is too long for this model's context.",
        ),
        _ if code == "rate_limit_exceeded" || code == "insufficient_quota" => (
            ErrorCode::ProviderRateLimited,
            "The provider's rate limit or quota was reached. Try again later.",
        ),
        _ if code == "invalid_api_key" => (
            ErrorCode::ProviderAuthentication,
            "The provider rejected the API key or this app's access.",
        ),
        _ => (
            ErrorCode::ProviderRejected,
            "The provider rejected the request. Check the model and its supported features.",
        ),
    };
    if message.is_empty() {
        InferenceError::new(kind, text)
    } else {
        InferenceError::new(kind, format!("{text} Provider message: {message}"))
    }
}

/// Normalize an unsuccessful HTTP response from its bounded body.
pub(crate) fn error_response(status: u16, body: &[u8]) -> InferenceError {
    let value = (body.len() <= MAX_JSON_BYTES)
        .then(|| serde_json::from_slice::<Value>(body).ok())
        .flatten();
    normalize(status, value.as_ref().and_then(|value| value.get("error")))
}

/// Model identifiers from a `/models` response.
pub(crate) fn model_ids(body: &[u8]) -> Result<Vec<String>, InferenceError> {
    if body.len() > MAX_JSON_BYTES {
        return Err(invalid_response());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid_response())?;
    let list = value
        .get("data")
        .or(value.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(invalid_response)?;
    Ok(list
        .iter()
        .filter_map(|model| {
            model
                .get("id")
                .or(model.get("name"))
                .and_then(Value::as_str)
        })
        .filter(|id| !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_control))
        .take(MAX_MODELS)
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_text_and_image_requests() {
        let messages = [
            PromptMessage {
                role: MessageRole::System,
                content: "Be brief.",
            },
            PromptMessage {
                role: MessageRole::User,
                content: "Hi",
            },
            PromptMessage {
                role: MessageRole::Assistant,
                content: "Hello",
            },
            PromptMessage {
                role: MessageRole::User,
                content: "Describe",
            },
        ];
        let body: Value = serde_json::from_slice(&chat_body(&ChatRequest {
            model: "m",
            messages: &messages,
            images: &[],
            max_tokens: 64,
            temperature: 0.5,
            stream_usage: true,
        }))
        .unwrap();
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][3]["content"], "Describe");
        assert_eq!(body["messages"][0]["role"], "system");

        let png = ImageInput::new(b"\x89PNG\r\n\x1a\nrest".to_vec()).unwrap();
        let body: Value = serde_json::from_slice(&chat_body(&ChatRequest {
            model: "m",
            messages: &messages,
            images: &[&png],
            max_tokens: 64,
            temperature: 0.5,
            stream_usage: false,
        }))
        .unwrap();
        assert!(body.get("stream_options").is_none());
        let parts = body["messages"][3]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert!(
            parts[1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,iVBORw0KGgpyZXN0")
        );
        assert_eq!(body["messages"][1]["content"], "Hi");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
        assert!(ImageInput::new(b"GIF89a".to_vec()).is_none());
    }

    #[test]
    fn parses_split_streams_usage_and_completion() {
        let wire =
            b": keep-alive\n\ndata: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\r\n\r\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\ndata: {\"ignored\":true}\n\n";
        let mut stream = Stream::default();
        let mut events = Vec::new();
        for piece in wire.chunks(7) {
            events.extend(stream.feed(piece).unwrap());
        }
        assert_eq!(
            events,
            vec![
                Event::Delta("Hel".into()),
                Event::Delta("lo".into()),
                Event::Finish(FinishReason::EndOfGeneration),
                Event::Usage(ReportedUsage {
                    input: 12,
                    output: 2
                }),
                Event::Done,
            ]
        );
        assert!(stream.is_done());
        let mut length = Stream::default();
        let events = length
            .feed(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}")
            .unwrap();
        assert!(events.is_empty());
        assert_eq!(
            length.finish().unwrap(),
            vec![Event::Finish(FinishReason::TokenLimit)]
        );
    }

    #[test]
    fn rejects_malformed_and_oversized_streams() {
        assert_eq!(
            Stream::default()
                .feed(b"data: {not json}\n\n")
                .unwrap_err()
                .code,
            ErrorCode::ProviderInvalidResponse
        );
        let mut huge = Stream::default();
        let line = vec![b'x'; MAX_EVENT_BYTES + 1];
        assert!(huge.feed(&line).is_err());
        let mut many = Stream::default();
        let mut wire = Vec::new();
        for _ in 0..(MAX_EVENT_BYTES / 1000 + 2) {
            wire.extend_from_slice(b"data: ");
            wire.extend(vec![b'y'; 1000]);
            wire.push(b'\n');
        }
        assert!(many.feed(&wire).is_err(), "multi-line data is bounded too");
        let filtered = Stream::default()
            .feed(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n")
            .unwrap_err();
        assert_eq!(filtered.code, ErrorCode::ProviderFiltered);
        let mid_stream = Stream::default()
            .feed(b"data: {\"error\":{\"message\":\"overloaded\",\"type\":\"server_error\"}}\n\n")
            .unwrap_err();
        assert_eq!(mid_stream.code, ErrorCode::ProviderRejected);
        assert!(mid_stream.message.contains("overloaded"));
    }

    #[test]
    fn normalizes_errors_without_echoing_control_characters() {
        let cases: [(u16, &str, ErrorCode); 7] = [
            (
                401,
                r#"{"error":{"message":"bad key","code":"invalid_api_key"}}"#,
                ErrorCode::ProviderAuthentication,
            ),
            (
                429,
                r#"{"error":{"message":"slow down"}}"#,
                ErrorCode::ProviderRateLimited,
            ),
            (
                400,
                r#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#,
                ErrorCode::ContextLimit,
            ),
            (
                400,
                r#"{"error":{"message":"unknown field stream_options"}}"#,
                ErrorCode::ProviderRejected,
            ),
            (503, "upstream down", ErrorCode::ProviderUnavailable),
            (
                404,
                r#"{"error":"no such model"}"#,
                ErrorCode::ProviderUnavailable,
            ),
            (
                400,
                r#"{"error":{"code":"insufficient_quota"}}"#,
                ErrorCode::ProviderRateLimited,
            ),
        ];
        for (status, body, code) in cases {
            assert_eq!(
                error_response(status, body.as_bytes()).code,
                code,
                "{status} {body}"
            );
        }
        let noisy = error_response(
            400,
            "{\"error\":{\"message\":\"line\\nbreak\\u0007bell\"}}".as_bytes(),
        );
        assert!(!noisy.message.contains('\n') && !noisy.message.contains('\u{7}'));
        assert!(noisy.message.contains("line break bell"));
        let long = format!("{{\"error\":{{\"message\":\"{}\"}}}}", "z".repeat(1000));
        assert!(error_response(400, long.as_bytes()).message.len() < 400);
        let echoed = error_response(
            401,
            br#"{"error":{"message":"Incorrect API key provided: sk-proj-abc123SECRET456. Also token A1b2C3d4E5f6G7h8I9j0K1."}}"#,
        );
        assert!(
            !echoed.message.contains("abc123SECRET456"),
            "{}",
            echoed.message
        );
        assert!(!echoed.message.contains("A1b2C3d4E5f6G7h8I9j0K1"));
        assert!(
            echoed
                .message
                .contains("Incorrect API key provided: [redacted]")
        );
    }

    #[test]
    fn reads_complete_responses_and_model_lists() {
        let events = complete_events(
            br#"{"choices":[{"message":{"content":"Whole"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
        )
        .unwrap();
        assert_eq!(events[0], Event::Delta("Whole".into()));
        assert_eq!(events.last(), Some(&Event::Done));
        assert!(complete_events(b"{}").is_err());
        assert_eq!(
            model_ids(br#"{"data":[{"id":"a"},{"id":""},{"id":"b"},{"nope":1}]}"#).unwrap(),
            vec!["a", "b"]
        );
        assert!(model_ids(b"[]").is_err());
    }
}
