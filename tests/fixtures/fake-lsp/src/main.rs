//! A scripted language server for the Language service tests (spec, section 23: a fake LSP
//! server in `tests/fixtures`). It speaks UTF-16 positions unless `FAKE_LSP_ENCODING=utf-8`,
//! keeps documents in sync incrementally, and answers:
//!
//! - diagnostics: a warning on every `TODO`, code `todo`;
//! - completion: `alpha` and `beta` replacing the word before the caret; resolving `beta` adds
//!   an import at the top;
//! - hover: the word under the caret;
//! - definition: the first occurrence of the word under the caret;
//! - code actions: "Replace TODO with DONE" for each TODO diagnostic (edit on resolve), and
//!   the command "Insert header", which the server carries out with `workspace/applyEdit`;
//! - an edit leaving `CRASH` in a document makes the server exit with code 3.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};

use serde_json::{Value, json};

fn read(input: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn send(message: &Value) {
    let body = message.to_string();
    let mut out = io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

struct Server {
    utf8: bool,
    docs: HashMap<String, Vec<String>>,
    /// The `workspace/executeCommand` waiting for our `workspace/applyEdit`.
    executing: Option<Value>,
}

impl Server {
    /// Byte offset in `line` of a position character in the negotiated encoding.
    fn byte(&self, line: &str, character: u64) -> usize {
        let character = character as usize;
        if self.utf8 {
            return character.min(line.len());
        }
        let mut units = 0;
        for (i, c) in line.char_indices() {
            if units >= character {
                return i;
            }
            units += c.len_utf16();
        }
        line.len()
    }

    /// Position character of a byte offset.
    fn character(&self, line: &str, byte: usize) -> usize {
        if self.utf8 {
            byte
        } else {
            line[..byte].encode_utf16().count()
        }
    }

    fn apply(&mut self, uri: &str, change: &Value) {
        let Some(lines) = self.docs.get(uri).cloned() else {
            return;
        };
        let text = change["text"].as_str().unwrap_or_default();
        let Some(range) = change.get("range") else {
            self.set(uri, text);
            return;
        };
        let offset = |position: &Value| {
            let line = position["line"].as_u64().unwrap_or(0) as usize;
            let before: usize = lines.iter().take(line).map(|l| l.len() + 1).sum();
            let current = lines.get(line).map(String::as_str).unwrap_or("");
            before + self.byte(current, position["character"].as_u64().unwrap_or(0))
        };
        let (start, end) = (offset(&range["start"]), offset(&range["end"]));
        let mut whole = lines.join("\n");
        let end = end.min(whole.len());
        whole.replace_range(start.min(end)..end, text);
        self.set(uri, &whole);
    }

    fn set(&mut self, uri: &str, text: &str) {
        self.docs.insert(
            uri.to_owned(),
            text.split('\n').map(str::to_owned).collect(),
        );
    }

    fn position(&self, line: usize, text: &str, byte: usize) -> Value {
        json!({"line": line, "character": self.character(text, byte)})
    }

    fn todos(&self, uri: &str) -> Vec<Value> {
        let mut found = Vec::new();
        for (n, line) in self.docs.get(uri).into_iter().flatten().enumerate() {
            for (byte, _) in line.match_indices("TODO") {
                found.push(json!({
                    "range": {"start": self.position(n, line, byte), "end": self.position(n, line, byte + 4)},
                    "severity": 2,
                    "code": "todo",
                    "source": "fake",
                    "message": "TODO found",
                }));
            }
        }
        found
    }

    /// Publishes the TODOs; `crash` makes a `CRASH` in the text end the process (only on
    /// edits, so a restarted server survives the document it gets again).
    fn publish(&self, uri: &str, crash: bool) {
        send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": uri, "diagnostics": self.todos(uri)},
        }));
        if crash
            && self
            .docs
            .get(uri)
            .is_some_and(|lines| lines.iter().any(|l| l.contains("CRASH")))
        {
            std::process::exit(3);
        }
    }

    /// The identifier around a position: its line, start and end bytes.
    fn word_at(&self, params: &Value) -> Option<(usize, String, usize, usize)> {
        let uri = params["textDocument"]["uri"].as_str()?;
        let line_no = params["position"]["line"].as_u64()? as usize;
        let line = self.docs.get(uri)?.get(line_no)?.clone();
        let at = self.byte(&line, params["position"]["character"].as_u64()?);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let start = line[..at]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map_or(at, |(i, _)| i);
        let end = line[at..]
            .char_indices()
            .find(|(_, c)| !is_word(*c))
            .map_or(line.len(), |(i, _)| at + i);
        Some((line_no, line, start, end))
    }

    fn request(&mut self, id: Value, method: &str, params: &Value) -> Option<Value> {
        let result = match method {
            "initialize" => json!({
                "capabilities": {
                    "positionEncoding": if self.utf8 { "utf-8" } else { "utf-16" },
                    "textDocumentSync": 2,
                    "completionProvider": {"resolveProvider": true},
                    "hoverProvider": true,
                    "definitionProvider": true,
                    "codeActionProvider": {"resolveProvider": true},
                    "executeCommandProvider": {"commands": ["fake.insertHeader"]},
                },
                "serverInfo": {"name": "fake-lsp"},
            }),
            "shutdown" => Value::Null,
            "textDocument/completion" => {
                let (line_no, line, start, _) = self.word_at(params)?;
                let at = self.byte(&line, params["position"]["character"].as_u64()?);
                let range = json!({"start": self.position(line_no, &line, start), "end": self.position(line_no, &line, at)});
                json!([
                    {"label": "alpha", "kind": 6, "textEdit": {"range": range, "newText": "alpha"}},
                    {"label": "beta", "kind": 3, "detail": "fn beta()", "textEdit": {"range": range, "newText": "beta()"}, "data": 1},
                ])
            }
            "completionItem/resolve" => {
                let mut item = params.clone();
                if item["label"] == "beta" {
                    item["additionalTextEdits"] = json!([{
                        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                        "newText": "import beta;\n",
                    }]);
                }
                item
            }
            "textDocument/hover" => match self.word_at(params) {
                Some((_, line, start, end)) if start < end => {
                    json!({"contents": {"kind": "markdown", "value": format!("**{}**", &line[start..end])}})
                }
                _ => Value::Null,
            },
            "textDocument/definition" => {
                let uri = params["textDocument"]["uri"].as_str()?.to_owned();
                let (_, line, start, end) = self.word_at(params)?;
                let word = &line[start..end];
                let lines = self.docs.get(&uri)?;
                let (n, text, byte) = lines
                    .iter()
                    .enumerate()
                    .find_map(|(n, l)| l.find(word).map(|b| (n, l, b)))?;
                json!({"uri": uri, "range": {"start": self.position(n, text, byte), "end": self.position(n, text, byte + word.len())}})
            }
            "textDocument/codeAction" => {
                let uri = params["textDocument"]["uri"].clone();
                let mut actions: Vec<Value> = params["context"]["diagnostics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|d| d["code"] == "todo")
                    .map(|d| {
                        json!({
                            "title": "Replace TODO with DONE",
                            "kind": "quickfix",
                            "isPreferred": true,
                            "diagnostics": [d],
                            "data": {"uri": uri, "range": d["range"]},
                        })
                    })
                    .collect();
                actions.push(json!({"title": "Insert header", "command": "fake.insertHeader", "arguments": [uri]}));
                Value::Array(actions)
            }
            "codeAction/resolve" => {
                let mut action = params.clone();
                let data = &params["data"];
                action["edit"] = json!({"changes": {data["uri"].as_str()?: [{"range": data["range"], "newText": "DONE"}]}});
                action
            }
            "workspace/executeCommand" => {
                let uri = params["arguments"][0].as_str()?.to_owned();
                send(&json!({
                    "jsonrpc": "2.0",
                    "id": "apply-1",
                    "method": "workspace/applyEdit",
                    "params": {"label": "Insert header", "edit": {"documentChanges": [{
                        "textDocument": {"uri": uri, "version": null},
                        "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "// header\n"}],
                    }]}},
                }));
                // Answered once the client applied the edit.
                self.executing = Some(id);
                return None;
            }
            _ => {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("unhandled {method}")},
                }));
            }
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
}

