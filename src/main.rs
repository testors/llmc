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
const HARD_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 10_000;
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

#[derive(PartialEq)]
enum Mode {
    Command,
    Chat { to_stderr: bool },
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .and_then(|mut f| f.write_all(s.as_bytes()));
        }
        #[cfg(not(unix))]
        {
            let _ = fs::write(&path, s);
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
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    eprintln!("\nllmc: failed to read input");
                    process::exit(1);
                }
                Ok(_) => line.trim().to_string(),
            }
        }
        Err(_) => {
            eprint!("{msg}");
            let mut line = String::new();
            match io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => {
                    eprintln!("\nllmc: failed to read input");
                    process::exit(1);
                }
                Ok(_) => line.trim().to_string(),
            }
        }
    }
}

fn is_interactive() -> bool {
    // Check if stdin is a TTY, or if /dev/tty is accessible for reading
    // When invoked from a shell widget (Ctrl+E), stdin is not a TTY
    // but /dev/tty may still be locked by zle, making interactive input unreliable
    unsafe { libc_isatty(0) != 0 }
}

extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
}

fn resolve_api_key(config: &Value) -> String {
    // 1. Environment variable
    if let Ok(key) = env::var("LLM_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }

    // 2. Config file
    if let Some(key) = config["api_key"].as_str() {
        if !key.is_empty() {
            return key.to_string();
        }
    }

    // 3. Interactive setup (only if running interactively)
    if !is_interactive() {
        eprintln!("llmc: not configured. Run `llmc --setup` first.");
        process::exit(1);
    }
    interactive_setup()
}

// ── remote model list ──────────────────────────────────────────────────────────
const MODELS_URL: &str =
    "https://raw.githubusercontent.com/testors/llmc/main/models.json";

fn fetch_provider_config(provider_key: &str) -> Option<(String, Vec<(String, String)>)> {
    let resp = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout_read(Duration::from_secs(3))
        .build()
        .get(MODELS_URL)
        .call()
        .ok()?;
    let text = resp.into_string().ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    let provider = json.get(provider_key)?;
    let api_base = provider["api_base"].as_str()?.to_string();
    let models = provider["models"]
        .as_array()?
        .iter()
        .filter_map(|m| {
            Some((
                m["id"].as_str()?.to_string(),
                m["desc"].as_str()?.to_string(),
            ))
        })
        .collect();
    Some((api_base, models))
}

fn fallback_provider(key: &str) -> (String, Vec<(String, String)>) {
    match key {
        "openai" => (
            "https://api.openai.com/v1".into(),
            vec![
                ("gpt-5-mini".into(), "recommended".into()),
                ("gpt-5.2".into(), "high performance".into()),
                ("gpt-4.1-mini".into(), "legacy, cheap".into()),
            ],
        ),
        "anthropic" => (
            "https://api.anthropic.com".into(),
            vec![
                ("claude-haiku-4-5-20251001".into(), "recommended".into()),
                ("claude-sonnet-4-5-20250929".into(), "balanced".into()),
                ("claude-opus-4-5-20251101".into(), "high performance".into()),
            ],
        ),
        "gemini" => (
            "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            vec![
                ("gemini-2.5-flash-lite".into(), "recommended".into()),
                ("gemini-2.5-flash".into(), "balanced".into()),
                ("gemini-2.5-pro".into(), "high performance".into()),
            ],
        ),
        _ => unreachable!(),
    }
}

fn get_provider(key: &str) -> (String, Vec<(String, String)>) {
    fetch_provider_config(key).unwrap_or_else(|| fallback_provider(key))
}

// ── interactive setup ──────────────────────────────────────────────────────────
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
        "1" => {
            let (base, models) = get_provider("openai");
            setup_preset(&base, &models)
        }
        "2" => {
            let (base, models) = get_provider("anthropic");
            setup_preset(&base, &models)
        }
        "3" => {
            let (base, models) = get_provider("gemini");
            setup_preset(&base, &models)
        }
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

fn setup_preset(api_base: &str, models: &[(String, String)]) -> (String, String, String) {
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
        models[model_idx - 1].0.clone()
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

fn resolve_config_field(config: &Value, env_var: &str, config_key: &str, default: &str) -> String {
    env::var(env_var)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            config[config_key]
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
         4. If you cannot produce a valid command, respond with EXACTLY: NOCOMMAND: <brief reason>\n\
            Example: NOCOMMAND: not a shell task"
    )
}

fn chat_system_prompt() -> String {
    "You are a helpful assistant. Be concise. Answer in plain text without markdown formatting.".to_string()
}

