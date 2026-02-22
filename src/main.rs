use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write as _};
use std::path::PathBuf;
use std::process::{self, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// ── constants ──────────────────────────────────────────────────────────────────
const HARD_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_OUTPUT_BYTES: usize = 2000;
const MAX_TOOL_ROUNDS: usize = 10;
const ALLOWED_COMMANDS: &[&str] = &[
    "ls", "grep", "cat", "find", "head", "tail", "tree", "file", "stat", "which", "wc", "du",
];

// ── API backend detection ──────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq)]
enum ApiBackend {
    OpenAI,
    Anthropic,
}

fn detect_backend(api_base: &str) -> ApiBackend {
    if api_base.contains("anthropic.com") {
        ApiBackend::Anthropic
    } else {
        ApiBackend::OpenAI
    }
}

// ── unified API result ─────────────────────────────────────────────────────────
struct ToolCallInfo {
    id: String,
    name: String,
    args: Value,
}

enum ApiResult {
    Text(String),
    ToolCalls(Vec<ToolCallInfo>),
    Empty,
}

// ── OpenAI response structs ────────────────────────────────────────────────────
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: MessageOut,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct MessageOut {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ToolCall {
    id: String,
    function: FnCall,
}

#[derive(Deserialize)]
struct FnCall {
    name: String,
    arguments: String,
}

// ── Anthropic response structs ─────────────────────────────────────────────────
#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    r#type: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

// ── shared structs ─────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct RunCmdArgs {
    command: String,
    args: Option<Vec<String>>,
}

// ── config persistence ─────────────────────────────────────────────────────────
fn config_path() -> PathBuf {
    let base = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut p = PathBuf::from(env::var("HOME").unwrap_or_else(|_| ".".into()));
            p.push(".config");
            p
        });
    base.join("llmc").join("config.json")
}

fn load_config() -> Value {
    let path = config_path();
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}))
}

fn save_config(config: &Value) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(config) {
        if fs::write(&path, s).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
        }
    }
}

fn prompt_stderr(msg: &str) -> String {
    let tty = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty");
    match tty {
        Ok(tty_file) => {
            let mut writer = io::BufWriter::new(&tty_file);
            let _ = writer.write_all(msg.as_bytes());
            let _ = writer.flush();
            let mut reader = io::BufReader::new(&tty_file);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            line.trim().to_string()
        }
        Err(_) => {
            eprint!("{msg}");
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            line.trim().to_string()
        }
    }
}

