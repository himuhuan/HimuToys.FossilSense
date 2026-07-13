use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
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

    #[cfg(windows)]
    fn private_bytes(&self) -> Option<u64> {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "(Get-Process -Id {} -ErrorAction Stop).PrivateMemorySize64",
                    self.child.id()
                ),
            ])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().parse().ok())?
    }

    #[cfg(not(windows))]
    fn private_bytes(&self) -> Option<u64> {
        None
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
    std::fs::write(root.join("CMakeLists.txt"), "project(lsp_smoke C)\n")?;
    std::fs::write(
        root.join("defs.h"),
        "#define VALUE 1\n/// @brief Helps the smoke test.\n/// <param name=\"unused\">structured param</param>\nvoid helper(int unused);\nint trailing_docs(void);\n",
    )?;
    let main_source =
        "#include \"defs.h\"\nint main(void) {\n    helper(0);\n    return VALUE;\n}\n";
    std::fs::write(root.join("main.c"), main_source)?;
    let live_source = "int live_caller(void) { helper(0); return 0; }\n";
    std::fs::write(root.join("live.c"), "int live_caller(void) { return 0; }\n")?;
    std::fs::write(
        root.join("ops_chain.h"),
        "/** Header lookup documentation. */\nint pair_lookup(int value);\n",
    )?;
    let ops_disk_source =
        "#include \"ops_chain.h\"\nint pair_lookup(int value) { return value; }\nvoid call_pair(void) { pair_lookup(1); }\nvoid use_pair(void) { pair_lookup(1); }\n";
    std::fs::write(root.join("ops_chain.c"), ops_disk_source)?;
    let ops_open_source =
        "#include \"ops_chain.h\"\nint pair_lookup(int value) { return value; }\nvoid call_pair(void) { pair_lookup(1); }\nvoid use_pair(void) { pair_lo }\n";
    std::fs::write(root.join("arity_one.h"), "int arity_pick(int value);\n")?;
    std::fs::write(
        root.join("arity_two.h"),
        "int arity_pick(int table, int value);\n",
    )?;
    std::fs::write(
        root.join("arity_two.c"),
        "#include \"arity_two.h\"\nint arity_pick(int table, int value) { return table + value; }\n",
    )?;
    let arity_source = concat!(
        "#include \"arity_one.h\"\n",
        "#include \"arity_two.h\"\n",
        "int use_complete(void) { return arity_pick(1, 2); }\n",
        "int use_partial(void) { return arity_pick(1, ); }\n",
    );
    std::fs::write(root.join("arity_main.c"), arity_source)?;
    std::fs::write(
        root.join("types.h"),
        "struct Packet {\n    int stale_disk_field;\n};\n\ntypedef const struct Packet *PacketView;\n",
    )?;
    let types_open_source = concat!(
        "struct Packet {\n",
        "    int type;\n",
        "    /* byte count */\n",
        "    int size;\n",
        "#ifdef WITH_CHECKSUM\n",
        "    int checksum;\n",
        "#endif\n",
        "};\n",
        "\n",
        "typedef const struct Packet *PacketView;\n",
    );
    // Keep trailing comments out of the on-disk declaration so indexing still
    // records `trailing_docs`; recover them from the open buffer instead.
    let defs_open_source =
        "#define VALUE 1\n/// @brief Helps the smoke test.\n/// <param name=\"unused\">structured param</param>\nvoid helper(int unused);\nint trailing_docs(void); // trailing hover comment\n";

    let root_uri = file_uri(root)?;
    let main_uri = file_uri(&root.join("main.c"))?;
    let defs_uri = file_uri(&root.join("defs.h"))?;
    let live_uri = file_uri(&root.join("live.c"))?;
    let ops_header_uri = file_uri(&root.join("ops_chain.h"))?;
    let ops_source_uri = file_uri(&root.join("ops_chain.c"))?;
    let arity_one_uri = file_uri(&root.join("arity_one.h"))?;
    let arity_two_source_uri = file_uri(&root.join("arity_two.c"))?;
    let arity_main_uri = file_uri(&root.join("arity_main.c"))?;
    let types_uri = file_uri(&root.join("types.h"))?;

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
    assert_eq!(
        initialized
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("hoverProvider"))
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        initialized
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("completionProvider"))
            .and_then(|completion| completion.get("resolveProvider"))
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        initialized
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("callHierarchyProvider"))
            .and_then(Value::as_bool),
        Some(true)
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
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": ops_source_uri,
                "languageId": "c",
                "version": 1,
                "text": ops_open_source
            }
        }),
    )?;
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": ops_header_uri,
                "languageId": "c",
                "version": 1,
                "text": "/** Header lookup documentation. */\nint pair_lookup(int value);\n"
            }
        }),
    )?;
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": arity_main_uri,
                "languageId": "c",
                "version": 1,
                "text": arity_source
            }
        }),
    )?;
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": types_uri,
                "languageId": "c",
                "version": 1,
                "text": types_open_source
            }
        }),
    )?;

    let project_status_id = lsp.request(
        "workspace/executeCommand",
        json!({
            "command": "fossilsense.lsp.projectContexts",
            "arguments": [{ "uri": ops_source_uri }]
        }),
    )?;
    let project_status = lsp.wait_response(project_status_id, Duration::from_secs(10))?;
    assert!(
        project_status
            .get("activeProject")
            .is_some_and(|value| !value.is_null()),
        "paired documentation test requires a discovered project, got {project_status}"
    );

    // didOpen reconciliation is deliberately asynchronous at the protocol
    // boundary. If another open document finishes reconciling between
    // completion and resolve, the v3 payload must be rejected as stale; a
    // fresh completion then carries the stable all-open overlay epoch.
    let mut paired_resolved = Value::Null;
    let mut paired_completion_docs = String::new();
    for _ in 0..4 {
        let paired_completion_id = lsp.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": ops_source_uri },
                "position": { "line": 3, "character": 29 },
                "context": { "triggerKind": 1 }
            }),
        )?;
        let paired_completion = lsp.wait_response(paired_completion_id, Duration::from_secs(10))?;
        let paired_item = paired_completion
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("label").and_then(Value::as_str) == Some("pair_lookup"))
            })
            .cloned()
            .context("paired completion missing pair_lookup")?;
        let paired_resolve_id = lsp.request("completionItem/resolve", paired_item)?;
        paired_resolved = lsp.wait_response(paired_resolve_id, Duration::from_secs(10))?;
        paired_completion_docs = paired_resolved
            .get("documentation")
            .and_then(|documentation| documentation.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if paired_completion_docs.contains("Header lookup documentation.") {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        paired_completion_docs.contains("Header lookup documentation.")
            && paired_completion_docs.contains("// In ops_chain.h")
            && paired_completion_docs.contains("int pair_lookup(int value);"),
        "source-file completion should use same-project header docs, got {paired_resolved}"
    );

    let paired_hover_id = lsp.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": ops_source_uri },
            "position": { "line": 1, "character": 6 }
        }),
    )?;
    let paired_hover = lsp.wait_response(paired_hover_id, Duration::from_secs(10))?;
    let paired_hover_value = paired_hover
        .get("contents")
        .and_then(|contents| contents.get("value"))
        .and_then(Value::as_str)
        .context("paired hover missing markdown")?;
    assert!(
        paired_hover_value.contains("Header lookup documentation.")
            && paired_hover_value.contains("// In ops_chain.h")
            && paired_hover_value.matches("// In ops_chain.h").count() == 1
            && !paired_hover_value.contains("// In ops_chain.c")
            && !paired_hover_value.contains("int pair_lookup(int value) {"),
        "a strict one-to-one group must render exactly its header presentation and no source section, got {paired_hover}"
    );

    let paired_signature_id = lsp.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": ops_source_uri },
            "position": { "line": 2, "character": 35 },
            "context": { "triggerKind": 1, "isRetrigger": false }
        }),
    )?;
    let paired_signature = lsp.wait_response(paired_signature_id, Duration::from_secs(10))?;
    assert_eq!(
        paired_signature
            .get("signatures")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "the strict header/source pair should produce one logical signature, got {paired_signature}"
    );
    assert!(
        paired_signature
            .get("signatures")
            .and_then(Value::as_array)
            .and_then(|signatures| signatures.first())
            .and_then(|signature| signature.get("documentation"))
            .and_then(|documentation| documentation.get("value"))
            .and_then(Value::as_str)
            .is_some_and(|documentation| documentation.contains("Header lookup documentation.")),
        "signature help in source should use same-project header docs, got {paired_signature}"
    );
    assert_eq!(
        paired_signature
            .get("signatures")
            .and_then(Value::as_array)
            .and_then(|signatures| signatures.first())
            .and_then(|signature| signature.get("label"))
            .and_then(Value::as_str),
        Some("int pair_lookup(int value);")
    );

    let source_jump_id = lsp.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": ops_source_uri },
            "position": { "line": 1, "character": 6 }
        }),
    )?;
    let source_jump = lsp.wait_response(source_jump_id, Duration::from_secs(10))?;
    let source_locations = source_jump
        .as_array()
        .context("source-anchor definition should return a location array")?;
    assert_eq!(
        source_locations.len(),
        1,
        "source anchor should expose only its proven header counterpart, got {source_jump}"
    );
    assert_eq!(
        source_locations
            .first()
            .and_then(|location| location.get("uri"))
            .and_then(Value::as_str),
        Some(ops_header_uri.as_str()),
        "goto on source definition should return the paired header declaration"
    );

    let header_jump_id = lsp.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": ops_header_uri },
            "position": { "line": 1, "character": 6 }
        }),
    )?;
    let header_jump = lsp.wait_response(header_jump_id, Duration::from_secs(10))?;
    let header_locations = header_jump
        .as_array()
        .context("header-anchor definition should return a location array")?;
    assert_eq!(
        header_locations.len(),
        1,
        "header anchor should expose only its proven source counterpart, got {header_jump}"
    );
    assert_eq!(
        header_locations
            .first()
            .and_then(|location| location.get("uri"))
            .and_then(Value::as_str),
        Some(ops_source_uri.as_str()),
        "goto on header declaration should return the paired source definition"
    );

    let call_jump_id = lsp.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": ops_source_uri },
            "position": { "line": 2, "character": 28 }
        }),
    )?;
    let call_jump = lsp.wait_response(call_jump_id, Duration::from_secs(10))?;
    assert_eq!(
        call_jump
            .as_array()
            .and_then(|locations| locations.first())
            .and_then(|location| location.get("uri"))
            .and_then(Value::as_str),
        Some(ops_source_uri.as_str()),
        "goto from an ordinary call site should keep the source definition first"
    );

    let complete_line = arity_source
        .lines()
        .position(|line| line.contains("use_complete"))
        .context("complete-call fixture line")? as u32;
    let complete_character = arity_source
        .lines()
        .nth(complete_line as usize)
        .and_then(|line| line.find("arity_pick"))
        .context("complete-call fixture callee")? as u32
        + 3;
    let arity_hover_id = lsp.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": arity_main_uri },
            "position": { "line": complete_line, "character": complete_character }
        }),
    )?;
    let arity_hover = lsp.wait_response(arity_hover_id, Duration::from_secs(10))?;
    let arity_hover_value = arity_hover
        .get("contents")
        .and_then(|contents| contents.get("value"))
        .and_then(Value::as_str)
        .context("complete-call hover missing markdown")?;
    assert!(
        arity_hover_value.contains("int arity_pick(int table, int value);")
            && !arity_hover_value.contains("int arity_pick(int value);"),
        "a complete two-argument call must exclude the proven one-argument candidate, got {arity_hover}"
    );

    let arity_definition_id = lsp.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": arity_main_uri },
            "position": { "line": complete_line, "character": complete_character }
        }),
    )?;
    let arity_definition = lsp.wait_response(arity_definition_id, Duration::from_secs(10))?;
    let arity_locations = arity_definition
        .as_array()
        .context("complete-call definition should return locations")?;
    assert!(
        !arity_locations.is_empty()
            && arity_locations.iter().all(|location| {
                location.get("uri").and_then(Value::as_str) != Some(arity_one_uri.as_str())
            }),
        "complete-call Definition must exclude the proven incompatible header, got {arity_definition}"
    );
    assert_eq!(
        arity_locations
            .first()
            .and_then(|location| location.get("uri"))
            .and_then(Value::as_str),
        Some(arity_two_source_uri.as_str()),
        "the compatible callable group should use its source definition at an ordinary call site"
    );

    let partial_line = arity_source
        .lines()
        .position(|line| line.contains("use_partial"))
        .context("partial-call fixture line")? as u32;
    let partial_character = arity_source
        .lines()
        .nth(partial_line as usize)
        .and_then(|line| line.find("arity_pick(1, "))
        .context("partial-call fixture arguments")? as u32
        + "arity_pick(1, ".len() as u32;
    let arity_signature_id = lsp.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": arity_main_uri },
            "position": { "line": partial_line, "character": partial_character },
            "context": { "triggerKind": 2, "triggerCharacter": ",", "isRetrigger": false }
        }),
    )?;
    let arity_signature = lsp.wait_response(arity_signature_id, Duration::from_secs(10))?;
    let arity_signatures = arity_signature
        .get("signatures")
        .and_then(Value::as_array)
        .context("partial-call signature help missing signatures")?;
    let one_signature = arity_signatures
        .iter()
        .find(|signature| {
            signature.get("label").and_then(Value::as_str) == Some("int arity_pick(int value);")
        })
        .context("partial call should retain the still-compatible one-argument signature")?;
    let two_signature = arity_signatures
        .iter()
        .find(|signature| {
            signature.get("label").and_then(Value::as_str)
                == Some("int arity_pick(int table, int value);")
        })
        .context("partial call should retain the two-argument signature")?;
    assert_eq!(
        one_signature.get("activeParameter").and_then(Value::as_u64),
        None,
        "the empty second argument is outside the one-argument signature"
    );
    assert_eq!(
        two_signature.get("activeParameter").and_then(Value::as_u64),
        Some(1),
        "the empty second argument should activate parameter 1 of the two-argument signature"
    );
    let first_signature = arity_signatures
        .first()
        .context("partial-call signature list should not be empty")?;
    let expected_active_parameter = (first_signature.get("label").and_then(Value::as_str)
        == Some("int arity_pick(int table, int value);"))
    .then_some(1);
    assert_eq!(
        arity_signature
            .get("activeParameter")
            .and_then(Value::as_u64),
        expected_active_parameter,
        "top-level activeParameter must describe activeSignature rather than another candidate"
    );

    let packet_character = types_open_source
        .lines()
        .next()
        .and_then(|line| line.find("Packet"))
        .context("record fixture name")? as u32
        + 2;
    let record_hover_id = lsp.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": types_uri },
            "position": { "line": 0, "character": packet_character }
        }),
    )?;
    let record_hover = lsp.wait_response(record_hover_id, Duration::from_secs(10))?;
    let record_hover_value = record_hover
        .get("contents")
        .and_then(|contents| contents.get("value"))
        .and_then(Value::as_str)
        .context("record hover missing markdown")?;
    assert!(
        record_hover_value.contains("### struct `Packet`")
            && record_hover_value.contains("struct Packet {\n")
            && record_hover_value.contains("    /* byte count */\n")
            && record_hover_value.contains("#ifdef WITH_CHECKSUM\n")
            && record_hover_value.contains("    int checksum;\n")
            && record_hover_value.contains("#endif\n};")
            && !record_hover_value.contains("stale_disk_field"),
        "record Hover must preserve the full multiline dirty-buffer definition and shadow stale disk facts, got {record_hover}"
    );

    let alias_line = types_open_source
        .lines()
        .position(|line| line.contains("typedef"))
        .context("typedef fixture line")? as u32;
    let alias_character = types_open_source
        .lines()
        .nth(alias_line as usize)
        .and_then(|line| line.find("PacketView"))
        .context("typedef fixture name")? as u32
        + 2;
    let alias_hover_id = lsp.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": types_uri },
            "position": { "line": alias_line, "character": alias_character }
        }),
    )?;
    let alias_hover = lsp.wait_response(alias_hover_id, Duration::from_secs(10))?;
    let alias_hover_value = alias_hover
        .get("contents")
        .and_then(|contents| contents.get("value"))
        .and_then(Value::as_str)
        .context("typedef hover missing markdown")?;
    assert!(
        alias_hover_value.contains("### typedef `PacketView`")
            && alias_hover_value.contains("typedef const struct Packet *PacketView;")
            && alias_hover_value.contains("`(aka. const struct Packet *)`")
            && alias_hover_value.contains("### struct `Packet`")
            && alias_hover_value.contains("#ifdef WITH_CHECKSUM\n")
            && alias_hover_value.contains("    int checksum;\n")
            && !alias_hover_value.contains("stale_disk_field"),
        "typedef Hover must show the exact aka spelling plus the complete dirty terminal record, got {alias_hover}"
    );
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": defs_uri,
                "languageId": "c",
                "version": 1,
                "text": defs_open_source
            }
        }),
    )?;
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": live_uri,
                "languageId": "c",
                "version": 1,
                "text": live_source
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
    let helper_completion = items
        .iter()
        .find(|item| item.get("label").and_then(Value::as_str) == Some("helper"))
        .cloned()
        .context("completion should include helper")?;
    assert!(
        helper_completion.get("data").is_some(),
        "completion should include helper, got {completion}"
    );

    let completion_resolve_id = lsp.request("completionItem/resolve", helper_completion)?;
    let resolved_completion = lsp.wait_response(completion_resolve_id, Duration::from_secs(10))?;
    let resolved_completion_docs = resolved_completion
        .get("documentation")
        .and_then(|documentation| documentation.get("value"))
        .and_then(Value::as_str)
        .context("resolved completion missing Markdown documentation")?;
    assert!(
        resolved_completion_docs.contains("Helps the smoke test.")
            && resolved_completion_docs.contains("### Parameters")
            && resolved_completion_docs.contains("structured param"),
        "resolved completion should render declaration comments, got {resolved_completion}"
    );

    let signature_id = lsp.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 2, "character": 11 },
            "context": { "triggerKind": 1, "isRetrigger": false }
        }),
    )?;
    let signature = lsp.wait_response(signature_id, Duration::from_secs(10))?;
    let signature_entry = signature
        .get("signatures")
        .and_then(Value::as_array)
        .and_then(|signatures| signatures.first())
        .context("signature help missing signature")?;
    let signature_docs = signature_entry
        .get("documentation")
        .and_then(|documentation| documentation.get("value"))
        .and_then(Value::as_str)
        .context("signature help missing Markdown documentation")?;
    let parameter_docs = signature_entry
        .get("parameters")
        .and_then(Value::as_array)
        .and_then(|parameters| parameters.first())
        .and_then(|parameter| parameter.get("documentation"))
        .and_then(|documentation| documentation.get("value"))
        .and_then(Value::as_str)
        .context("signature parameter missing Markdown documentation")?;
    assert!(signature_docs.contains("Helps the smoke test."));
    assert!(parameter_docs.contains("structured param"));

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

    let hover_id = lsp.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 2, "character": 7 }
        }),
    )?;
    let hover = lsp.wait_response(hover_id, Duration::from_secs(10))?;
    let hover_value = hover
        .get("contents")
        .and_then(|contents| contents.get("value"))
        .and_then(Value::as_str)
        .context("hover response missing markdown value")?;
    assert!(
        hover_value.contains("void helper(int unused);")
            && hover_value.contains("Helps the smoke test.")
            && hover_value.contains("### Parameters")
            && hover_value.contains("- `unused` — structured param")
            && hover_value.contains("// In defs.h")
            && hover_value.contains("tier: reachable"),
        "hover should include signature, structured comment, and ranking evidence, got {hover}"
    );

    let trailing_hover_id = lsp.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": defs_uri },
            "position": { "line": 4, "character": 8 }
        }),
    )?;
    let trailing_hover = lsp.wait_response(trailing_hover_id, Duration::from_secs(10))?;
    let trailing_value = trailing_hover
        .get("contents")
        .and_then(|contents| contents.get("value"))
        .and_then(Value::as_str)
        .context("trailing hover response missing markdown value")?;
    assert!(
        trailing_value.contains("trailing hover comment")
            && trailing_value.contains("int trailing_docs(void);"),
        "hover should recover trailing same-line comments from the open buffer, got {trailing_hover}"
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

    let prepare_id = lsp.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 1, "character": 5 }
        }),
    )?;
    let prepared = lsp.wait_response(prepare_id, Duration::from_secs(10))?;
    let main_item = prepared
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .context("call hierarchy should prepare main")?;
    assert_eq!(main_item.get("name").and_then(Value::as_str), Some("main"));

    let outgoing_id = lsp.request("callHierarchy/outgoingCalls", json!({ "item": main_item }))?;
    let outgoing = lsp.wait_response(outgoing_id, Duration::from_secs(10))?;
    assert!(
        outgoing
            .as_array()
            .is_some_and(|items| items.iter().any(|call| {
                call.get("to")
                    .and_then(|item| item.get("name"))
                    .and_then(Value::as_str)
                    == Some("helper")
            })),
        "main should have a standard outgoing helper call, got {outgoing}"
    );

    let call_root_id = lsp.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 2, "character": 7 }
        }),
    )?;
    let call_root = lsp.wait_response(call_root_id, Duration::from_secs(10))?;
    assert_eq!(
        call_root
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("name"))
            .and_then(Value::as_str),
        Some("helper"),
        "callee token should prepare the called function before the enclosing caller"
    );

    let body_root_id = lsp.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 3, "character": 5 }
        }),
    )?;
    let body_root = lsp.wait_response(body_root_id, Duration::from_secs(10))?;
    assert_eq!(
        body_root
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("name"))
            .and_then(Value::as_str),
        Some("main"),
        "a function-body position should fall back to the enclosing callable"
    );

    let helper_prepare_id = lsp.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": defs_uri },
            "position": { "line": 3, "character": 7 }
        }),
    )?;
    let helper_prepared = lsp.wait_response(helper_prepare_id, Duration::from_secs(10))?;
    let helper_item = helper_prepared
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .context("call hierarchy should prepare helper declaration")?;
    let incoming_id = lsp.request(
        "callHierarchy/incomingCalls",
        json!({ "item": helper_item }),
    )?;
    let incoming = lsp.wait_response(incoming_id, Duration::from_secs(10))?;
    assert!(
        incoming
            .as_array()
            .is_some_and(|items| items.iter().any(|call| {
                call.get("from")
                    .and_then(|item| item.get("name"))
                    .and_then(Value::as_str)
                    == Some("live_caller")
            })),
        "helper should see an incoming call from another unsaved open document, got {incoming}"
    );

    let overlay_private_before = lsp.private_bytes();
    for revision in 2..=101 {
        let has_call = revision % 2 == 0;
        let text = if has_call {
            "int live_caller(void) { helper(0); return 0; }\n"
        } else {
            "int live_caller(void) { return 0; }\n"
        };
        lsp.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": live_uri, "version": revision },
                "contentChanges": [{ "text": text }]
            }),
        )?;
        let incoming_id = lsp.request(
            "callHierarchy/incomingCalls",
            json!({ "item": helper_item }),
        )?;
        let edited_incoming = lsp.wait_response(incoming_id, Duration::from_secs(10))?;
        let sees_live = edited_incoming.as_array().is_some_and(|items| {
            items.iter().any(|call| {
                call.get("from")
                    .and_then(|item| item.get("name"))
                    .and_then(Value::as_str)
                    == Some("live_caller")
            })
        });
        assert_eq!(
            sees_live, has_call,
            "overlay revision {revision} must shadow the base file without stale relations"
        );
    }
    if let (Some(before), Some(after)) = (overlay_private_before, lsp.private_bytes()) {
        eprintln!(
            "call_overlay_memory cycles=100 private_before_bytes={before} private_after_bytes={after} delta_bytes={}",
            after.saturating_sub(before)
        );
        assert!(
            after <= before + 32 * 1024 * 1024,
            "100 overlay edit/query cycles retained too much private memory: before={before}, after={after}"
        );
    }

    let rich_id = lsp.request(
        "workspace/executeCommand",
        json!({
            "command": "fossilsense.lsp.callRelations",
            "arguments": [{ "uri": main_uri, "line": 1, "character": 5, "direction": "outgoing" }]
        }),
    )?;
    let rich = lsp.wait_response(rich_id, Duration::from_secs(10))?;
    assert_eq!(rich.get("protocolVersion").and_then(Value::as_u64), Some(2));
    assert!(rich.get("entities").is_some());
    assert_eq!(rich.get("complete").and_then(Value::as_bool), Some(true));
    assert_eq!(
        rich.get("budgetState").and_then(Value::as_str),
        Some("complete")
    );
    assert!(rich.get("coverage").is_some());
    assert!(
        contains_uri(&rich, "helper"),
        "rich relations should retain target and evidence, got {rich}"
    );

    lsp.shutdown()?;
    Ok(())
}

