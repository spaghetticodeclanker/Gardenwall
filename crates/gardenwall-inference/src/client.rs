//! OpenAI-compatible chat client for the local inference sidecar.
//!
//! Everything received from the runtime is untrusted model output: it is
//! parsed with size limits and surfaced as data, never interpreted as
//! authority. Usage and timing data feed the context-window tracker
//! (ADR-014 §2.4).

use std::fmt;
use std::ops::ControlFlow;
use std::time::Duration;

use bytes::Bytes;
use http::{Method, Request, Response, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};

use crate::launch::{ApiKey, MODEL_ALIAS};
use crate::transport::Connector;

/// Maximum bytes of a single server-sent event line.
const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum bytes of a non-streaming JSON response.
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
/// Maximum accumulated assistant content per response.
const MAX_RESPONSE_CHARS: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    Http(hyper::Error),
    Timeout,
    Status(u16),
    TooLarge,
    Malformed,
    Cancelled,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot reach inference runtime: {e}"),
            Self::Http(e) => write!(f, "inference request failed: {e}"),
            Self::Timeout => f.write_str("inference runtime did not respond in time"),
            Self::Status(code) => write!(f, "inference runtime returned HTTP {code}"),
            Self::TooLarge => f.write_str("inference response exceeded the size limit"),
            Self::Malformed => f.write_str("inference response was malformed"),
            Self::Cancelled => f.write_str("generation cancelled"),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments exactly as produced by the model (untrusted).
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Images attached to a user message, as `data:` URLs (vision models).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub images: Vec<String>,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            images: Vec::new(),
        }
    }

    /// The OpenAI wire form: a message with images carries a content array
    /// of text and `image_url` parts.
    fn wire(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.remove("images");
            if !self.images.is_empty() {
                let mut parts = vec![serde_json::json!({"type": "text", "text": self.content})];
                parts.extend(self.images.iter().map(
                    |url| serde_json::json!({"type": "image_url", "image_url": {"url": url}}),
                ));
                obj.insert("content".into(), serde_json::Value::Array(parts));
            }
        }
        v
    }
}

/// Sampling parameters. `None` uses the runtime/model defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Sampling {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    /// OpenAI-style tool definitions (JSON schema), supplied by the trusted
    /// core's tool registry.
    pub tools: Vec<serde_json::Value>,
    pub sampling: Sampling,
    /// Variables for the chat template, such as `enable_thinking` or
    /// `reasoning_effort` (llama-server's `chat_template_kwargs`). Built by
    /// the trusted core from the user's settings only.
    pub template_kwargs: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'static str,
    messages: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tools: &'a [serde_json::Value],
    #[serde(flatten)]
    sampling: &'a Sampling,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<&'a serde_json::Map<String, serde_json::Value>>,
    stream: bool,
    stream_options: StreamOptions,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct Timings {
    #[serde(default)]
    pub cache_n: u64,
    #[serde(default)]
    pub prompt_n: u64,
    #[serde(default)]
    pub prompt_ms: f64,
    #[serde(default)]
    pub prompt_per_second: f64,
    #[serde(default)]
    pub predicted_n: u64,
    #[serde(default)]
    pub predicted_ms: f64,
    #[serde(default)]
    pub predicted_per_second: f64,
    /// Tokens guessed by speculative decoding, and how many the model kept.
    #[serde(default)]
    pub draft_n: u64,
    #[serde(default)]
    pub draft_n_accepted: u64,
}

/// Incremental output from a streaming completion.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Content(String),
    Reasoning(String),
    ToolCallDelta {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    Finish(String),
}

/// The assembled result of a completion.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatResponse {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub timings: Option<Timings>,
    /// Time from sending the request to the first content/reasoning token.
    pub time_to_first_token: Option<Duration>,
}

/// Runtime properties relevant to the UI and context tracker.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeProps {
    pub context_window: u64,
    pub supports_tools: bool,
    pub build: Option<String>,
}

// --- Wire formats for streamed chunks ---

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    usage: Option<Usage>,
    timings: Option<Timings>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallChunk>,
}