fn resolve_api_key() -> String {
    // 1. Environment variable
    if let Ok(key) = env::var("LLM_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }

    // 2. Config file
    let config = load_config();
    if let Some(key) = config["api_key"].as_str() {
        if !key.is_empty() {
            return key.to_string();
        }
    }

    // 3. Interactive setup
    interactive_setup()
}

fn interactive_setup() -> String {
    eprintln!("llmc: initial setup");
    eprintln!();
    eprintln!("Select API provider:");
    eprintln!("  1) ChatGPT (OpenAI)");
    eprintln!("  2) Claude (Anthropic)");
    eprintln!("  3) Gemini (Google)");
    eprintln!("  4) Other (manual input)");
    eprintln!();

    let choice = prompt_stderr("Choice [1-4]: ");
    eprintln!();

    let (api_base, model, api_key) = match choice.as_str() {
        "1" => setup_preset(
            "https://api.openai.com/v1",
            &[
                ("gpt-5-mini", "recommended"),
                ("gpt-5.2", "high performance"),
                ("gpt-4.1-mini", "legacy, cheap"),
            ],
        ),
        "2" => setup_preset(
            "https://api.anthropic.com",
            &[
                ("claude-haiku-4-5-20251001", "recommended"),
                ("claude-sonnet-4-5-20250929", "balanced"),
                ("claude-opus-4-5-20251101", "high performance"),
            ],
        ),
        "3" => setup_preset(
            "https://generativelanguage.googleapis.com/v1beta/openai",
            &[
                ("gemini-2.5-flash-lite", "recommended"),
                ("gemini-2.5-flash", "balanced"),
                ("gemini-2.5-pro", "high performance"),
            ],
        ),
        _ => setup_custom(),
    };

    let config = json!({
        "api_key": api_key,
        "api_base": api_base,
        "model": model,
    });
    save_config(&config);

    let path = config_path();
    eprintln!("llmc: config saved -> {}", path.display());
    eprintln!();

    api_key
}

fn setup_preset(api_base: &str, models: &[(&str, &str)]) -> (String, String, String) {
    eprintln!("Select model:");
    for (i, (name, desc)) in models.iter().enumerate() {
        eprintln!("  {}) {} ({})", i + 1, name, desc);
    }
    let manual = models.len() + 1;
    eprintln!("  {manual}) Enter manually");
    eprintln!();

    let model_choice = prompt_stderr(&format!("Choice [1-{manual}]: "));
    let model_idx: usize = model_choice.parse().unwrap_or(1);

    let model = if model_idx >= 1 && model_idx <= models.len() {
        models[model_idx - 1].0.to_string()
    } else {
        eprintln!();
        let m = prompt_stderr("Model name: ");
        if m.is_empty() {
            eprintln!("llmc: model name is empty.");
            process::exit(1);
        }
        m
    };

    eprintln!();
    let api_key = prompt_stderr("API Key: ");
    if api_key.is_empty() {
        eprintln!("llmc: API key is empty.");
        process::exit(1);
    }

    (api_base.to_string(), model, api_key)
}

fn setup_custom() -> (String, String, String) {
    let api_base = prompt_stderr("API Base URL: ");
    if api_base.is_empty() {
        eprintln!("llmc: API base URL is empty.");
        process::exit(1);
    }
    eprintln!();

    let model = prompt_stderr("Model name: ");
    if model.is_empty() {
        eprintln!("llmc: model name is empty.");
        process::exit(1);
    }
    eprintln!();

    let api_key = prompt_stderr("API Key: ");
    if api_key.is_empty() {
        eprintln!("llmc: API key is empty.");
        process::exit(1);
    }

    (api_base, model, api_key)
}

fn resolve_config_field(env_var: &str, config_key: &str, default: &str) -> String {
    env::var(env_var)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            load_config()[config_key]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(default)
                .to_string()
        })
}

// ── spinner ────────────────────────────────────────────────────────────────────
struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Spinner {
    fn start(msg: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let msg = msg.to_string();

        let handle = thread::spawn(move || {
            const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while !stop_clone.load(Ordering::Relaxed) {
                eprint!("\r\x1b[2K{} {}", FRAMES[i % FRAMES.len()], msg);
                i += 1;
                thread::sleep(Duration::from_millis(80));
            }
            eprint!("\r\x1b[2K");
        });

        Spinner {
            stop,
            handle: Some(handle),
        }
    }

    fn stop(self) {}
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ── tool schemas ───────────────────────────────────────────────────────────────
fn tool_schema_openai() -> Value {
    let allowed = ALLOWED_COMMANDS.join(", ");
    json!([{
        "type": "function",
        "function": {
            "name": "run_readonly_command",
            "description": format!("Execute a read-only command on the local system to inspect files, directories, or text. Only whitelisted commands are allowed: {allowed}."),
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command binary to run (e.g. \"ls\", \"grep\")"
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Arguments to pass to the command"
                    }
                },
                "required": ["command"]
            }
        }
    }])
}

fn tool_schema_anthropic() -> Value {
    let allowed = ALLOWED_COMMANDS.join(", ");
    json!([{
        "name": "run_readonly_command",
        "description": format!("Execute a read-only command on the local system to inspect files, directories, or text. Only whitelisted commands are allowed: {allowed}."),
        "input_schema": {
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command binary to run (e.g. \"ls\", \"grep\")"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments to pass to the command"
                }
            },
            "required": ["command"]
        }
    }])
}

// ── system prompt ──────────────────────────────────────────────────────────────
fn system_prompt() -> String {
    let cwd = env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());
    let shell = env::var("SHELL").unwrap_or_else(|_| "bash".into());
    let os = env::consts::OS;

    format!(
        "You are a shell command generator. The user describes what they want to do in natural language. \
         Your job is to produce the EXACT shell command they need.\n\n\
         Environment:\n- OS: {os}\n- Shell: {shell}\n- CWD: {cwd}\n\n\
         You may call the `run_readonly_command` tool to inspect the local filesystem before answering \
         (e.g. list files, read configs). Only use it when the user's request requires local context.\n\n\
         Rules:\n\
         1. Your final answer MUST be a single shell command (or pipeline) — nothing else.\n\
         2. Do NOT wrap the command in markdown code fences or quotes.\n\
         3. Do NOT include any explanation, commentary, or surrounding text.\n\
         4. If you cannot produce a valid command, output exactly: echo \"ERROR: unable to generate command\""
    )
}