#[test]
fn lsp_completion_prefers_indexed_printf_over_typed_word_prefix() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    std::fs::create_dir_all(root.join("src"))?;
    let external = tempfile::tempdir()?;
    std::fs::write(
        external.path().join("stdio.h"),
        "int printf(const char *fmt);\n",
    )?;

    let main_source = "#include <stdio.h>\nint main(void) {\n    prin\n}\n";
    std::fs::write(root.join("src/main.c"), main_source)?;

    let root_uri = file_uri(root)?;
    let main_uri = file_uri(&root.join("src/main.c"))?;
    let include_path = external.path().to_string_lossy().replace('\\', "/");

    let mut lsp = LspProcess::start()?;
    let init_id = lsp.request(
        "initialize",
        json!({
            "processId": null,
            "rootUri": root_uri,
            "workspaceFolders": [{
                "uri": root_uri,
                "name": "printf-completion"
            }],
            "capabilities": {},
            "initializationOptions": {
                "fossilsense": {
                    "completion": { "mode": "on" },
                    "semanticColoring": { "mode": "on" },
                    "includeScoping": { "mode": "auto" },
                    "includePaths": [include_path],
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
            "position": { "line": 2, "character": 8 },
            "context": { "triggerKind": 1 }
        }),
    )?;
    let completion = lsp.wait_response(completion_id, Duration::from_secs(10))?;
    let items = completion
        .get("items")
        .and_then(Value::as_array)
        .context("completion response missing items")?;
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|item| item.get("label").and_then(Value::as_str))
        .collect();

    assert_eq!(
        labels.first().copied(),
        Some("printf"),
        "indexed printf should rank before the typed word prefix, got {completion}"
    );
    assert!(
        !labels.contains(&"prin"),
        "the in-progress prefix should not be returned as a local word candidate, got {completion}"
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

fn path_to_file_uri(path: &Path) -> String {
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