fn main() {
    let mut server = Server {
        utf8: std::env::var("FAKE_LSP_ENCODING").is_ok_and(|e| e == "utf-8"),
        docs: HashMap::new(),
        executing: None,
    };
    let mut input = BufReader::new(io::stdin().lock());
    while let Some(message) = read(&mut input) {
        let method = message["method"].as_str().unwrap_or_default().to_owned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (message.get("id").cloned(), method.as_str()) {
            // The client's answer to our applyEdit: finish the command.
            (Some(id), "") => {
                if id == "apply-1"
                    && let Some(command) = server.executing.take()
                {
                    let applied = message["result"]["applied"].clone();
                    send(&json!({"jsonrpc": "2.0", "id": command, "result": {"applied": applied}}));
                }
            }
            (Some(id), method) => {
                if let Some(answer) = server.request(id.clone(), method, &params) {
                    send(&answer);
                } else if server.executing.as_ref() != Some(&id) {
                    send(&json!({"jsonrpc": "2.0", "id": id, "result": null}));
                }
            }
            (None, "exit") => return,
            (None, "textDocument/didOpen") => {
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().unwrap_or_default().to_owned();
                server.set(&uri, doc["text"].as_str().unwrap_or_default());
                server.publish(&uri, false);
            }
            (None, "textDocument/didChange") => {
                let uri = params["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                for change in params["contentChanges"].as_array().into_iter().flatten() {
                    server.apply(&uri, change);
                }
                server.publish(&uri, true);
            }
            (None, "textDocument/didClose") => {
                let uri = params["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                server.docs.remove(&uri);
            }
            _ => {}
        }
    }
}