// ── sandbox executor ───────────────────────────────────────────────────────────
fn exec_sandboxed(cmd: &str, args: &[String], deadline: Instant) -> String {
    if !ALLOWED_COMMANDS.contains(&cmd) {
        return format!("Permission Denied: '{cmd}' is not in the allowed command list.");
    }

    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return "Error: timeout reached".into();
    }

    let result = Command::new(cmd)
        .args(args)
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn();

    let mut child = match result {
        Ok(c) => c,
        Err(e) => return format!("Error: {e}"),
    };

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => return format!("Error: {e}"),
    };

    let mut stdout_buf = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_end(&mut stdout_buf);
    }

    let mut output = if stdout_buf.len() > MAX_OUTPUT_BYTES {
        let mut s = String::from_utf8_lossy(&stdout_buf[..MAX_OUTPUT_BYTES]).into_owned();
        s.push_str("...(truncated)");
        s
    } else {
        String::from_utf8_lossy(&stdout_buf).into_owned()
    };

    if !status.success() {
        let mut stderr_buf = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_end(&mut stderr_buf);
        }
        let stderr_str = String::from_utf8_lossy(&stderr_buf);
        output.push_str(&format!(
            "\n[exit {}] {}",
            status.code().unwrap_or(-1),
            stderr_str
        ));
    }

    output
}

// ── OpenAI API call ────────────────────────────────────────────────────────────
fn call_openai(
    agent: &ureq::Agent,
    api_base: &str,
    model: &str,
    api_key: &str,
    messages: &[Value],
    tools: &Value,
) -> ApiResult {
    let body = json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "temperature": 0,
    });

    let resp = agent
        .post(&format!("{api_base}/chat/completions"))
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(&body);

    let text = match resp {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(e) => {
            eprintln!("llmc: API request failed: {e}");
            process::exit(1);
        }
    };

    let parsed: ChatResponse = serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("llmc: failed to parse API response: {e}");
        eprintln!("llmc: raw response: {}", &text[..text.len().min(500)]);
        process::exit(1);
    });

    if parsed.choices.is_empty() {
        return ApiResult::Empty;
    }

    let choice = &parsed.choices[0];
    let msg = &choice.message;

    if let Some(tool_calls) = &msg.tool_calls {
        let calls = tool_calls
            .iter()
            .map(|tc| {
                let args = serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
                ToolCallInfo {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    args,
                }
            })
            .collect();
        return ApiResult::ToolCalls(calls);
    }

    if let Some(content) = &msg.content {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return ApiResult::Text(trimmed.to_string());
        }
    }

    ApiResult::Empty
}

