# llmc

A lightweight agentic CLI tool that converts natural language into shell commands.

Type what you want to do in plain language on your prompt, press `Ctrl+E`, and the LLM inspects your local environment to produce the exact shell command.

Supports **OpenAI**, **Anthropic** (native), and **Google Gemini** out of the box, plus any OpenAI-compatible API.

## Usage

```bash
# Ctrl+E (shell integration) — type naturally, press Ctrl+E
$ find py files modified in the last 3 days   # <- press Ctrl+E here
$ find . -name "*.py" -mtime -3               # <- auto-replaced

# Direct invocation
$ llmc "find the 10 largest files in the current directory"
du -ah . | sort -rh | head -10
```

### CLI Options

```
llmc <query>        convert natural language to a shell command
llmc --setup        configure or reconfigure API provider/model/key
llmc --config       show current configuration
llmc --version      show version
llmc --help         show help
```

## How It Works

```
User input (natural language)
  |
  v
llmc ----> LLM API (OpenAI / Anthropic / Gemini)
  |              |
  |              v
  |        run_readonly_command (Tool Call)
  |              |
  |              v
  |        Sandboxed execution (whitelisted commands only)
  |              |
  |              v
  |        Return result to LLM (multi-turn)
  |
  v
Final shell command -> replaces READLINE_LINE / BUFFER
```

- When the LLM needs to inspect local files or system state, it calls the `run_readonly_command` tool to execute read-only commands.
- Up to 10 tool-call rounds are supported, with a hard 15-second timeout on the entire execution.

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/testors/llmc/main/install.sh | sh
```

That's it. Restart your shell (or `source ~/.zshrc`) and you're ready to go.

Supports **macOS** and **Linux** on both **x86_64** and **arm64**.

### Build from Source

```bash
git clone https://github.com/testors/llmc.git
cd llmc
cargo build --release
cp target/release/llmc ~/.local/bin/
```

Requires [Rust toolchain](https://rustup.rs/). Run the curl installer afterwards for shell integration.

### Uninstall

```bash
rm ~/.local/bin/llmc
rm -rf ~/.local/share/llmc ~/.config/llmc
```

Remove the `source` and `export PATH` lines from your `~/.zshrc` or `~/.bashrc`.

## Configuration

### Setup

Run `llmc --setup` to configure your API provider, model, and key:

```
$ llmc --setup
llmc: initial setup

Select API provider:
  1) ChatGPT (OpenAI)
  2) Claude (Anthropic)
  3) Gemini (Google)
  4) Other (manual input)

Choice [1-4]: 2

Select model:
  1) claude-haiku-4-5-20251001 (recommended)
  2) claude-sonnet-4-5-20250929 (balanced)
  3) claude-opus-4-5-20251101 (high performance)
  4) Enter manually

Choice [1-4]: 1

API Key: sk-ant-...

llmc: config saved -> ~/.config/llmc/config.json
```

The model list is fetched from the latest [models.json](models.json) at setup time, with a built-in fallback if the fetch fails.

Config is saved to `~/.config/llmc/config.json` with `chmod 600` (owner-only access). Run `llmc --setup` again at any time to reconfigure.

### Environment Variables (Override)

Environment variables take precedence over the config file:

```bash
export LLM_API_KEY="sk-..."
export LLM_API_BASE="https://api.openai.com/v1"
export LLM_MODEL="gpt-5-mini"
```

### Resolution Order

1. Environment variables (`LLM_API_KEY`, `LLM_API_BASE`, `LLM_MODEL`)
2. Config file (`~/.config/llmc/config.json`)
3. Interactive prompt (first run only, persisted to config file)

### OpenAI-Compatible APIs

Set `LLM_API_BASE` to use any OpenAI-compatible server:

```bash
# Ollama
export LLM_API_BASE="http://localhost:11434/v1"
export LLM_MODEL="llama3"

# LiteLLM / vLLM
export LLM_API_BASE="http://localhost:4000/v1"
```

## Supported Providers

| Provider | API Base | Auth | Models |
|----------|----------|------|--------|
| OpenAI | `https://api.openai.com/v1` | Bearer token | gpt-5-mini, gpt-5.2, gpt-4.1-mini |
| Anthropic | `https://api.anthropic.com` | `x-api-key` header | claude-haiku-4-5, claude-sonnet-4-5, claude-opus-4-5 |
| Gemini | `https://generativelanguage.googleapis.com/v1beta/openai` | Bearer token | gemini-2.5-flash-lite, gemini-2.5-flash, gemini-2.5-pro |

Anthropic uses native Messages API (`/v1/messages`). OpenAI and Gemini use Chat Completions API (`/chat/completions`).

## Security

### Sandbox

Commands the LLM can execute are strictly limited to a hardcoded whitelist:

```
ls, grep, cat, find, head, tail, tree, file, stat, which, wc, du
```

- Any command outside the whitelist returns `Permission Denied`
- Binaries are executed directly via `std::process::Command` — no `sh -c` wrapper, preventing shell injection
- Command output exceeding 10,000 bytes is automatically truncated

### Timeout

The entire execution (API calls + tool execution) is subject to a **15-second** hard timeout.

### Config File

`~/.config/llmc/config.json` is protected with `chmod 600` (owner read/write only).

## License

MIT
