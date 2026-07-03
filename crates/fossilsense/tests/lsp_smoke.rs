use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

struct LspProcess {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Result<Value, String>>,
    next_id: u64,
}

impl LspProcess {
    fn start() -> Result<Self> {
        let bin = env!("CARGO_BIN_EXE_fossilsense");
        let mut child = Command::new(bin)
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn fossilsense lsp")?;

        let stdin = child.stdin.take().context("missing child stdin")?;
        let stdout = child.stdout.take().context("missing child stdout")?;
        let messages = spawn_reader(stdout);

        Ok(Self {
            child,
            stdin,
            messages,
            next_id: 1,
        })
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        Ok(id)
    }

    fn write(&mut self, message: &Value) -> Result<()> {
        let body = serde_json::to_vec(message)?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())?;
        self.stdin.write_all(&body)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn wait_response(&mut self, id: u64, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let message = self.recv_until(deadline, &format!("response id {id}"))?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    bail!("LSP request {id} failed: {error}");
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn wait_index_ready(&mut self, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        loop {
            let message = match self.recv_until(deadline, "fossilsense/indexStatus ready") {
                Ok(message) => message,
                Err(err) => {
                    bail!("{err:#}; recent messages: {}", seen.join(" | "));
                }
            };
            if seen.len() < 20 {
                seen.push(compact_message(&message));
            }
            if message.get("method").and_then(Value::as_str) == Some("fossilsense/indexStatus") {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if params.get("state").and_then(Value::as_str) == Some("ready") {
                    return Ok(params);
                }
                if params.get("state").and_then(Value::as_str) == Some("failed") {
                    bail!("index failed: {params}");
                }
            }
        }
    }

    fn recv_until(&mut self, deadline: Instant, label: &str) -> Result<Value> {
        let now = Instant::now();
        if now >= deadline {
            bail!("timed out waiting for {label}");
        }
        match self
            .messages
            .recv_timeout(deadline.saturating_duration_since(now))
        {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(err)) => bail!("LSP reader failed while waiting for {label}: {err}"),
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("timed out waiting for {label}"),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("LSP reader disconnected while waiting for {label}")
            }
        }
    }

    fn shutdown(mut self) -> Result<()> {
        let id = self.request("shutdown", Value::Null)?;
        let _ = self.wait_response(id, Duration::from_secs(5));
        let _ = self.notify("exit", Value::Null);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

fn compact_message(message: &Value) -> String {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        if method == "window/logMessage" {
            let text = message
                .get("params")
                .and_then(|p| p.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("");
            return format!("{method}:{text}");
        }
        if method == "fossilsense/indexStatus" {
            return format!("{method}:{}", message.get("params").unwrap_or(&Value::Null));
        }
        return method.to_string();
    }
    if let Some(id) = message.get("id") {
        return format!("response:{id}");
    }
    message.to_string()
}

fn spawn_reader(stdout: ChildStdout) -> Receiver<Result<Value, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        loop {
            match read_message(&mut stdout) {
                Ok(message) => {
                    if sender.send(Ok(message)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let _ = sender.send(Err(format!("{err:#}")));
                    return;
                }
            }
        }
    });
    receiver
}

fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut content_len = None;
    loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line)?;
        if n == 0 {
            bail!("LSP server closed stdout");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_len = Some(value.trim().parse::<usize>()?);
        }
    }

    let len = content_len.context("LSP message missing Content-Length")?;
    let mut body = vec![0; len];
    stdout.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn lsp_smoke_completion_definition_and_references() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    std::fs::write(root.join("defs.h"), "#define VALUE 1\nvoid helper(void);\n")?;
    let main_source =
        "#include \"defs.h\"\nint main(void) {\n    helper();\n    return VALUE;\n}\n";
    std::fs::write(root.join("main.c"), main_source)?;

    let root_uri = file_uri(root)?;
    let main_uri = file_uri(&root.join("main.c"))?;

    let mut lsp = LspProcess::start()?;
    let init_id = lsp.request(
        "initialize",
        json!({
            "processId": null,
            "rootUri": root_uri,
            "workspaceFolders": [{
                "uri": root_uri,
                "name": "lsp-smoke"
            }],
            "capabilities": {},
            "initializationOptions": {
                "fossilsense": {
                    "completion": { "mode": "on" },
                    "semanticColoring": { "mode": "on" },
                    "includeScoping": { "mode": "auto" },
                    "includePaths": [],
                    "debug": {
                        "candidateReasons": false,
                        "perfLogs": false
                    }
                }
            }
        }),
    )?;
    let initialized = lsp.wait_response(init_id, Duration::from_secs(10))?;
    assert_eq!(
        initialized
            .get("serverInfo")
            .and_then(|info| info.get("name"))
            .and_then(Value::as_str),
        Some("FossilSense")
    );

    lsp.notify("initialized", json!({}))?;
    let ready = lsp.wait_index_ready(Duration::from_secs(30))?;
    assert_eq!(ready.get("state").and_then(Value::as_str), Some("ready"));
    assert_eq!(
        ready
            .get("degradedCapabilities")
            .and_then(|d| d.get("reachGraph"))
            .and_then(Value::as_bool),
        Some(false)
    );

    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": main_uri,
                "languageId": "c",
                "version": 1,
                "text": main_source
            }
        }),
    )?;

    let completion_id = lsp.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 2, "character": 5 },
            "context": { "triggerKind": 1 }
        }),
    )?;
    let completion = lsp.wait_response(completion_id, Duration::from_secs(10))?;
    let items = completion
        .get("items")
        .and_then(Value::as_array)
        .context("completion response missing items")?;
    assert!(
        items
            .iter()
            .any(|item| item.get("label").and_then(Value::as_str) == Some("helper")),
        "completion should include helper, got {completion}"
    );

    let definition_id = lsp.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 2, "character": 7 }
        }),
    )?;
    let definition = lsp.wait_response(definition_id, Duration::from_secs(10))?;
    assert!(
        contains_uri(&definition, "defs.h"),
        "definition should include defs.h, got {definition}"
    );

    let references_id = lsp.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 2, "character": 7 },
            "context": { "includeDeclaration": true }
        }),
    )?;
    let references = lsp.wait_response(references_id, Duration::from_secs(10))?;
    let reference_count = references.as_array().map_or(0, Vec::len);
    assert!(
        reference_count >= 2,
        "references should include declaration and call, got {references}"
    );

    lsp.shutdown()?;
    Ok(())
}

fn contains_uri(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(s) => s.contains(needle),
        Value::Array(values) => values.iter().any(|value| contains_uri(value, needle)),
        Value::Object(map) => map.values().any(|value| contains_uri(value, needle)),
        _ => false,
    }
}

fn file_uri(path: &Path) -> Result<String> {
    let absolute = path.canonicalize()?;
    Ok(path_to_file_uri(&absolute))
}

fn path_to_file_uri(path: &PathBuf) -> String {
    let mut slash = path.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = slash.strip_prefix("//?/") {
        slash = stripped.to_string();
    }
    if slash.starts_with('/') {
        format!("file://{}", percent_encode_uri_path(&slash))
    } else {
        format!("file:///{}", percent_encode_uri_path(&slash))
    }
}

fn percent_encode_uri_path(path: &str) -> String {
    path.bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