// ── Anthropic API call ─────────────────────────────────────────────────────────
fn call_anthropic(
    agent: &ureq::Agent,
    api_base: &str,
    model: &str,
    api_key: &str,
    system: &str,
    messages: &[Value],
    tools: &Value,
) -> ApiResult {
    let body = json!({
        "model": model,
        "system": system,
        "messages": messages,
        "tools": tools,
        "max_tokens": 4096,
        "temperature": 0,
    });

    let url = format!("{}/v1/messages", api_base.trim_end_matches('/'));

    let resp = agent
        .post(&url)
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("Content-Type", "application/json")
        .send_json(&body);

    let text = match resp {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(e) => {
            eprintln!("llmc: API request failed: {e}");
            process::exit(1);
        }
    };

    let parsed: AnthropicResponse = serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("llmc: failed to parse API response: {e}");
        eprintln!("llmc: raw response: {}", &text[..text.len().min(500)]);
        process::exit(1);
    });

    let mut tool_calls = Vec::new();
    let mut text_parts = Vec::new();

    for block in &parsed.content {
        match block.r#type.as_str() {
            "tool_use" => {
                if let (Some(id), Some(name)) = (&block.id, &block.name) {
                    tool_calls.push(ToolCallInfo {
                        id: id.clone(),
                        name: name.clone(),
                        args: block.input.clone().unwrap_or(json!({})),
                    });
                }
            }
            "text" => {
                if let Some(t) = &block.text {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        text_parts.push(trimmed.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    if !tool_calls.is_empty() {
        return ApiResult::ToolCalls(tool_calls);
    }

    if !text_parts.is_empty() {
        return ApiResult::Text(text_parts.join("\n"));
    }

    ApiResult::Empty
}

// ── message history helpers ────────────────────────────────────────────────────

/// Append assistant response with tool calls to OpenAI message history
fn openai_push_assistant_tool_calls(messages: &mut Vec<Value>, calls: &[ToolCallInfo]) {
    let tc_json: Vec<Value> = calls
        .iter()
        .map(|tc| {
            json!({
                "id": tc.id,
                "type": "function",
                "function": {
                    "name": tc.name,
                    "arguments": tc.args.to_string(),
                }
            })
        })
        .collect();
    messages.push(json!({
        "role": "assistant",
        "content": null,
        "tool_calls": tc_json,
    }));
}

/// Append tool result to OpenAI message history
fn openai_push_tool_result(messages: &mut Vec<Value>, tool_call_id: &str, result: &str) {
    messages.push(json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": result,
    }));
}

/// Append assistant response with tool calls to Anthropic message history
fn anthropic_push_assistant_tool_calls(messages: &mut Vec<Value>, calls: &[ToolCallInfo]) {
    let content: Vec<Value> = calls
        .iter()
        .map(|tc| {
            json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": tc.args,
            })
        })
        .collect();
    messages.push(json!({
        "role": "assistant",
        "content": content,
    }));
}

/// Append tool results to Anthropic message history (all in one user message)
fn anthropic_push_tool_results(messages: &mut Vec<Value>, results: &[(String, String)]) {
    let content: Vec<Value> = results
        .iter()
        .map(|(id, result)| {
            json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": result,
            })
        })
        .collect();
    messages.push(json!({
        "role": "user",
        "content": content,
    }));
}

// ── embedded shell integration scripts ──────────────────────────────────────────
const SETUP_BASH: &str = r#"# llmc: Bash integration — source this file in your .bashrc
# Usage: Press Ctrl+E with a natural language description on the command line

