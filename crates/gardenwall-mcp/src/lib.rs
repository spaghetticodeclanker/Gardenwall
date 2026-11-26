//! A minimal Model Context Protocol client
//! (local stdio servers, tools only).
//!
//! The server is an untrusted process that Gardenwall starts inside the OS
//! sandbox; this crate only speaks the protocol over its stdin and stdout:
//! - messages are newline-delimited JSON-RPC 2.0, each line capped in size;
//! - `initialize`, then `tools/list` (paginated) and `tools/call`;
//!   resources (`resources/list`, `resources/read`) and prompts
//!   (`prompts/list`, `prompts/get`; chosen by the user, never the model);
//! - requests the server makes of the client (sampling, roots, elicitation)
//!   are refused, so a server can't reach the model or the user's files
//!   through Gardenwall;
//! - tool names, descriptions, schemas and results are validated and
//!   bounded. Everything a server sends is data for the model, never
//!   instructions for Gardenwall.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// The protocol version Gardenwall asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Longest line accepted from a server.
pub const MAX_LINE_BYTES: usize = 8 << 20;
/// Most tools accepted from one server.
pub const MAX_TOOLS: usize = 128;
/// Longest tool description kept (characters).
pub const MAX_DESCRIPTION_CHARS: usize = 2000;
/// Largest input schema kept (bytes of JSON).
pub const MAX_SCHEMA_BYTES: usize = 32 * 1024;
/// Most text kept from one tool result.
pub const MAX_RESULT_CHARS: usize = 200_000;
/// Most resources and templates listed per server.
pub const MAX_RESOURCES: usize = 500;
/// Longest resource URI accepted.
pub const MAX_URI_CHARS: usize = 2048;
/// Most prompts listed per server.
pub const MAX_PROMPTS: usize = 100;
/// Most arguments kept per prompt.
pub const MAX_PROMPT_ARGUMENTS: usize = 16;
/// Longest argument value sent to a server (characters).
pub const MAX_ARGUMENT_CHARS: usize = 8_000;
/// Most text kept from a filled-in prompt (characters).
pub const MAX_PROMPT_CHARS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpError {
    /// The server stopped or closed its output.
    Closed,
    Timeout,
    /// The server answered with a JSON-RPC error.
    Server(String),
    /// The server sent something that isn't valid MCP.
    Protocol(&'static str),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("the MCP server stopped"),
            Self::Timeout => f.write_str("the MCP server did not answer in time"),
            Self::Server(m) => write!(f, "the MCP server reported an error: {m}"),
            Self::Protocol(m) => write!(f, "the MCP server sent an invalid message ({m})"),
        }
    }
}

impl std::error::Error for McpError {}

#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// A JSON Schema object for the arguments.
    pub input_schema: Value,
}

/// A resource a server offers (`resources/list`), or a URI template
/// (`resources/templates/list`, with `template` set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: Option<String>,
    pub template: bool,
}

/// A URI the client will send: bounded, no control characters.
pub fn valid_uri(uri: &str) -> bool {
    !uri.is_empty() && uri.chars().count() <= MAX_URI_CHARS && !uri.chars().any(char::is_control)
}

/// A prompt template a server offers (`prompts/list`). Prompts are chosen
/// by the user, never by the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPrompt {
    pub name: String,
    pub description: String,
    pub arguments: Vec<McpPromptArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptArgument {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// One message of a filled-in prompt (`prompts/get`), text only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptMessage {
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value, McpError>>>>>;

pub struct McpClient {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pending: Pending,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    tools_changed: Arc<AtomicBool>,
    /// Capabilities from `initialize`.
    has_tools: AtomicBool,
    has_resources: AtomicBool,
    has_prompts: AtomicBool,
}

fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Read one line of at most [`MAX_LINE_BYTES`]. `None` at end of stream or
/// when the line is too long (the connection is then unusable).
fn read_line(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let buf = reader.fill_buf().ok()?;
        if buf.is_empty() {
            return (!line.is_empty()).then_some(line);
        }
        match buf.iter().position(|b| *b == b'\n') {
            Some(i) => {
                line.extend_from_slice(&buf[..i]);
                reader.consume(i + 1);
                return Some(line);
            }
            None => {
                let n = buf.len();
                line.extend_from_slice(buf);
                reader.consume(n);
            }
        }
        if line.len() > MAX_LINE_BYTES {
            return None;
        }
    }
}