#[derive(Deserialize)]
struct ToolCallChunk {
    index: u32,
    id: Option<String>,
    function: Option<FunctionChunk>,
}

#[derive(Deserialize)]
struct FunctionChunk {
    name: Option<String>,
    arguments: Option<String>,
}

/// Incremental parser for `text/event-stream` bodies. Yields the payload of
/// each `data:` line; enforces a per-line size limit.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SseItem {
    Data(String),
    Done,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<SseItem>) -> Result<(), ClientError> {
        self.buf.extend_from_slice(bytes);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8(line).map_err(|_| ClientError::Malformed)?;
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.strip_prefix(' ').unwrap_or(data);
                if data == "[DONE]" {
                    out.push(SseItem::Done);
                } else {
                    out.push(SseItem::Data(data.to_string()));
                }
            }
            // Comments (":"), "event:", "id:", and blank lines are ignored.
        }
        if self.buf.len() > MAX_SSE_LINE_BYTES {
            return Err(ClientError::TooLarge);
        }
        Ok(())
    }
}

/// Accumulates streamed chunks into a [`ChatResponse`].
#[derive(Default)]
struct Assembler {
    response: ChatResponse,
    chars: usize,
}

impl Assembler {
    fn apply(&mut self, chunk: Chunk, events: &mut Vec<StreamEvent>) -> Result<(), ClientError> {
        if let Some(usage) = chunk.usage {
            self.response.usage = Some(usage);
        }
        if let Some(timings) = chunk.timings {
            self.response.timings = Some(timings);
        }
        for choice in chunk.choices {
            let delta = choice.delta;
            if let Some(text) = delta.reasoning_content.filter(|t| !t.is_empty()) {
                self.add_chars(text.len())?;
                self.response.reasoning.push_str(&text);
                events.push(StreamEvent::Reasoning(text));
            }
            if let Some(text) = delta.content.filter(|t| !t.is_empty()) {
                self.add_chars(text.len())?;
                self.response.content.push_str(&text);
                events.push(StreamEvent::Content(text));
            }
            for call in delta.tool_calls {
                let index = call.index as usize;
                if index > 256 {
                    return Err(ClientError::Malformed);
                }
                while self.response.tool_calls.len() <= index {
                    self.response.tool_calls.push(ToolCall {
                        id: String::new(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: String::new(),
                            arguments: String::new(),
                        },
                    });
                }
                let (name, arguments) = match call.function {
                    Some(f) => (f.name, f.arguments),
                    None => (None, None),
                };
                let added = call.id.as_ref().map_or(0, String::len)
                    + name.as_ref().map_or(0, String::len)
                    + arguments.as_ref().map_or(0, String::len);
                self.add_chars(added)?;
                let entry = &mut self.response.tool_calls[index];
                if let Some(id) = &call.id {
                    entry.id.push_str(id);
                }
                if let Some(n) = &name {
                    entry.function.name.push_str(n);
                }
                if let Some(a) = &arguments {
                    entry.function.arguments.push_str(a);
                }
                events.push(StreamEvent::ToolCallDelta {
                    index: call.index,
                    id: call.id,
                    name,
                    arguments,
                });
            }
            if let Some(reason) = choice.finish_reason {
                self.response.finish_reason = Some(reason.clone());
                events.push(StreamEvent::Finish(reason));
            }
        }
        Ok(())
    }

    fn add_chars(&mut self, n: usize) -> Result<(), ClientError> {
        self.chars = self.chars.saturating_add(n);
        if self.chars > MAX_RESPONSE_CHARS {
            Err(ClientError::TooLarge)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct InferenceClient {
    connector: Connector,
    key: ApiKey,
}

impl fmt::Debug for InferenceClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InferenceClient")
            .field("connector", &self.connector)
            .finish_non_exhaustive()
    }
}

/// Timeouts for runtime requests.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const PROPS_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for one embeddings request.
const EMBED_TIMEOUT: Duration = Duration::from_secs(300);
/// Largest embeddings response.
const MAX_EMBED_RESPONSE_BYTES: usize = 128 << 20;
/// Maximum silence between streamed chunks (covers long prompt processing).
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Attempts when a pre-connected stream turns out to be stale.
const STALE_STREAM_ATTEMPTS: usize = 3;

impl InferenceClient {
    pub fn new(connector: Connector, key: ApiKey) -> Self {
        Self { connector, key }
    }