// ── model upgrade for ask mode ──────────────────────────────────────────────────
fn upgrade_model_for_ask(config_model: &str) -> String {
    // Map recommended models to their high-performance counterpart
    let providers: &[(&[&str], &str)] = &[
        (
            &["claude-haiku-4-5-20251001"],
            "claude-opus-4-5-20251101",
        ),
        (&["gpt-5-mini"], "gpt-5.2"),
        (&["gemini-2.5-flash-lite"], "gemini-2.5-pro"),
    ];

    for (recommended, high_perf) in providers {
        if recommended.contains(&config_model) {
            return high_perf.to_string();
        }
    }

    // User manually chose a model — respect it
    config_model.to_string()
}

// ── sandbox executor ───────────────────────────────────────────────────────────
const DANGEROUS_FIND_FLAGS: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fls", "-fprintf",
];

fn exec_sandboxed(cmd: &str, args: &[String], deadline: Instant) -> String {
    if !ALLOWED_COMMANDS.contains(&cmd) {
        return format!("Permission Denied: '{cmd}' is not in the allowed command list.");
    }

    // Block dangerous find flags that allow arbitrary execution or file modification
    if cmd == "find" {
        for arg in args {
            if DANGEROUS_FIND_FLAGS.iter().any(|f| arg.eq_ignore_ascii_case(f)) {
                return format!("Permission Denied: '{arg}' is not allowed with find.");
            }
        }
    }

    if Instant::now() >= deadline {
        return "Error: timeout reached".into();
    }

    let mut child = match Command::new(cmd)
        .args(args)
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .env_remove("LLM_API_KEY")
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("Error: {e}"),
    };

    // Read stdout/stderr in threads to avoid pipe buffer deadlock
    let stdout = child.stdout.take();
    let max_bytes = MAX_OUTPUT_BYTES as u64 + 1;
    let stdout_thread = thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(out) = stdout {
            let _ = out.take(max_bytes).read_to_end(&mut buf);
        }
        buf
    });

    let stderr = child.stderr.take();
    let stderr_thread = thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(err) = stderr {
            let _ = err.take(max_bytes).read_to_end(&mut buf);
        }
        buf
    });

    // Wait with timeout enforcement via polling
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return "Error: timeout reached".into();
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return format!("Error: {e}");
            }
        }
    };

    let stdout_buf = stdout_thread.join().unwrap_or_default();
    let stderr_buf = stderr_thread.join().unwrap_or_default();

    let mut output = if stdout_buf.len() > MAX_OUTPUT_BYTES {
        let mut s = String::from_utf8_lossy(&stdout_buf[..MAX_OUTPUT_BYTES]).into_owned();
        s.push_str("...(truncated)");
        s
    } else {
        String::from_utf8_lossy(&stdout_buf).into_owned()
    };

    if !status.success() {
        let stderr_str = String::from_utf8_lossy(&stderr_buf);
        output.push_str(&format!(
            "\n[exit {}] {}",
            status.code().unwrap_or(-1),
            stderr_str
        ));
    }

    output
}

// ── API error handling ─────────────────────────────────────────────────────────
fn handle_api_error(err: ureq::Error) -> ! {
    match err {
        ureq::Error::Status(status, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let hint = match status {
                401 => "Invalid API key. Run `llmc --setup` to reconfigure.",
                403 => "Access denied. Check your API key permissions.",
                404 => "Model not found. Run `llmc --setup` to change model.",
                429 => "Rate limited. Please try again later.",
                500..=599 => "Server error. Please try again later.",
                _ => "",
            };
            eprintln!("llmc: API error {status}: {hint}");
            // Try to extract error message from JSON response
            if let Ok(json) = serde_json::from_str::<Value>(&body) {
                if let Some(msg) = json["error"]["message"].as_str() {
                    eprintln!("llmc: {msg}");
                }
            }
            process::exit(1);
        }
        ureq::Error::Transport(t) => {
            eprintln!("llmc: connection failed: {t}");
            process::exit(1);
        }
    }
}

