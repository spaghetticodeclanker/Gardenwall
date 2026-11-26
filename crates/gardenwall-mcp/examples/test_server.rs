//! A tiny MCP server for testing Gardenwall's client and sandbox: tools
//! `echo` (returns its text) and `read_file` (tries to read a path, to show
//! what the sandbox allows), and a `greet` prompt.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let result = match msg["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}, "prompts": {}},
                "serverInfo": {"name": "gardenwall-test-server"}
            }),
            Some("prompts/list") => json!({"prompts": [
                {"name": "greet", "description": "Greet someone", "arguments": [
                    {"name": "who", "required": true},
                    {"name": "style"}
                ]}
            ]}),
            // The text carries a zero-width space, which the client side
            // must make visible before anyone sends it.
            Some("prompts/get") => {
                let args = &msg["params"]["arguments"];
                json!({"messages": [{"role": "user", "content": {"type": "text", "text": format!(
                    "Say hello to {} ({}).\u{200B}",
                    args["who"].as_str().unwrap_or("?"),
                    args["style"].as_str().unwrap_or("plain")
                )}}]})
            }
            Some("tools/list") => json!({"tools": [
                {"name": "echo", "description": "Return the text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
                {"name": "read_file", "description": "Read a file", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}}
            ]}),
            Some("tools/call") => {
                let args = &msg["params"]["arguments"];
                match msg["params"]["name"].as_str() {
                    Some("echo") => {
                        json!({"content": [{"type": "text", "text": args["text"].as_str().unwrap_or("")}]})
                    }
                    Some("read_file") => {
                        match std::fs::read_to_string(args["path"].as_str().unwrap_or("")) {
                            Ok(t) => json!({"content": [{"type": "text", "text": t}]}),
                            Err(e) => {
                                json!({"content": [{"type": "text", "text": format!("denied: {e}")}], "isError": true})
                            }
                        }
                    }
                    _ => json!({"content": [], "isError": true}),
                }
            }
            _ => json!({}),
        };
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
}