    pub fn connector(&self) -> &Connector {
        &self.connector
    }

    /// Send one request on a fresh connection and return the response head
    /// with its streaming body. llama-server has no side effects beyond
    /// compute, so a request whose pre-connected stream was stale is retried
    /// on another one.
    async fn send(
        &self,
        method: Method,
        path: &'static str,
        body: Option<Vec<u8>>,
    ) -> Result<Response<Incoming>, ClientError> {
        let attempts = if self.connector.retries_stale_streams() {
            STALE_STREAM_ATTEMPTS
        } else {
            1
        };
        let mut last = ClientError::Malformed;
        for _ in 0..attempts {
            let stream = self.connector.connect().await.map_err(ClientError::Io)?;
            let (mut sender, connection) = match http1::handshake(TokioIo::new(stream)).await {
                Ok(pair) => pair,
                Err(e) => {
                    last = ClientError::Http(e);
                    continue;
                }
            };
            tokio::spawn(async move {
                // Errors surface through the request future.
                let _ = connection.await;
            });
            let mut builder = Request::builder()
                .method(method.clone())
                .uri(path)
                .header(header::HOST, "localhost")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", self.key.expose()),
                );
            if body.is_some() {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
            }
            let request = builder
                .body(Full::new(Bytes::from(body.clone().unwrap_or_default())))
                .map_err(|_| ClientError::Malformed)?;
            match sender.send_request(request).await {
                Ok(response) => return Ok(response),
                Err(e) => last = ClientError::Http(e),
            }
        }
        Err(last)
    }