fn write_message(writer: &Mutex<Box<dyn Write + Send>>, message: &Value) -> Result<(), McpError> {
    let mut line = serde_json::to_vec(message).map_err(|_| McpError::Protocol("encoding"))?;
    line.push(b'\n');
    let mut w = writer.lock().map_err(|_| McpError::Closed)?;
    w.write_all(&line)
        .and_then(|()| w.flush())
        .map_err(|_| McpError::Closed)
}

impl McpClient {
    /// Connect to a server's stdout (`reader`) and stdin (`writer`).
    pub fn connect(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        let writer = Arc::new(Mutex::new(writer));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let tools_changed = Arc::new(AtomicBool::new(false));
        {
            let writer = writer.clone();
            let pending = pending.clone();
            let alive = alive.clone();
            let tools_changed = tools_changed.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(reader);
                while let Some(line) = read_line(&mut reader) {
                    let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
                        continue; // servers may print noise; it is ignored
                    };
                    let id = msg.get("id").and_then(Value::as_u64);
                    let method = msg.get("method").and_then(Value::as_str);
                    match (id, method) {
                        // A response to one of our requests.
                        (Some(id), None) => {
                            let result = match (msg.get("result"), msg.get("error")) {
                                (Some(r), _) => Ok(r.clone()),
                                (None, Some(e)) => Err(McpError::Server(clip(
                                    e.get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown error"),
                                    500,
                                ))),
                                _ => Err(McpError::Protocol("response without result")),
                            };
                            if let Some(tx) = pending.lock().ok().and_then(|mut p| p.remove(&id)) {
                                let _ = tx.send(result);
                            }
                        }
                        // A request from the server: refused (no sampling,
                        // roots or elicitation through Gardenwall).
                        (Some(_), Some(_)) | (None, Some(_)) if msg.get("id").is_some() => {
                            let reply = json!({
                                "jsonrpc": "2.0",
                                "id": msg["id"],
                                "error": {"code": -32601, "message": "not supported by Gardenwall"}
                            });
                            let _ = write_message(&writer, &reply);
                        }
                        (None, Some("notifications/tools/list_changed")) => {
                            tools_changed.store(true, Ordering::SeqCst);
                        }
                        _ => {}
                    }
                }
                alive.store(false, Ordering::SeqCst);
                // Wake everyone still waiting.
                if let Ok(mut p) = pending.lock() {
                    for (_, tx) in p.drain() {
                        let _ = tx.send(Err(McpError::Closed));
                    }
                }
            });
        }
        Self {
            writer,
            pending,
            next_id: AtomicU64::new(1),
            alive,
            tools_changed,
            has_tools: AtomicBool::new(false),
            has_resources: AtomicBool::new(false),
            has_prompts: AtomicBool::new(false),
        }
    }

    /// The server offers prompts (known after `initialize`).
    pub fn has_prompts(&self) -> bool {
        self.has_prompts.load(Ordering::SeqCst)
    }

    /// The server offers tools (known after `initialize`).
    pub fn has_tools(&self) -> bool {
        self.has_tools.load(Ordering::SeqCst)
    }

    /// The server offers resources (known after `initialize`).
    pub fn has_resources(&self) -> bool {
        self.has_resources.load(Ordering::SeqCst)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Whether the server said its tool list changed since the last listing.
    pub fn tools_changed(&self) -> bool {
        self.tools_changed.load(Ordering::SeqCst)
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, McpError> {
        if !self.is_alive() {
            return Err(McpError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending
            .lock()
            .map_err(|_| McpError::Closed)?
            .insert(id, tx);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = write_message(&self.writer, &message) {
            let _ = self.pending.lock().map(|mut p| p.remove(&id));
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                let _ = self.pending.lock().map(|mut p| p.remove(&id));
                // Ask the server to stop working on it.
                let _ = write_message(
                    &self.writer,
                    &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}}),
                );
                Err(McpError::Timeout)
            }
        }
    }

    /// The MCP handshake. Returns the server's self-reported name.
    pub fn initialize(&self, timeout: Duration) -> Result<String, McpError> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "Gardenwall", "version": env!("CARGO_PKG_VERSION")}
            }),
            timeout,
        )?;
        if result
            .get("capabilities")
            .and_then(Value::as_object)
            .is_none()
        {
            return Err(McpError::Protocol("initialize result without capabilities"));
        }
        let tools = result.pointer("/capabilities/tools").is_some();
        let resources = result.pointer("/capabilities/resources").is_some();
        let prompts = result.pointer("/capabilities/prompts").is_some();
        if !tools && !resources && !prompts {
            return Err(McpError::Protocol(
                "the server offers no tools, resources or prompts",
            ));
        }
        self.has_tools.store(tools, Ordering::SeqCst);
        self.has_resources.store(resources, Ordering::SeqCst);
        self.has_prompts.store(prompts, Ordering::SeqCst);
        write_message(
            &self.writer,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )?;
        Ok(clip(
            result
                .pointer("/serverInfo/name")
                .and_then(Value::as_str)
                .unwrap_or("MCP server"),
            80,
        ))
    }

    /// Every tool the server offers (following pagination), validated.
    /// Tools with invalid names are skipped.
    pub fn list_tools(&self, timeout: Duration) -> Result<Vec<McpTool>, McpError> {
        self.tools_changed.store(false, Ordering::SeqCst);
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..32 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.request("tools/list", params, timeout)?;
            let list = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or(McpError::Protocol("tools/list without tools"))?;
            for t in list {
                let Some(name) = t
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| valid_tool_name(n))
                else {
                    continue;
                };
                if tools.iter().any(|x: &McpTool| x.name == name) {
                    continue;
                }
                let schema = t
                    .get("inputSchema")
                    .filter(|s| s.get("type").and_then(Value::as_str) == Some("object"))
                    .filter(|s| s.to_string().len() <= MAX_SCHEMA_BYTES)
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                tools.push(McpTool {
                    name: name.to_string(),
                    description: clip(
                        t.get("description").and_then(Value::as_str).unwrap_or(""),
                        MAX_DESCRIPTION_CHARS,
                    ),
                    input_schema: schema,
                });
                if tools.len() >= MAX_TOOLS {
                    return Ok(tools);
                }
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty() && c.len() < 1024)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    /// Every resource and URI template the server offers (following
    /// pagination), validated and bounded.
    pub fn list_resources(&self, timeout: Duration) -> Result<Vec<McpResource>, McpError> {
        let mut out = Vec::new();
        for (method, key, uri_key, template) in [
            ("resources/list", "resources", "uri", false),
            (
                "resources/templates/list",
                "resourceTemplates",
                "uriTemplate",
                true,
            ),
        ] {
            let mut cursor: Option<String> = None;
            for _ in 0..32 {
                let params = match &cursor {
                    Some(c) => json!({"cursor": c}),
                    None => json!({}),
                };
                let result = match self.request(method, params, timeout) {
                    Ok(r) => r,
                    // Templates are optional.
                    Err(McpError::Server(_)) if template => break,
                    Err(e) => return Err(e),
                };
                for r in result
                    .get(key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(uri) = r
                        .get(uri_key)
                        .and_then(Value::as_str)
                        .filter(|u| valid_uri(u))
                    else {
                        continue;
                    };
                    out.push(McpResource {
                        uri: uri.to_string(),
                        name: clip(r.get("name").and_then(Value::as_str).unwrap_or(""), 200),
                        description: clip(
                            r.get("description").and_then(Value::as_str).unwrap_or(""),
                            500,
                        ),
                        mime_type: r
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .map(|m| clip(m, 100)),
                        template,
                    });
                    if out.len() >= MAX_RESOURCES {
                        return Ok(out);
                    }
                }
                cursor = result
                    .get("nextCursor")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty() && c.len() < 1024)
                    .map(str::to_string);
                if cursor.is_none() {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Every prompt the server offers (following pagination). Names follow
    /// the tool-name rules; prompts with other names are skipped.
    pub fn list_prompts(&self, timeout: Duration) -> Result<Vec<McpPrompt>, McpError> {
        let mut out: Vec<McpPrompt> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..32 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.request("prompts/list", params, timeout)?;
            let list = result
                .get("prompts")
                .and_then(Value::as_array)
                .ok_or(McpError::Protocol("prompts/list without prompts"))?;
            for p in list {
                let Some(name) = p
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| valid_tool_name(n))
                else {
                    continue;
                };
                if out.iter().any(|x| x.name == name) {
                    continue;
                }
                let mut arguments: Vec<McpPromptArgument> = Vec::new();
                for a in p
                    .get("arguments")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(arg) = a
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| valid_tool_name(n))
                    else {
                        continue;
                    };
                    if arguments.iter().any(|x| x.name == arg) {
                        continue;
                    }
                    arguments.push(McpPromptArgument {
                        name: arg.to_string(),
                        description: clip(
                            a.get("description").and_then(Value::as_str).unwrap_or(""),
                            300,
                        ),
                        required: a.get("required").and_then(Value::as_bool).unwrap_or(false),
                    });
                    if arguments.len() >= MAX_PROMPT_ARGUMENTS {
                        break;
                    }
                }
                out.push(McpPrompt {
                    name: name.to_string(),
                    description: clip(
                        p.get("description").and_then(Value::as_str).unwrap_or(""),
                        500,
                    ),
                    arguments,
                });
                if out.len() >= MAX_PROMPTS {
                    return Ok(out);
                }
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty() && c.len() < 1024)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Fill in a prompt. Text parts are kept (embedded text resources too);
    /// images, audio and other content are noted but not passed on.
    pub fn get_prompt(
        &self,
        name: &str,
        arguments: &[(String, String)],
        timeout: Duration,
    ) -> Result<Vec<McpPromptMessage>, McpError> {
        if !valid_tool_name(name) {
            return Err(McpError::Protocol("invalid prompt name"));
        }
        let mut args = serde_json::Map::new();
        for (k, v) in arguments {
            if !valid_tool_name(k) || v.chars().count() > MAX_ARGUMENT_CHARS {
                return Err(McpError::Protocol("invalid prompt argument"));
            }
            args.insert(k.clone(), Value::String(v.clone()));
        }
        let result = self.request(
            "prompts/get",
            json!({"name": name, "arguments": args}),
            timeout,
        )?;
        let mut out = Vec::new();
        let mut total = 0usize;
        for m in result
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(McpError::Protocol("prompts/get without messages"))?
        {
            let role = match m.get("role").and_then(Value::as_str) {
                Some("assistant") => "assistant",
                _ => "user",
            };
            let c = m.get("content").unwrap_or(&Value::Null);
            let text = match c.get("type").and_then(Value::as_str) {
                Some("text") => c
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                Some("resource") => match c.pointer("/resource/text").and_then(Value::as_str) {
                    Some(t) => t.to_string(),
                    None => "[binary resource omitted]".into(),
                },
                Some(kind) => format!("[{} content omitted]", clip(kind, 20)),
                None => continue,
            };
            total += text.chars().count();
            out.push(McpPromptMessage {
                role: role.into(),
                text,
            });
            if total > MAX_PROMPT_CHARS {
                return Err(McpError::Protocol("the prompt is too long"));
            }
        }
        Ok(out)
    }

    /// Read a resource. Text contents are joined; binary contents are noted
    /// but not passed on.
    pub fn read_resource(&self, uri: &str, timeout: Duration) -> Result<String, McpError> {
        if !valid_uri(uri) {
            return Err(McpError::Protocol("invalid resource URI"));
        }
        let result = self.request("resources/read", json!({"uri": uri}), timeout)?;
        let mut parts = Vec::new();
        for c in result
            .get("contents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(t) = c.get("text").and_then(Value::as_str) {
                parts.push(t.to_string());
            } else if c.get("blob").is_some() {
                let mime = c
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("binary");
                parts.push(format!("[{} content omitted]", clip(mime, 60)));
            }
        }
        Ok(clip(&parts.join("\n"), MAX_RESULT_CHARS))
    }

    /// Call a tool. Text parts of the result are joined; other content is
    /// noted but not passed on.
    pub fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<ToolResult, McpError> {
        let result = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            timeout,
        )?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut parts = Vec::new();
        for c in result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match c.get("type").and_then(Value::as_str) {
                Some("text") => parts.push(
                    c.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
                Some("resource") => {
                    if let Some(t) = c.pointer("/resource/text").and_then(Value::as_str) {
                        parts.push(t.to_string());
                    }
                }
                Some(kind) => parts.push(format!("[{} content omitted]", clip(kind, 20))),
                None => {}
            }
        }
        if parts.is_empty()
            && let Some(s) = result.get("structuredContent")
        {
            parts.push(s.to_string());
        }
        Ok(ToolResult {
            text: clip(&parts.join("\n"), MAX_RESULT_CHARS),
            is_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, channel as ch};

    /// One direction of an in-memory pipe.
    struct PipeRead(Receiver<Vec<u8>>, Vec<u8>);
    struct PipeWrite(Sender<Vec<u8>>);

    impl Read for PipeRead {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.1.is_empty() {
                match self.0.recv() {
                    Ok(b) => self.1 = b,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.1.len());
            buf[..n].copy_from_slice(&self.1[..n]);
            self.1.drain(..n);
            Ok(n)
        }
    }

    impl Write for PipeWrite {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .send(buf.to_vec())
                .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A fake server: answers by `respond`, and records what it got.
    fn server(respond: fn(&Value) -> Vec<Value>) -> (McpClient, Arc<Mutex<Vec<Value>>>) {
        let (to_server, from_client) = ch::<Vec<u8>>();
        let (to_client, from_server) = ch::<Vec<u8>>();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(PipeRead(from_client, vec![]));
            while let Some(line) = read_line(&mut r) {
                let msg: Value = serde_json::from_slice(&line).unwrap();
                log.lock().unwrap().push(msg.clone());
                for out in respond(&msg) {
                    let mut l = serde_json::to_vec(&out).unwrap();
                    l.push(b'\n');
                    if to_client.send(l).is_err() {
                        return;
                    }
                }
            }
        });
        let client = McpClient::connect(
            Box::new(PipeRead(from_server, vec![])),
            Box::new(PipeWrite(to_server)),
        );
        (client, seen)
    }

    fn reply(msg: &Value, result: Value) -> Vec<Value> {
        vec![json!({"jsonrpc": "2.0", "id": msg["id"], "result": result})]
    }

    fn good(msg: &Value) -> Vec<Value> {
        match msg["method"].as_str() {
            Some("initialize") => reply(
                msg,
                json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {"tools": {}}, "serverInfo": {"name": "notes"}}),
            ),
            Some("tools/list") if msg["params"].get("cursor").is_none() => reply(
                msg,
                json!({"tools": [
                    {"name": "search_notes", "description": "Search", "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}},
                    {"name": "bad name!", "description": "x"},
                    {"name": "no_schema"}
                ], "nextCursor": "p2"}),
            ),
            Some("tools/list") => reply(
                msg,
                json!({"tools": [{"name": "page_two", "inputSchema": {"type": "string"}}]}),
            ),
            Some("tools/call") => {
                // The server also tries to ask the client for sampling.
                let mut out = vec![
                    json!({"jsonrpc": "2.0", "id": 99, "method": "sampling/createMessage", "params": {}}),
                ];
                out.extend(reply(msg, json!({"content": [
                    {"type": "text", "text": format!("found {}", msg["params"]["arguments"]["q"])},
                    {"type": "image", "data": "AAAA", "mimeType": "image/png"}
                ]})));
                out
            }
            _ => vec![],
        }
    }

    #[test]
    fn handshake_listing_and_calls_work() {
        let (c, seen) = server(good);
        let t = Duration::from_secs(5);
        assert_eq!(c.initialize(t).unwrap(), "notes");
        let tools = c.list_tools(t).unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["search_notes", "no_schema", "page_two"]);
        // Missing or non-object schemas become an empty object schema.
        assert_eq!(tools[1].input_schema, json!({"type": "object"}));
        assert_eq!(tools[2].input_schema, json!({"type": "object"}));
        let r = c.call_tool("search_notes", json!({"q": "tax"}), t).unwrap();
        assert_eq!(r.text, "found \"tax\"\n[image content omitted]");
        assert!(!r.is_error);
        std::thread::sleep(Duration::from_millis(100));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1]["method"], "notifications/initialized");
        // The sampling request was refused.
        assert!(
            seen.iter()
                .any(|m| m["id"] == 99 && m["error"]["code"] == -32601)
        );
    }

    #[test]
    fn errors_timeouts_and_closed_servers_are_reported() {
        fn rude(msg: &Value) -> Vec<Value> {
            match msg["method"].as_str() {
                Some("initialize") => vec![
                    json!({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -1, "message": "nope"}}),
                ],
                _ => vec![],
            }
        }
        let (c, _) = server(rude);
        assert_eq!(
            c.initialize(Duration::from_secs(5)),
            Err(McpError::Server("nope".into()))
        );
        assert_eq!(
            c.list_tools(Duration::from_millis(200)),
            Err(McpError::Timeout)
        );

        fn nothing(msg: &Value) -> Vec<Value> {
            reply(msg, json!({"capabilities": {"logging": {}}}))
        }
        let (c, _) = server(nothing);
        assert!(matches!(
            c.initialize(Duration::from_secs(5)),
            Err(McpError::Protocol(_))
        ));

        // A server whose output ends.
        let (tx, rx) = ch::<Vec<u8>>();
        drop(tx);
        let (w, _keep) = ch::<Vec<u8>>();
        let c = McpClient::connect(Box::new(PipeRead(rx, vec![])), Box::new(PipeWrite(w)));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!c.is_alive());
        assert_eq!(c.initialize(Duration::from_secs(1)), Err(McpError::Closed));
    }

    #[test]
    fn resources_are_listed_and_read_within_limits() {
        fn res(msg: &Value) -> Vec<Value> {
            match msg.get("method").and_then(Value::as_str) {
                Some("initialize") => reply(msg, json!({"capabilities": {"resources": {}}})),
                Some("resources/list") => reply(
                    msg,
                    json!({"resources": [
                        {"uri": "notes://today", "name": "Today", "mimeType": "text/markdown"},
                        {"uri": "bad\u{7}uri", "name": "hidden"},
                        {"name": "no uri"}
                    ]}),
                ),
                Some("resources/templates/list") => reply(
                    msg,
                    json!({"resourceTemplates": [{"uriTemplate": "notes://{date}", "name": "By date"}]}),
                ),
                Some("resources/read") => reply(
                    msg,
                    json!({"contents": [
                        {"uri": "notes://today", "text": "buy milk"},
                        {"uri": "notes://today", "blob": "AAAA", "mimeType": "image/png"}
                    ]}),
                ),
                _ => vec![],
            }
        }
        let (c, seen) = server(res);
        c.initialize(Duration::from_secs(5)).unwrap();
        assert!(c.has_resources() && !c.has_tools());
        let list = c.list_resources(Duration::from_secs(5)).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].uri.as_str(), list[0].template),
            ("notes://today", false)
        );
        assert_eq!(
            (list[1].uri.as_str(), list[1].template),
            ("notes://{date}", true)
        );
        assert_eq!(
            c.read_resource("notes://today", Duration::from_secs(5))
                .unwrap(),
            "buy milk\n[image/png content omitted]"
        );
        assert!(c.read_resource("x\ny", Duration::from_secs(5)).is_err());
        let sent: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| m.get("method").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert!(sent.contains(&"resources/read".to_string()));
        assert!(!sent.contains(&"tools/list".to_string()));
    }

    #[test]
    fn prompts_are_listed_and_filled_in_within_limits() {
        fn pr(msg: &Value) -> Vec<Value> {
            match msg.get("method").and_then(Value::as_str) {
                Some("initialize") => reply(msg, json!({"capabilities": {"prompts": {}}})),
                Some("prompts/list") => reply(
                    msg,
                    json!({"prompts": [
                        {"name": "review", "description": "Review code", "arguments": [
                            {"name": "file", "description": "Which file", "required": true},
                            {"name": "bad arg!"},
                            {"name": "focus"}
                        ]},
                        {"name": "bad name!"},
                        {"name": "review"},
                        {"name": "plain"}
                    ]}),
                ),
                Some("prompts/get") => reply(
                    msg,
                    json!({"messages": [
                        {"role": "user", "content": {"type": "text", "text": format!("Review {}", msg["params"]["arguments"]["file"])}},
                        {"role": "assistant", "content": {"type": "text", "text": "Sure."}},
                        {"role": "system", "content": {"type": "image", "data": "AAAA"}},
                        {"role": "user", "content": {"type": "resource", "resource": {"uri": "x://y", "text": "embedded"}}}
                    ]}),
                ),
                _ => vec![],
            }
        }
        let (c, seen) = server(pr);
        let t = Duration::from_secs(5);
        c.initialize(t).unwrap();
        assert!(c.has_prompts() && !c.has_tools() && !c.has_resources());
        let list = c.list_prompts(t).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "review");
        let args: Vec<(&str, bool)> = list[0]
            .arguments
            .iter()
            .map(|a| (a.name.as_str(), a.required))
            .collect();
        assert_eq!(args, vec![("file", true), ("focus", false)]);
        let msgs = c
            .get_prompt("review", &[("file".into(), "main.rs".into())], t)
            .unwrap();
        let got: Vec<(&str, &str)> = msgs
            .iter()
            .map(|m| (m.role.as_str(), m.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("user", "Review \"main.rs\""),
                ("assistant", "Sure."),
                // Unknown roles become user; images are noted, not passed.
                ("user", "[image content omitted]"),
                ("user", "embedded"),
            ]
        );
        // Invalid names and oversized values never reach the server.
        assert!(c.get_prompt("bad name!", &[], t).is_err());
        assert!(
            c.get_prompt(
                "review",
                &[("file".into(), "x".repeat(MAX_ARGUMENT_CHARS + 1))],
                t
            )
            .is_err()
        );
        let gets = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m["method"] == "prompts/get")
            .count();
        assert_eq!(gets, 1);
    }

    #[test]
    fn line_reader_survives_mutation() {
        let seed = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"search\",\"inputSchema\":{\"type\":\"object\"}}]}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n".to_vec();
        gardenwall_fuzzkit::run("mcp-lines", &[seed], 5000, |bytes| {
            let mut r = BufReader::new(bytes);
            while let Some(line) = read_line(&mut r) {
                assert!(line.len() <= MAX_LINE_BYTES + 8192);
                let _ = serde_json::from_slice::<Value>(&line);
            }
        });
    }

    #[test]
    fn overlong_lines_end_the_connection() {
        let data = vec![b'x'; MAX_LINE_BYTES + 10];
        let mut r = BufReader::new(&data[..]);
        assert!(read_line(&mut r).is_none());
        let mut r = BufReader::new(&b"{\"a\":1}\npartial"[..]);
        assert_eq!(read_line(&mut r).unwrap(), b"{\"a\":1}");
        assert_eq!(read_line(&mut r).unwrap(), b"partial");
        assert!(read_line(&mut r).is_none());
    }
}
