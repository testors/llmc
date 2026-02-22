# llmc

A lightweight agentic CLI tool that converts natural language into shell commands.

Type what you want to do in plain language on your prompt, press `Ctrl+E`, and the LLM inspects your local environment to produce the exact shell command.

## How It Works

```
User input (natural language)
  |
  v
llmc ----> LLM API (OpenAI-compatible)
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

### Prerequisites

- [Rust toolchain](https://rustup.rs/) (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)

### Quick Install

```bash
git clone https://github.com/your-username/llmc.git
cd llmc
./install.sh
```

The install script will:
1. Build the release binary
2. Install it to `~/.local/bin/` (no `sudo` required)
3. Add `~/.local/bin` to your `PATH` if needed
4. Register the **Ctrl+E** shell integration for your shell (Bash/Zsh)

## Configuration

### Interactive Setup (First Run)

If no API key is configured, you will be prompted interactively:

```
$ llmc "check disk usage"
llmc: API key is not set.

API Key: sk-...
API Base URL (Enter=https://api.openai.com/v1):
Model (Enter=gpt-4o-mini):
llmc: config saved -> ~/.config/llmc/config.json
```

The config is saved to `~/.config/llmc/config.json` with `chmod 600` (owner-only access).

### Environment Variables (Override)

Environment variables take precedence over the config file:

```bash
export LLM_API_KEY="sk-..."                          # Required (if no config file)
export LLM_API_BASE="https://api.openai.com/v1"      # Optional, default: OpenAI
export LLM_MODEL="gpt-4o-mini"                        # Optional, default: gpt-4o-mini
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

## Shell Integration

Shell integration is automatically configured by `./install.sh`. To set it up manually:

```bash
# Zsh
echo 'source /path/to/llmc/setup_zsh.sh' >> ~/.zshrc

# Bash
echo 'source /path/to/llmc/setup_bash.sh' >> ~/.bashrc
```

After setup, type a natural language description on the prompt and press **Ctrl+E** to replace it with the generated command. Pressing Ctrl+E on an empty line does nothing.

## Usage

```bash
# Direct invocation
$ llmc "find the 10 largest files in the current directory"
du -ah . | sort -rh | head -10

# Via Ctrl+E
$ find py files modified in the last 3 days   # <- press Ctrl+E here
$ find . -name "*.py" -mtime -3               # <- auto-replaced
```

## Security

### Sandbox

Commands the LLM can execute are strictly limited to a hardcoded whitelist:

```
ls, grep, cat, find, head, tail, tree, file, stat, which, wc, du
```

- Any command outside the whitelist returns `Permission Denied`
- Binaries are executed directly via `std::process::Command` — no `sh -c` wrapper, preventing shell injection
- Command output exceeding 2,000 bytes is automatically truncated

### Timeout

The entire execution (API calls + tool execution) is subject to a **15-second** hard timeout.

### Config File

`~/.config/llmc/config.json` is protected with `chmod 600` (owner read/write only).

## Project Structure

```
llmc/
├── Cargo.toml         # Dependencies and release optimizations
├── src/main.rs        # Agent loop, sandbox, config management
├── install.sh         # Build + install script (no sudo)
├── setup_bash.sh      # Bash Ctrl+E integration
├── setup_zsh.sh       # Zsh Ctrl+E integration
└── README.md
```

## Release Build Optimization

The following settings in `Cargo.toml` minimize binary size:

```toml
[profile.release]
opt-level = "z"    # Optimize for size
lto = true         # Link-time optimization
codegen-units = 1  # Single codegen unit
panic = "abort"    # Abort on panic
strip = true       # Strip debug symbols
```

Resulting binary size: ~1.3MB (arm64 macOS)

## License

MIT