    /// Read a whole body with a size cap.
    async fn read_body(body: &mut Incoming, limit: usize) -> Result<Vec<u8>, ClientError> {
        let mut out = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.map_err(ClientError::Http)?.into_data() {
                if out.len() + data.len() > limit {
                    return Err(ClientError::TooLarge);
                }
                out.extend_from_slice(&data);
            }
        }
        Ok(out)
    }

    /// `true` once the model is loaded and the server accepts requests.
    pub async fn health(&self) -> Result<bool, ClientError> {
        let response =
            tokio::time::timeout(HEALTH_TIMEOUT, self.send(Method::GET, "/health", None))
                .await
                .map_err(|_| ClientError::Timeout)??;
        Ok(response.status().is_success())
    }

    async fn get_json(&self, path: &'static str) -> Result<serde_json::Value, ClientError> {
        tokio::time::timeout(PROPS_TIMEOUT, async {
            let response = self.send(Method::GET, path, None).await?;
            if !response.status().is_success() {
                return Err(ClientError::Status(response.status().as_u16()));
            }
            let mut body = response.into_body();
            let bytes = Self::read_body(&mut body, MAX_JSON_BYTES).await?;
            serde_json::from_slice(&bytes).map_err(|_| ClientError::Malformed)
        })
        .await
        .map_err(|_| ClientError::Timeout)?
    }

    pub async fn props(&self) -> Result<RuntimeProps, ClientError> {
        let v = self.get_json("/props").await?;
        let context_window = v
            .pointer("/default_generation_settings/n_ctx")
            .and_then(serde_json::Value::as_u64)
            .ok_or(ClientError::Malformed)?;
        let supports_tools = v
            .pointer("/chat_template_caps/supports_tool_calls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let build = v
            .get("build_info")
            .and_then(serde_json::Value::as_str)
            .filter(|s| s.len() <= 64)
            .map(str::to_string);
        Ok(RuntimeProps {
            context_window,
            supports_tools,
            build,
        })
    }

    /// Embedding vectors for `inputs`, from a server started with
    /// `--embedding`. Vectors are returned in input order, bounded in size,
    /// and checked to be finite.
    pub async fn embeddings(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, ClientError> {
        #[derive(Serialize)]
        struct Req<'a> {
            model: &'static str,
            input: &'a [String],
        }
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<Item>,
        }
        const MAX_DIMS: usize = 8192;
        let body = serde_json::to_vec(&Req {
            model: MODEL_ALIAS,
            input: inputs,
        })
        .map_err(|_| ClientError::Malformed)?;
        let bytes = tokio::time::timeout(EMBED_TIMEOUT, async {
            let response = self
                .send(Method::POST, "/v1/embeddings", Some(body))
                .await?;
            if !response.status().is_success() {
                return Err(ClientError::Status(response.status().as_u16()));
            }
            let mut body = response.into_body();
            Self::read_body(&mut body, MAX_EMBED_RESPONSE_BYTES).await
        })
        .await
        .map_err(|_| ClientError::Timeout)??;
        let mut resp: Resp = serde_json::from_slice(&bytes).map_err(|_| ClientError::Malformed)?;
        resp.data.sort_by_key(|i| i.index);
        let ok = resp.data.len() == inputs.len()
            && resp.data.iter().enumerate().all(|(n, i)| {
                i.index == n
                    && i.embedding.len() <= MAX_DIMS
                    && i.embedding.iter().all(|x| x.is_finite())
            });
        if !ok {
            return Err(ClientError::Malformed);
        }
        Ok(resp.data.into_iter().map(|i| i.embedding).collect())
    }

    /// Stream a chat completion. `on_event` receives each increment; return
    /// `ControlFlow::Break(())` to cancel generation.
    pub async fn chat(
        &self,
        request: &ChatRequest,
        mut on_event: impl FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, ClientError> {
        let body = WireRequest {
            model: MODEL_ALIAS,
            messages: request.messages.iter().map(ChatMessage::wire).collect(),
            tools: &request.tools,
            sampling: &request.sampling,
            chat_template_kwargs: request.template_kwargs.as_ref(),
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };
        let body = serde_json::to_vec(&body).map_err(|_| ClientError::Malformed)?;
        let started = std::time::Instant::now();
        let response = tokio::time::timeout(
            STREAM_IDLE_TIMEOUT,
            self.send(Method::POST, "/v1/chat/completions", Some(body)),
        )
        .await
        .map_err(|_| ClientError::Timeout)??;
        if !response.status().is_success() {
            return Err(ClientError::Status(response.status().as_u16()));
        }

        let mut parser = SseParser::default();
        let mut assembler = Assembler::default();
        let mut items = Vec::new();
        let mut events = Vec::new();
        let mut body = response.into_body();
        let mut done = false;
        while !done {
            let frame = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, body.frame()).await {
                Err(_) => return Err(ClientError::Timeout),
                Ok(None) => break,
                Ok(Some(frame)) => frame.map_err(ClientError::Http)?,
            };
            let Ok(bytes) = frame.into_data() else {
                continue;
            };
            parser.push(&bytes, &mut items)?;
            for item in items.drain(..) {
                match item {
                    // Anything after [DONE] in the same frame is ignored, but
                    // events already parsed from this frame are still
                    // delivered below.
                    SseItem::Done => {
                        done = true;
                        break;
                    }
                    SseItem::Data(data) => {
                        let chunk: Chunk =
                            serde_json::from_str(&data).map_err(|_| ClientError::Malformed)?;
                        assembler.apply(chunk, &mut events)?;
                    }
                }
            }
            items.clear();
            for event in events.drain(..) {
                if assembler.response.time_to_first_token.is_none()
                    && matches!(event, StreamEvent::Content(_) | StreamEvent::Reasoning(_))
                {
                    assembler.response.time_to_first_token = Some(started.elapsed());
                }
                if on_event(event).is_break() {
                    // Dropping the body closes the connection, which stops
                    // generation in llama-server.
                    return Err(ClientError::Cancelled);
                }
            }
        }
        Ok(assembler.response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_become_content_parts_on_the_wire() {
        let plain = ChatMessage::new(Role::User, "hi").wire();
        assert_eq!(plain["content"], "hi");
        assert!(plain.get("images").is_none());
        let mut m = ChatMessage::new(Role::User, "what is this?");
        m.images.push("data:image/png;base64,AAAA".into());
        let w = m.wire();
        assert_eq!(w["content"][0]["type"], "text");
        assert_eq!(w["content"][0]["text"], "what is this?");
        assert_eq!(
            w["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        assert!(w.get("images").is_none());
    }

    #[test]
    fn sse_parser_handles_split_lines_and_crlf() {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        p.push(b"data: {\"a\"", &mut out).unwrap();
        assert!(out.is_empty());
        p.push(b":1}\r\n\r\n: comment\nevent: x\ndata:[DONE]\n", &mut out)
            .unwrap();
        assert_eq!(out, vec![SseItem::Data("{\"a\":1}".into()), SseItem::Done]);
    }

    #[test]
    fn sse_parser_bounds_line_length() {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        let big = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
        assert!(matches!(p.push(&big, &mut out), Err(ClientError::TooLarge)));
    }

    #[test]
    fn sse_parser_rejects_invalid_utf8() {
        let mut p = SseParser::default();
        assert!(matches!(
            p.push(b"data: \xff\n", &mut Vec::new()),
            Err(ClientError::Malformed)
        ));
    }

    fn chunk(json: &str) -> Chunk {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn assembler_builds_content_reasoning_tools_usage() {
        let mut a = Assembler::default();
        let mut ev = Vec::new();
        a.apply(
            chunk(r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#),
            &mut ev,
        )
        .unwrap();
        a.apply(
            chunk(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#),
            &mut ev,
        )
        .unwrap();
        a.apply(
            chunk(r#"{"choices":[{"delta":{"content":"lo"}}]}"#),
            &mut ev,
        )
        .unwrap();
        a.apply(
            chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#),
            &mut ev,
        )
        .unwrap();
        a.apply(
            chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]},"finish_reason":"tool_calls"}]}"#),
            &mut ev,
        )
        .unwrap();
        a.apply(
            chunk(r#"{"choices":[],"usage":{"prompt_tokens":19,"completion_tokens":10,"total_tokens":29},"timings":{"prompt_n":19,"predicted_n":10,"predicted_per_second":572.1}}"#),
            &mut ev,
        )
        .unwrap();
        let r = a.response;
        assert_eq!(r.reasoning, "think");
        assert_eq!(r.content, "Hello");
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "c1");
        assert_eq!(r.tool_calls[0].function.name, "read");
        assert_eq!(r.tool_calls[0].function.arguments, r#"{"path":"a"}"#);
        assert_eq!(r.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(r.usage.unwrap().total_tokens, 29);
        assert_eq!(r.timings.unwrap().predicted_n, 10);
        assert_eq!(ev.len(), 6);
    }

    #[test]
    fn assembler_rejects_absurd_tool_indices() {
        let mut a = Assembler::default();
        let result = a.apply(
            chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":4000000000}]}}]}"#),
            &mut Vec::new(),
        );
        assert!(matches!(result, Err(ClientError::Malformed)));
    }

    #[test]
    fn request_serializes_like_openai() {
        let req = ChatRequest {
            messages: vec![ChatMessage::new(Role::User, "hi")],
            tools: vec![],
            sampling: Sampling {
                temperature: Some(0.6),
                ..Sampling::default()
            },
            template_kwargs: serde_json::json!({"reasoning_effort": "medium"})
                .as_object()
                .cloned(),
        };
        let wire = WireRequest {
            model: MODEL_ALIAS,
            messages: req.messages.iter().map(ChatMessage::wire).collect(),
            tools: &req.tools,
            sampling: &req.sampling,
            chat_template_kwargs: req.template_kwargs.as_ref(),
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            v["messages"][0],
            serde_json::json!({"role":"user","content":"hi"})
        );
        assert_eq!(v["temperature"], serde_json::json!(0.6f32));
        assert!(v.get("tools").is_none());
        assert!(v.get("top_p").is_none());
        assert_eq!(v["stream_options"]["include_usage"], true);
        assert_eq!(v["chat_template_kwargs"]["reasoning_effort"], "medium");
    }
}