_ai_cmd_replace() {
  [[ -z "$READLINE_LINE" ]] && return

  local result
  result="$(llmc "$READLINE_LINE" 2>/dev/tty)"

  if [[ $? -eq 0 && -n "$result" ]]; then
    READLINE_LINE="$result"
    READLINE_POINT=${#READLINE_LINE}
  fi
}

bind -x '"\C-e": _ai_cmd_replace'
"#;

const SETUP_ZSH: &str = r#"# llmc: Zsh integration — source this file in your .zshrc
# Usage: Press Ctrl+E with a natural language description on the command line

_ai_cmd_replace() {
  [[ -z "$BUFFER" ]] && return

  local result
  result="$(llmc "$BUFFER" 2>/dev/tty)"

  if [[ $? -eq 0 && -n "$result" ]]; then
    BUFFER="$result"
    CURSOR=${#BUFFER}
  fi
  zle redisplay
}

zle -N _ai_cmd_replace
bindkey '^e' _ai_cmd_replace
"#;

// ── install / uninstall ────────────────────────────────────────────────────────
fn cmd_install() {
    let home = env::var("HOME").unwrap_or_else(|_| {
        eprintln!("llmc: HOME not set");
        process::exit(1);
    });

    let install_dir = PathBuf::from(&home).join(".local/bin");
    let data_dir = PathBuf::from(&home).join(".local/share/llmc");

    // 1. Copy self to ~/.local/bin/llmc
    let self_path = env::current_exe().unwrap_or_else(|e| {
        eprintln!("llmc: cannot determine own path: {e}");
        process::exit(1);
    });
    let _ = fs::create_dir_all(&install_dir);
    let dest = install_dir.join("llmc");
    if self_path != dest {
        fs::copy(&self_path, &dest).unwrap_or_else(|e| {
            eprintln!("llmc: failed to copy binary: {e}");
            process::exit(1);
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o755));
        }
        eprintln!("Installed: {}", dest.display());
    } else {
        eprintln!("Binary already at {}", dest.display());
    }

    // 2. Write shell integration scripts
    let _ = fs::create_dir_all(&data_dir);
    let bash_path = data_dir.join("setup_bash.sh");
    let zsh_path = data_dir.join("setup_zsh.sh");
    let _ = fs::write(&bash_path, SETUP_BASH);
    let _ = fs::write(&zsh_path, SETUP_ZSH);
    eprintln!("Installed: {}/", data_dir.display());

    // 3. Detect shell and rc file
    let shell_name = env::var("SHELL").unwrap_or_default();
    let (rc_file, setup_file) = if shell_name.ends_with("zsh") {
        (PathBuf::from(&home).join(".zshrc"), &zsh_path)
    } else if shell_name.ends_with("bash") {
        (PathBuf::from(&home).join(".bashrc"), &bash_path)
    } else {
        eprintln!("Done! Shell integration is available for bash and zsh only.");
        return;
    };

    // 4. Ensure PATH
    let rc_content = fs::read_to_string(&rc_file).unwrap_or_default();
    let install_dir_str = install_dir.display().to_string();
    if !rc_content.contains(&install_dir_str) {
        let line = format!("\nexport PATH=\"{}:$PATH\"\n", install_dir_str);
        let _ = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&rc_file)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        eprintln!("Added {} to PATH in {}", install_dir_str, rc_file.display());
    }

    // 5. Add shell integration source line
    let setup_str = setup_file.display().to_string();
    let rc_content = fs::read_to_string(&rc_file).unwrap_or_default();
    if !rc_content.contains(&setup_str) {
        let line = format!("\nsource \"{}\"\n", setup_str);
        let _ = fs::OpenOptions::new()
            .append(true)
            .open(&rc_file)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        eprintln!("Added Ctrl+E integration to {}", rc_file.display());
    }

    eprintln!();
    eprintln!("Done! Run this to activate now:");
    eprintln!("  source {}", rc_file.display());
}

fn cmd_uninstall() {
    let home = env::var("HOME").unwrap_or_else(|_| {
        eprintln!("llmc: HOME not set");
        process::exit(1);
    });

    let bin = PathBuf::from(&home).join(".local/bin/llmc");
    let data_dir = PathBuf::from(&home).join(".local/share/llmc");
    let config_dir = PathBuf::from(&home).join(".config/llmc");

    eprintln!("Uninstalling llmc...");

    // Remove shell integration from rc files
    for name in &[".zshrc", ".bashrc", ".profile"] {
        let rc = PathBuf::from(&home).join(name);
        if let Ok(content) = fs::read_to_string(&rc) {
            if content.contains("llmc") {
                let filtered: String = content
                    .lines()
                    .filter(|l| !l.contains("setup_zsh.sh") && !l.contains("setup_bash.sh"))
                    .map(|l| format!("{l}\n"))
                    .collect();
                let _ = fs::write(&rc, filtered);
                eprintln!("Cleaned: {}", rc.display());
            }
        }
    }

    // Remove data dir
    if data_dir.exists() {
        let _ = fs::remove_dir_all(&data_dir);
        eprintln!("Removed: {}", data_dir.display());
    }

    // Remove config
    if config_dir.exists() {
        let answer = prompt_stderr("Remove config (API key)? [y/N]: ");
        if answer.eq_ignore_ascii_case("y") {
            let _ = fs::remove_dir_all(&config_dir);
            eprintln!("Removed: {}", config_dir.display());
        } else {
            eprintln!("Kept: {}", config_dir.display());
        }
    }

    // Remove binary last (we are running from it, but the OS keeps the fd open)
    if bin.exists() {
        let _ = fs::remove_file(&bin);
        eprintln!("Removed: {}", bin.display());
    }

    eprintln!();
    eprintln!("Done! Restart your shell to apply changes.");
}