// ── show config ────────────────────────────────────────────────────────────────
fn cmd_config() {
    let config = load_config();
    let path = config_path();

    let api_base = config["api_base"].as_str().unwrap_or("(not set)");
    let model = config["model"].as_str().unwrap_or("(not set)");
    let api_key = config["api_key"]
        .as_str()
        .map(|k| {
            if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            }
        })
        .unwrap_or_else(|| "(not set)".to_string());

    let backend = config["api_base"]
        .as_str()
        .map(|b| {
            if b.contains("anthropic.com") {
                "Anthropic"
            } else if b.contains("googleapis.com") {
                "Gemini"
            } else if b.contains("openai.com") {
                "OpenAI"
            } else {
                "Custom"
            }
        })
        .unwrap_or("(unknown)");

    eprintln!("Config: {}", path.display());
    eprintln!();
    eprintln!("  Provider:  {backend}");
    eprintln!("  API Base:  {api_base}");
    eprintln!("  Model:     {model}");
    eprintln!("  API Key:   {api_key}");
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
        Err(e) => handle_api_error(e),
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
    max_tokens: u32,
) -> ApiResult {
    let body = json!({
        "model": model,
        "system": system,
        "messages": messages,
        "tools": tools,
        "max_tokens": max_tokens,
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
        Err(e) => handle_api_error(e),
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

fn print_help() {
    eprintln!("llmc {} — natural language to shell command", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("Usage: llmc <query>        convert natural language to a shell command");
    eprintln!("       llmc --ask <query>  ask a question and get an answer");
    eprintln!("       llmc --setup        reconfigure API provider/model/key");
    eprintln!("       llmc --config       show current configuration");
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
            "--config" => {
                cmd_config();
                return;
            }
            _ => {}
        }
    }

    // Detect mode: --ask flag or ? prefix
    let (user_query, mode) = if args[0] == "--ask" {
        let query = args[1..].join(" ");
        if query.is_empty() {
            eprintln!("llmc: --ask requires a question");
            process::exit(1);
        }
        (query, Mode::Chat { to_stderr: false })
    } else {
        let joined = args.join(" ");
        if joined.starts_with('?') {
            let query = joined["?".len()..].trim().to_string();
            if query.is_empty() {
                eprintln!("llmc: empty question");
                process::exit(1);
            }
            (query, Mode::Chat { to_stderr: true })
        } else {
            (joined, Mode::Command)
        }
    };

    // Config: env vars → config file → interactive setup (load once)
    let config = load_config();
    let api_key = resolve_api_key(&config);
    let api_base = resolve_config_field(&config, "LLM_API_BASE", "api_base", "https://api.openai.com/v1");
    let backend = detect_backend(&api_base);
    let model_default = match backend {
        ApiBackend::Anthropic => "claude-haiku-4-5-20251001",
        ApiBackend::OpenAI => "gpt-5-mini",
    };
    let config_model = resolve_config_field(&config, "LLM_MODEL", "model", model_default);

    // Select system prompt and model based on mode
    let (system, model) = match &mode {
        Mode::Command => (system_prompt(), config_model),
        Mode::Chat { .. } => (chat_system_prompt(), upgrade_model_for_ask(&config_model)),
    };

    let max_tokens: u32 = match &mode {
        Mode::Command => 512,
        Mode::Chat { .. } => 4096,
    };

    // Build ureq agent with timeouts
    let remaining = deadline.saturating_duration_since(Instant::now());
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(remaining)
        .timeout_write(Duration::from_secs(5))
        .build();

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

    let tools = match &mode {
        Mode::Command => match backend {
            ApiBackend::OpenAI => tool_schema_openai(),
            ApiBackend::Anthropic => tool_schema_anthropic(),
        },
        Mode::Chat { .. } => json!([]),
    };

    // ── agent loop ─────────────────────────────────────────────────────────────
    for _round in 0..MAX_TOOL_ROUNDS {
        if Instant::now() >= deadline {
            eprintln!("llmc: {}s timeout exceeded", HARD_TIMEOUT.as_secs());
            process::exit(1);
        }

        let spinner = Spinner::start("Thinking...");
        let result = match backend {
            ApiBackend::OpenAI => {
                call_openai(&agent, &api_base, &model, &api_key, &messages, &tools)
            }
            ApiBackend::Anthropic => {
                call_anthropic(&agent, &api_base, &model, &api_key, &system, &messages, &tools, max_tokens)
            }
        };
        spinner.stop();

        match result {
            ApiResult::Text(text) => {
                match &mode {
                    Mode::Command => {
                        if let Some(rest) = text.strip_prefix("NOCOMMAND:") {
                            let reason = rest.lines().next().unwrap_or("").trim();
                            if reason.is_empty() {
                                eprintln!("llmc: could not generate a command");
                            } else {
                                eprintln!("llmc: {reason}");
                            }
                            process::exit(1);
                        }
                        // Heuristic: a valid command is typically 1-3 lines.
                        // Multi-line prose without shell metacharacters is likely an explanation.
                        let line_count = text.lines().count();
                        if line_count > 3
                            && !text.contains('|')
                            && !text.contains('&')
                            && !text.contains(';')
                            && !text.ends_with('\\')
                        {
                            eprintln!("llmc: could not generate a command");
                            process::exit(1);
                        }
                        println!("{text}");
                        return;
                    }
                    Mode::Chat { to_stderr: true } => {
                        eprintln!("\n{text}");
                        return; // exit 0 — widget clears BUFFER
                    }
                    Mode::Chat { to_stderr: false } => {
                        println!("{text}");
                        return;
                    }
                }
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