fn print_help() {
    eprintln!("llmc {} — natural language to shell command", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("Usage: llmc <query>        convert natural language to a shell command");
    eprintln!("       llmc --setup        reconfigure API provider/model/key");
    eprintln!("       llmc --install      install binary & Ctrl+E shell integration");
    eprintln!("       llmc --uninstall    remove everything");
    eprintln!("       llmc --version      show version");
    eprintln!("       llmc --help         show this help");
}

// ── main ───────────────────────────────────────────────────────────────────────
fn main() {
    let deadline = Instant::now() + HARD_TIMEOUT;

    // Gather user query from args
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_help();
        process::exit(1);
    }

    if args.len() == 1 {
        match args[0].as_str() {
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--version" | "-V" => {
                println!("llmc {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--setup" => {
                interactive_setup();
                eprintln!("Setup complete.");
                return;
            }
            "--install" => {
                cmd_install();
                return;
            }
            "--uninstall" => {
                cmd_uninstall();
                return;
            }
            _ => {}
        }
    }

    let user_query = args.join(" ");

    // Config: env vars → config file → interactive setup
    let api_key = resolve_api_key();
    let api_base = resolve_config_field("LLM_API_BASE", "api_base", "https://api.openai.com/v1");
    let model = resolve_config_field("LLM_MODEL", "model", "gpt-4o-mini");

    let backend = detect_backend(&api_base);

    // Build ureq agent with timeouts
    let remaining = deadline.saturating_duration_since(Instant::now());
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(remaining)
        .timeout_write(Duration::from_secs(5))
        .build();

    let system = system_prompt();

    // Build initial messages (backend-specific)
    let mut messages: Vec<Value> = match backend {
        ApiBackend::OpenAI => vec![
            json!({ "role": "system", "content": system }),
            json!({ "role": "user",   "content": user_query }),
        ],
        ApiBackend::Anthropic => vec![
            json!({ "role": "user", "content": user_query }),
        ],
    };

    let tools = match backend {
        ApiBackend::OpenAI => tool_schema_openai(),
        ApiBackend::Anthropic => tool_schema_anthropic(),
    };

    // ── agent loop ─────────────────────────────────────────────────────────────
    for _round in 0..MAX_TOOL_ROUNDS {
        if Instant::now() >= deadline {
            eprintln!("llmc: 15s timeout exceeded");
            process::exit(1);
        }

        let spinner = Spinner::start("Thinking...");
        let result = match backend {
            ApiBackend::OpenAI => {
                call_openai(&agent, &api_base, &model, &api_key, &messages, &tools)
            }
            ApiBackend::Anthropic => {
                call_anthropic(&agent, &api_base, &model, &api_key, &system, &messages, &tools)
            }
        };
        spinner.stop();

        match result {
            ApiResult::Text(text) => {
                println!("{text}");
                return;
            }
            ApiResult::ToolCalls(calls) => {
                // Push assistant message with tool calls
                match backend {
                    ApiBackend::OpenAI => openai_push_assistant_tool_calls(&mut messages, &calls),
                    ApiBackend::Anthropic => {
                        anthropic_push_assistant_tool_calls(&mut messages, &calls)
                    }
                }

                // Execute each tool and collect results
                let mut tool_results: Vec<(String, String)> = Vec::new();
                for tc in &calls {
                    let result = if tc.name == "run_readonly_command" {
                        match serde_json::from_value::<RunCmdArgs>(tc.args.clone()) {
                            Ok(parsed) => {
                                let cmd_args = parsed.args.unwrap_or_default();
                                let label =
                                    format!("Running: {} {}", parsed.command, cmd_args.join(" "));
                                let sp = Spinner::start(&label);
                                let out = exec_sandboxed(&parsed.command, &cmd_args, deadline);
                                sp.stop();
                                out
                            }
                            Err(e) => format!("Error parsing arguments: {e}"),
                        }
                    } else {
                        format!("Unknown tool: {}", tc.name)
                    };

                    tool_results.push((tc.id.clone(), result));
                }

                // Push tool results into message history
                match backend {
                    ApiBackend::OpenAI => {
                        for (id, result) in &tool_results {
                            openai_push_tool_result(&mut messages, id, result);
                        }
                    }
                    ApiBackend::Anthropic => {
                        anthropic_push_tool_results(&mut messages, &tool_results);
                    }
                }

                continue;
            }
            ApiResult::Empty => {
                eprintln!("llmc: model returned empty response");
                process::exit(1);
            }
        }
    }

    eprintln!("llmc: max tool rounds ({MAX_TOOL_ROUNDS}) exceeded");
    process::exit(1);
}
