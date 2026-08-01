use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use similar::TextDiff;

const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";
const DEFAULT_MODEL: &str = "llama3.1";

#[derive(Parser, Debug)]
#[command(
    name = "clive",
    version,
    about = "Clive: a coding assistant CLI backed by Ollama",
    long_about = "Clive is a local-first coding assistant for terminal workflows. Chat with Ollama models, run multi-turn sessions, manage models, and safely edit code.",
    arg_required_else_help = true,
    subcommand_required = true,
    propagate_version = true,
    disable_help_subcommand = true,
    infer_subcommands = true,
    next_line_help = true,
    after_help = "Examples:\n  clive chat \"Explain Rust ownership\" --model qwen2.5-coder:latest\n  clive session --model qwen2.5-coder:latest\n  clive agent \"Refactor error handling\" --files src/main.rs --apply --verify \"cargo check -q\"\n  clive ollama pull qwen2.5-coder:latest\n  clive edit src/main.rs \"Add structured logging\" --write --backup"
)]
struct Cli {
    /// Override Ollama base URL (default: http://127.0.0.1:11434)
    #[arg(long, global = true)]
    ollama_url: Option<String>,

    /// Default model for chat/session/edit commands
    #[arg(long, short = 'm', global = true, env = "CLIVE_MODEL")]
    model: Option<String>,

    /// Hide the Clive ASCII art banner
    #[arg(long, global = true)]
    no_banner: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Send a prompt to an Ollama model
    /// Example: clive chat "Explain Rust ownership" --model qwen2.5-coder:latest
    #[command(visible_alias = "ask")]
    Chat(ChatArgs),

    /// List local Ollama models
    /// Example: clive models
    #[command(visible_alias = "ls")]
    Models,

    /// Check Ollama connectivity
    /// Example: clive doctor
    #[command(visible_alias = "health")]
    Doctor,

    /// Start an interactive multi-turn coding session
    /// Example: clive session --model qwen2.5-coder:latest
    #[command(visible_alias = "repl")]
    Session(SessionArgs),

    /// Run an autonomous coding workflow across multiple files
    /// Example: clive agent "Refactor error handling" --files src/main.rs --apply --verify "cargo check -q"
    #[command(visible_alias = "run")]
    Agent(AgentArgs),

    /// Manage Ollama from Clive (serve, pull, rm)
    /// Example: clive ollama pull qwen2.5-coder:latest
    Ollama(OllamaArgs),

    /// Ask Clive to edit a file in-place (or preview changes)
    /// Example: clive edit src/main.rs "Add logging" --write
    Edit(EditArgs),

    /// Generate a unified diff and optionally apply it
    /// Example: clive patch src/main.rs "Extract helper functions" --write
    Patch(EditArgs),

    /// Generate shell completions
    /// Example: clive completions bash
    Completions(CompletionsArgs),

    /// View or edit Clive's persistent configuration
    /// Example: clive config set model qwen2.5-coder:latest
    Config(ConfigArgs),
}

#[derive(Args, Debug)]
struct ChatArgs {
    /// Prompt to send to the model (optional when piping via stdin)
    /// Example: clive chat "Explain Rust ownership"
    #[arg(default_value = "")]
    prompt: String,

    /// Model name (e.g. llama3.1, codellama, qwen2.5-coder)
    /// Example: --model qwen2.5-coder:latest
    #[arg(short, long)]
    model: Option<String>,

    /// Optional system message to steer behavior
    /// Example: --system "Be concise and practical"
    #[arg(short, long)]
    system: Option<String>,

    /// Read additional context from stdin and append it to the prompt
    /// Example: cat file.rs | clive chat "review this" --stdin
    #[arg(long, action = ArgAction::SetTrue)]
    stdin: bool,

    /// Disable token streaming and wait for full response
    /// Example: --no-stream
    #[arg(long, action = ArgAction::SetTrue)]
    no_stream: bool,
}

#[derive(Args, Debug)]
struct EditArgs {
    /// Path to file you want to modify
    /// Example: src/main.rs
    file: PathBuf,

    /// Edit instruction for the assistant
    /// Example: "Add structured logging"
    instruction: String,

    /// Model name to use for editing
    /// Example: --model qwen2.5-coder:latest
    #[arg(short, long)]
    model: Option<String>,

    /// Write the generated changes to disk
    /// Example: --write
    #[arg(long)]
    write: bool,

    /// Create a .bak copy before writing
    /// Example: --write --backup
    #[arg(long)]
    backup: bool,

    /// Refuse to write if git sees uncommitted changes in target file
    /// Example: --write --require-clean-git
    #[arg(long)]
    require_clean_git: bool,

    /// Stage the file with git add after writing
    /// Example: --write --stage
    #[arg(long)]
    stage: bool,
}

#[derive(Args, Debug)]
struct SessionArgs {
    /// Model name (e.g. qwen2.5-coder, codellama)
    /// Example: --model qwen2.5-coder:latest
    #[arg(short, long)]
    model: Option<String>,

    /// Optional system instruction
    /// Example: --system "Be concise and production-focused"
    #[arg(short, long)]
    system: Option<String>,

    /// Disable token streaming and wait for full response
    /// Example: --no-stream
    #[arg(long, action = ArgAction::SetTrue)]
    no_stream: bool,
}

#[derive(Args, Debug)]
struct AgentArgs {
    /// Goal to accomplish
    /// Example: "Refactor error handling and add tests"
    goal: String,

    /// Files Clive is allowed to edit
    /// Example: --files src/main.rs README.md
    #[arg(long, short = 'f', required = true)]
    files: Vec<PathBuf>,

    /// Verification commands to run after each apply (repeat flag)
    /// Example: --verify "cargo check -q"
    #[arg(long, short = 'v')]
    verify: Vec<String>,

    /// Model name override for this run
    /// Example: --model qwen2.5-coder:latest
    #[arg(short, long)]
    model: Option<String>,

    /// Apply edits to files. Without this, only previews are shown.
    /// Example: --apply
    #[arg(long)]
    apply: bool,

    /// Roll back all changed files if verification fails
    /// Example: --rollback-on-fail
    #[arg(long)]
    rollback_on_fail: bool,

    /// Require target files to be clean in git before applying edits
    /// Example: --require-clean-git
    #[arg(long)]
    require_clean_git: bool,

    /// Output machine-readable run summary JSON
    /// Example: --json
    #[arg(long)]
    json: bool,

    /// Allow agent plans to execute run_command actions
    /// Example: --allow-agent-commands
    #[arg(long)]
    allow_agent_commands: bool,

    /// Agent execution profile
    /// Example: --profile strict
    #[arg(long, value_enum, default_value_t = AgentProfile::Balanced)]
    profile: AgentProfile,

    /// Maximum agent refinement iterations
    /// Example: --max-iterations 3
    #[arg(long)]
    max_iterations: Option<usize>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AgentProfile {
    Quick,
    Balanced,
    Strict,
}

#[derive(Args, Debug)]
struct OllamaArgs {
    #[command(subcommand)]
    command: OllamaCommand,
}

#[derive(Subcommand, Debug)]
enum OllamaCommand {
    /// Start Ollama server from Clive
    Serve(ServeArgs),
    /// Pull/install a model
    Pull(PullArgs),
    /// Remove an installed model
    Rm(ModelNameArg),
    /// Curated coding-model recommendations
    Recommend(RecommendArgs),
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Start in the background and return immediately
    /// Example: --detach
    #[arg(long)]
    detach: bool,
}

#[derive(Args, Debug)]
struct PullArgs {
    /// Model name to install (e.g. qwen2.5-coder:latest)
    /// Example: qwen2.5-coder:latest
    model: String,
}

#[derive(Args, Debug)]
struct ModelNameArg {
    /// Model name
    /// Example: qwen2.5-coder:latest
    model: String,
}

#[derive(Args, Debug)]
struct RecommendArgs {
    /// Recommendation profile
    /// Example: --profile rust
    #[arg(long, value_enum, default_value_t = RecommendProfile::Coding)]
    profile: RecommendProfile,

    /// Only show models that are already installed
    /// Example: --installed-only
    #[arg(long)]
    installed_only: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RecommendProfile {
    Coding,
    Rust,
    Fast,
    Reasoning,
}

#[derive(Args, Debug)]
struct CompletionsArgs {
    /// Target shell (bash, zsh, fish, powershell, elvish)
    /// Example: bash
    #[arg(value_enum)]
    shell: Shell,
}

#[derive(Args, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Print the current configuration and its file path
    /// Example: clive config show
    Show,
    /// Set a configuration value (model, ollama_url, system)
    /// Example: clive config set model qwen2.5-coder:latest
    Set(ConfigSetArgs),
    /// Clear a configuration value (model, ollama_url, system)
    /// Example: clive config unset system
    Unset(ConfigKeyArg),
    /// Print the path to the configuration file
    /// Example: clive config path
    Path,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ConfigKey {
    Model,
    OllamaUrl,
    System,
}

#[derive(Args, Debug)]
struct ConfigSetArgs {
    /// Configuration key to set
    /// Example: model
    #[arg(value_enum)]
    key: ConfigKey,

    /// Value to store
    /// Example: qwen2.5-coder:latest
    value: String,
}

#[derive(Args, Debug)]
struct ConfigKeyArg {
    /// Configuration key to clear
    /// Example: system
    #[arg(value_enum)]
    key: ConfigKey,
}

/// Persistent user configuration stored as JSON on disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CliveConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ollama_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Message {
    role: String,
    #[serde(default)]
    content: String,
    /// Reasoning/thinking tokens emitted by thinking models (e.g. qwen3, r1).
    /// Only present on responses; never sent back in requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct AgentPlan {
    summary: String,
    #[serde(default)]
    edits: Vec<AgentEdit>,
    #[serde(default)]
    actions: Vec<AgentAction>,
    done: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AgentEdit {
    file: String,
    updated_content: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AgentAction {
    #[serde(rename = "edit_file")]
    EditFile {
        file: String,
        updated_content: String,
        reason: Option<String>,
    },
    #[serde(rename = "add_file")]
    AddFile {
        file: String,
        content: String,
        reason: Option<String>,
    },
    #[serde(rename = "run_command")]
    RunCommand {
        command: String,
        reason: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct AgentRunReport {
    goal: String,
    iterations: usize,
    changed_files: Vec<String>,
    verify_commands: Vec<String>,
    success: bool,
    rolled_back: bool,
}

#[derive(Debug, Deserialize)]
struct StreamChatResponse {
    message: Option<Message>,
    done: bool,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<ModelTag>,
}

#[derive(Debug, Deserialize)]
struct ModelTag {
    name: String,
    size: Option<u64>,
    modified_at: Option<String>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    let disable_banner_for_command = matches!(cli.command, Commands::Completions(_));
    if !cli.no_banner && !disable_banner_for_command {
        print_banner();
    }

    // Load persisted config; CLI flags and env vars still take precedence.
    let config = load_config().unwrap_or_default();

    // Config command does not need a live Ollama client.
    if let Commands::Config(args) = &cli.command {
        return cmd_config(args);
    }

    let base_url = cli
        .ollama_url
        .or_else(|| std::env::var("OLLAMA_HOST").ok())
        .or_else(|| config.ollama_url.clone())
        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());

    let ollama = OllamaClient::new(base_url)?;
    let default_model = cli.model.clone().or_else(|| config.model.clone());
    let default_system = config.system.clone();

    match cli.command {
        Commands::Chat(args) => cmd_chat(&ollama, args, &default_model, &default_system),
        Commands::Models => cmd_models(&ollama),
        Commands::Doctor => cmd_doctor(&ollama),
        Commands::Session(args) => cmd_session(&ollama, args, &default_model, &default_system),
        Commands::Agent(args) => cmd_agent(&ollama, args, &default_model),
        Commands::Ollama(args) => cmd_ollama(&ollama, args),
        Commands::Edit(args) => cmd_edit(&ollama, args, false, &default_model),
        Commands::Patch(args) => cmd_edit(&ollama, args, true, &default_model),
        Commands::Completions(args) => cmd_completions(args),
        Commands::Config(_) => unreachable!("handled before client creation"),
    }
}

fn print_banner() {
    println!(
        r"  ____ _ _            
 / ___| (_)_   _____   
| |   | | \ \ / / _ \  
| |___| | |\ V /  __/  
 \____|_|_| \_/ \___|  
"
    );
    println!("Clive - local coding assistant on Ollama\n");
}

fn resolve_model(ollama: &OllamaClient, local: Option<String>, global: &Option<String>) -> String {
    let installed_models = ollama.installed_model_names().unwrap_or_default();
    resolve_model_name(&installed_models, local, global)
}

/// Resolve the path to Clive's configuration file, honoring platform
/// conventions and the CLIVE_CONFIG override without extra dependencies.
fn config_file_path() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("CLIVE_CONFIG") {
        if !explicit.trim().is_empty() {
            return Ok(PathBuf::from(explicit));
        }
    }

    let base_dir = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("Could not determine config directory (APPDATA unset)"))?
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        xdg
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("Could not determine config directory (HOME unset)"))?;
        home.join(".config")
    };

    Ok(base_dir.join("clive").join("config.json"))
}

/// Load persisted configuration, returning defaults if the file is absent.
fn load_config() -> Result<CliveConfig> {
    let path = config_file_path()?;
    if !path.exists() {
        return Ok(CliveConfig::default());
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config at {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse config at {}", path.display()))
}

/// Persist configuration to disk, creating parent directories as needed.
fn save_config(config: &CliveConfig) -> Result<PathBuf> {
    let path = config_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory {}", parent.display()))?;
    }
    let serialized =
        serde_json::to_string_pretty(config).context("Failed to serialize configuration")?;
    fs::write(&path, format!("{serialized}\n"))
        .with_context(|| format!("Failed to write config to {}", path.display()))?;
    Ok(path)
}

fn cmd_config(args: &ConfigArgs) -> Result<()> {
    match &args.command {
        ConfigCommand::Show => {
            let path = config_file_path()?;
            let config = load_config()?;
            println!("Config file: {}", path.display());
            println!(
                "  model:      {}",
                config.model.as_deref().unwrap_or("(unset)")
            );
            println!(
                "  ollama_url: {}",
                config.ollama_url.as_deref().unwrap_or("(unset)")
            );
            println!(
                "  system:     {}",
                config.system.as_deref().unwrap_or("(unset)")
            );
        }
        ConfigCommand::Path => {
            println!("{}", config_file_path()?.display());
        }
        ConfigCommand::Set(set) => {
            let mut config = load_config()?;
            match set.key {
                ConfigKey::Model => config.model = Some(set.value.clone()),
                ConfigKey::OllamaUrl => config.ollama_url = Some(set.value.clone()),
                ConfigKey::System => config.system = Some(set.value.clone()),
            }
            let path = save_config(&config)?;
            println!("Updated {:?} = {}", set.key, set.value);
            println!("Saved to {}", path.display());
        }
        ConfigCommand::Unset(key) => {
            let mut config = load_config()?;
            match key.key {
                ConfigKey::Model => config.model = None,
                ConfigKey::OllamaUrl => config.ollama_url = None,
                ConfigKey::System => config.system = None,
            }
            let path = save_config(&config)?;
            println!("Cleared {:?}", key.key);
            println!("Saved to {}", path.display());
        }
    }
    Ok(())
}


fn resolve_model_name(
    installed_models: &[String],
    local: Option<String>,
    global: &Option<String>,
) -> String {
    if let Some(model) = local.or_else(|| global.clone()) {
        return model;
    }

    installed_models
        .first()
        .cloned()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn cmd_chat(
    ollama: &OllamaClient,
    args: ChatArgs,
    default_model: &Option<String>,
    default_system: &Option<String>,
) -> Result<()> {
    let model = resolve_model(ollama, args.model, default_model);
    let mut messages = Vec::new();
    let system = args.system.or_else(|| default_system.clone());
    if let Some(system) = system {
        messages.push(Message {
            role: "system".to_string(),
            content: system,
            ..Default::default()
        });
    }

    // Combine the positional prompt with piped stdin (if any) so users can do
    // `cat file.rs | clive chat "review this" --stdin` or pipe content with no
    // prompt at all. We only ever block on a stdin read when it is safe to do
    // so, otherwise a normal `clive chat "message"` would hang forever.
    let prompt = combine_prompt_with_stdin(&args.prompt, args.stdin)?;
    if prompt.trim().is_empty() {
        bail!("No prompt provided. Pass a message, or pipe input, e.g. `git diff | clive chat`.");
    }
    messages.push(Message {
        role: "user".to_string(),
        content: prompt,
        ..Default::default()
    });

    if args.no_stream {
        let spinner = Spinner::start("Thinking");
        let response = ollama.chat(&model, messages);
        spinner.stop();
        let response = response?;
        println!("{}", response.message.content.trim());
    } else {
        // No spinner here: streamed thinking/content tokens already provide
        // live feedback, and a spinner writing to stderr would interleave with
        // the thinking output that chat_stream emits.
        let full = ollama.chat_stream(&model, messages, |chunk| {
            print!("{chunk}");
            let _ = io::stdout().flush();
        })?;
        if !full.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

/// Merge the positional prompt with any content piped via stdin.
///
/// To avoid ever blocking the common `clive chat "message"` case, stdin is only
/// read when:
///   * the user explicitly passes `--stdin` (`force` is true), or
///   * no prompt was given AND stdin is not a terminal (a real pipe).
///
/// In every other case the original prompt is returned unchanged.
fn combine_prompt_with_stdin(prompt: &str, force: bool) -> Result<String> {
    use std::io::IsTerminal;

    let prompt_is_empty = prompt.trim().is_empty();
    let stdin_is_pipe = !io::stdin().is_terminal();

    // Only read stdin when it is safe: explicitly requested, or there is no
    // prompt and stdin is actually piped in.
    let should_read = force || (prompt_is_empty && stdin_is_pipe);
    if !should_read {
        return Ok(prompt.to_string());
    }

    // If the user forced --stdin but stdin is an interactive terminal, don't
    // hang waiting for EOF; just use the prompt as-is.
    if force && !stdin_is_pipe {
        return Ok(prompt.to_string());
    }

    let mut piped = String::new();
    io::stdin()
        .read_to_string(&mut piped)
        .context("Failed to read piped stdin")?;
    let piped = piped.trim_end();

    if piped.is_empty() {
        return Ok(prompt.to_string());
    }
    if prompt_is_empty {
        return Ok(piped.to_string());
    }
    Ok(format!("{prompt}\n\n{piped}"))
}

fn cmd_models(ollama: &OllamaClient) -> Result<()> {
    let models = ollama.tags()?;
    if models.models.is_empty() {
        println!("No local models found. Pull one with: clive ollama pull llama3.1");
        return Ok(());
    }

    for model in models.models {
        let size = model
            .size
            .map(human_size)
            .unwrap_or_else(|| "unknown".to_string());
        let modified = model.modified_at.unwrap_or_else(|| "unknown".to_string());
        println!("{}\t{}\t{}", model.name, size, modified);
    }
    Ok(())
}

fn cmd_doctor(ollama: &OllamaClient) -> Result<()> {
    println!("Clive v{}", env!("CARGO_PKG_VERSION"));

    match config_file_path() {
        Ok(path) => {
            let exists = if path.exists() { "found" } else { "not created yet" };
            println!("Config:  {} ({exists})", path.display());
        }
        Err(err) => println!("Config:  unavailable ({err})"),
    }

    match ollama.version() {
        Ok(version) => {
            println!("Ollama:  reachable at {}", ollama.base_url);
            println!("Version: {version}");
        }
        Err(err) => {
            println!("Ollama:  NOT reachable at {}", ollama.base_url);
            println!("         {err:#}");
            println!("Hint:    start it with `clive ollama serve --detach`");
            return Ok(());
        }
    }

    match ollama.tags() {
        Ok(tags) => {
            println!("Models:  {} installed locally", tags.models.len());
            if tags.models.is_empty() {
                println!("Hint:    pull one with `clive ollama pull qwen2.5-coder:latest`");
            }
        }
        Err(err) => println!("Models:  could not list ({err})"),
    }

    Ok(())
}

fn cmd_session(
    ollama: &OllamaClient,
    args: SessionArgs,
    default_model: &Option<String>,
    default_system: &Option<String>,
) -> Result<()> {
    let model = resolve_model(ollama, args.model, default_model);
    let mut messages = Vec::new();

    let system = args.system.or_else(|| default_system.clone());
    if let Some(system) = system {
        messages.push(Message {
            role: "system".to_string(),
            content: system,
            ..Default::default()
        });
    } else {
        messages.push(Message {
            role: "system".to_string(),
            content: "You are Clive, a practical coding assistant. Prefer concise, correct answers with runnable code examples when useful.".to_string(),
            ..Default::default()
        });
    }

    println!("Interactive session started with model: {model}");
    println!("Commands: /help, /clear, /save <file>, /load <file>, /exit");

    loop {
        print!("> ");
        io::stdout().flush().context("Failed to flush stdout")?;

        let mut input = String::new();
        let bytes = io::stdin()
            .read_line(&mut input)
            .context("Failed to read input")?;

        if bytes == 0 {
            println!("\nSession ended.");
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Handle slash-commands (some take an argument after a space).
        let (command, argument) = match input.split_once(char::is_whitespace) {
            Some((cmd, rest)) => (cmd, rest.trim()),
            None => (input, ""),
        };

        match command {
            "/exit" | "/quit" => {
                println!("Session ended.");
                break;
            }
            "/help" => {
                println!("/help          Show commands");
                println!("/clear         Clear conversation context");
                println!("/save <file>   Save conversation history to a JSON file");
                println!("/load <file>   Load conversation history from a JSON file");
                println!("/exit          Exit session");
                continue;
            }
            "/clear" => {
                messages.retain(|m| m.role == "system");
                println!("Conversation context cleared.");
                continue;
            }
            "/save" => {
                if argument.is_empty() {
                    eprintln!("Usage: /save <file>");
                    continue;
                }
                match save_session_history(argument, &messages) {
                    Ok(count) => println!("Saved {count} message(s) to {argument}"),
                    Err(err) => eprintln!("Failed to save session: {err:#}"),
                }
                continue;
            }
            "/load" => {
                if argument.is_empty() {
                    eprintln!("Usage: /load <file>");
                    continue;
                }
                match load_session_history(argument) {
                    Ok(loaded) => {
                        let count = loaded.len();
                        messages = loaded;
                        println!("Loaded {count} message(s) from {argument}");
                    }
                    Err(err) => eprintln!("Failed to load session: {err:#}"),
                }
                continue;
            }
            _ => {}
        }

        messages.push(Message {
            role: "user".to_string(),
            content: input.to_string(),
            ..Default::default()
        });

        let assistant_result = if args.no_stream {
            let spinner = Spinner::start("Thinking");
            let result = ollama.chat(&model, messages.clone());
            spinner.stop();
            result.map(|r| {
                let text = r.message.content.trim().to_string();
                println!("\n{text}\n");
                text
            })
        } else {
            println!();
            let result = ollama.chat_stream(&model, messages.clone(), |chunk| {
                print!("{chunk}");
                let _ = io::stdout().flush();
            });
            println!("\n");
            result.map(|s| s.trim().to_string())
        };

        match assistant_result {
            Ok(assistant) => {
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: assistant,
                    ..Default::default()
                });
            }
            Err(err) => {
                // Print the error but keep the session alive so the user
                // can retry without losing conversation context.
                eprintln!("\nError: {err:#}");
                eprintln!("(Session continues. Type /exit to quit.)\n");
                // Remove the failed user message so context stays consistent.
                messages.pop();
            }
        }
    }

    Ok(())
}

/// Serialize the conversation to a JSON file and return the message count.
fn save_session_history(path: &str, messages: &[Message]) -> Result<usize> {
    let serialized =
        serde_json::to_string_pretty(messages).context("Failed to serialize session history")?;
    fs::write(path, format!("{serialized}\n"))
        .with_context(|| format!("Failed to write session history to {path}"))?;
    Ok(messages.len())
}

/// Load a previously saved conversation from a JSON file.
fn load_session_history(path: &str) -> Result<Vec<Message>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read session history from {path}"))?;
    serde_json::from_str(&raw).context("Failed to parse session history JSON")
}

fn cmd_agent(ollama: &OllamaClient, args: AgentArgs, default_model: &Option<String>) -> Result<()> {
    if args.files.is_empty() {
        bail!("Provide at least one file with --files");
    }

    let model = resolve_model(ollama, args.model, default_model);
    let max_iterations = args
        .max_iterations
        .unwrap_or_else(|| default_agent_iterations(args.profile));

    let mut target_files = Vec::new();
    for file in &args.files {
        if file.exists() && !file.is_file() {
            bail!("Target path is not a file: {}", file.display());
        }
        target_files.push(file.clone());
    }

    if args.require_clean_git && args.apply {
        for file in &target_files {
            if file.exists() {
                ensure_clean_git_for_file(file)?;
            }
        }
    }

    let mut backups: HashMap<PathBuf, String> = HashMap::new();
    let mut changed_files: Vec<String> = Vec::new();
    let mut verification_feedback = String::new();
    let mut last_verify_commands: Vec<String> = Vec::new();
    let mut success = false;

    for iteration in 1..=max_iterations {
        let mut file_payload = String::new();
        let mut file_map: HashMap<String, String> = HashMap::new();
        let mut allowed_map: HashMap<String, PathBuf> = HashMap::new();

        for file in &target_files {
            let file_key = file.display().to_string();
            let normalized = normalize_file_key(&file_key);
            allowed_map.insert(normalized, file.clone());

            if file.exists() {
                let content = fs::read_to_string(file)
                    .with_context(|| format!("Failed to read {}", file.display()))?;
                file_map.insert(file_key.clone(), content.clone());
                file_payload.push_str(&format!(
                    "\n<file path=\"{}\">\n{}\n</file>\n",
                    file_key, content
                ));
            } else {
                file_map.insert(file_key.clone(), String::new());
                file_payload.push_str(&format!(
                    "\n<file path=\"{}\">\n<missing_file />\n</file>\n",
                    file_key
                ));
            }
        }

        let prompt = build_agent_prompt(
            &args.goal,
            &target_files,
            &file_payload,
            &args.verify,
            &verification_feedback,
            iteration,
            max_iterations,
        );

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: "You are Clive agent. Produce only JSON matching the required schema with concrete actions. Keep changes minimal, deterministic, and safe.".to_string(),
                ..Default::default()
            },
            Message {
                role: "user".to_string(),
                content: prompt,
                ..Default::default()
            },
        ];

        let response = ollama.chat(&model, messages)?;
        let mut plan = parse_agent_plan(&response.message.content)?;

        if !args.json {
            println!("\n[agent] Iteration {iteration}/{max_iterations}: {}", plan.summary);
        }

        for edit in plan.edits {
            plan.actions.push(AgentAction::EditFile {
                file: edit.file,
                updated_content: edit.updated_content,
                reason: edit.reason,
            });
        }

        let mut applied_in_iteration = false;

        for action in plan.actions {
            match action {
                AgentAction::EditFile {
                    file,
                    updated_content,
                    reason,
                } => {
                    let normalized = normalize_file_key(&file);
                    let target = allowed_map
                        .get(&normalized)
                        .ok_or_else(|| anyhow!("Agent attempted to edit disallowed file: {file}"))?;

                    let old = file_map
                        .get(&target.display().to_string())
                        .ok_or_else(|| anyhow!("Missing preloaded file content for {}", target.display()))?;

                    if updated_content == *old {
                        continue;
                    }

                    if !args.json {
                        if let Some(reason) = &reason {
                            println!("\n[agent] {}: {}", target.display(), reason);
                        }
                        print_diff(target, old, &updated_content);
                    }

                    if args.apply {
                        backups.entry(target.clone()).or_insert_with(|| old.clone());
                        fs::write(target, &updated_content)
                            .with_context(|| format!("Failed to write {}", target.display()))?;
                        applied_in_iteration = true;

                        let target_name = target.display().to_string();
                        if !changed_files.iter().any(|f| f == &target_name) {
                            changed_files.push(target_name);
                        }
                    }
                }
                AgentAction::AddFile { file, content, reason } => {
                    let normalized = normalize_file_key(&file);
                    let target = allowed_map
                        .get(&normalized)
                        .ok_or_else(|| anyhow!("Agent attempted to add disallowed file: {file}"))?;

                    let old = if target.exists() {
                        fs::read_to_string(target)
                            .with_context(|| format!("Failed to read {}", target.display()))?
                    } else {
                        String::new()
                    };

                    if !args.json {
                        if let Some(reason) = &reason {
                            println!("\n[agent] create {}: {}", target.display(), reason);
                        }
                        print_diff(target, &old, &content);
                    }

                    if args.apply {
                        if let Some(parent) = target.parent() {
                            fs::create_dir_all(parent).with_context(|| {
                                format!("Failed to create parent directory for {}", target.display())
                            })?;
                        }

                        backups.entry(target.clone()).or_insert(old);
                        fs::write(target, &content)
                            .with_context(|| format!("Failed to write {}", target.display()))?;
                        applied_in_iteration = true;

                        let target_name = target.display().to_string();
                        if !changed_files.iter().any(|f| f == &target_name) {
                            changed_files.push(target_name);
                        }
                    }
                }
                AgentAction::RunCommand { command, reason } => {
                    if !args.allow_agent_commands {
                        bail!(
                            "Agent returned run_command action but --allow-agent-commands is not enabled"
                        );
                    }

                    if !args.json {
                        if let Some(reason) = &reason {
                            println!("\n[agent] run `{}`: {}", command, reason);
                        } else {
                            println!("\n[agent] run `{}`", command);
                        }
                    }

                    if args.apply {
                        let command_output = run_shell_command(&command)
                            .with_context(|| format!("Failed to run agent command: {command}"))?;
                        verification_feedback.push_str(&format!(
                            "\n[agent_command] {command}\n{command_output}\n"
                        ));
                        applied_in_iteration = true;
                    }
                }
            }
        }

        if !args.apply {
            success = true;
            break;
        }

        if !applied_in_iteration && plan.done.unwrap_or(false) {
            success = true;
            break;
        }

        let verify_commands = if args.verify.is_empty() {
            default_verify_commands(args.profile)
        } else {
            args.verify.clone()
        };

        last_verify_commands = verify_commands.clone();
        let verify_report = run_verify_commands(&verify_commands)?;
        verification_feedback.push_str(&verify_report.output);

        if verify_report.success {
            if !args.json {
                println!("[agent] Verification passed.");
            }
            if plan.done.unwrap_or(true) {
                success = true;
                break;
            }
        } else if !args.json {
            println!("[agent] Verification failed. Refining in next iteration...");
        }
    }

    let mut rolled_back = false;
    if args.apply && !success && args.rollback_on_fail {
        for (path, content) in backups {
            if content.is_empty() {
                let _ = fs::remove_file(&path);
            } else {
                fs::write(&path, content)
                    .with_context(|| format!("Failed to restore {}", path.display()))?;
            }
        }
        rolled_back = true;
    }

    if args.json {
        let report = AgentRunReport {
            goal: args.goal,
            iterations: max_iterations,
            changed_files,
            verify_commands: last_verify_commands,
            success,
            rolled_back,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("Failed to serialize report")?
        );
    }

    if !success {
        bail!("Agent workflow did not converge within {max_iterations} iteration(s)");
    }

    Ok(())
}

fn build_agent_prompt(
    goal: &str,
    files: &[PathBuf],
    file_payload: &str,
    verify_commands: &[String],
    verification_feedback: &str,
    iteration: usize,
    max_iterations: usize,
) -> String {
    let file_list = files
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let verify_list = if verify_commands.is_empty() {
        "(none)".to_string()
    } else {
        verify_commands.join("; ")
    };

    format!(
        "Goal: {goal}\n\
Iteration: {iteration}/{max_iterations}\n\
Allowed files: {file_list}\n\
Verification commands: {verify_list}\n\
Previous verification output:\n{verification_feedback}\n\n\
Return ONLY JSON with this shape:\n\
{{\"summary\":\"...\",\"done\":true|false,\"actions\":[\
{{\"type\":\"edit_file\",\"file\":\"<allowed path>\",\"updated_content\":\"<full file text>\",\"reason\":\"...\"}},\
{{\"type\":\"add_file\",\"file\":\"<allowed path>\",\"content\":\"<full file text>\",\"reason\":\"...\"}},\
{{\"type\":\"run_command\",\"command\":\"cargo check -q\",\"reason\":\"...\"}}\
]}}\n\
For backward compatibility you may also return edits[] instead of actions[].\n\
Do not include markdown fences.\n\
Current files:\n{file_payload}"
    )
}

fn parse_agent_plan(raw: &str) -> Result<AgentPlan> {
    let json_text = extract_json_object(raw)?;
    serde_json::from_str(&json_text).context("Failed to parse agent JSON plan")
}

fn extract_json_object(raw: &str) -> Result<String> {
    if raw.trim_start().starts_with('{') {
        return Ok(raw.trim().to_string());
    }

    let start = raw
        .find('{')
        .ok_or_else(|| anyhow!("Agent response does not contain JSON object"))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| anyhow!("Agent response does not contain closing JSON brace"))?;
    if end <= start {
        bail!("Malformed JSON object in agent response");
    }

    Ok(raw[start..=end].trim().to_string())
}

fn normalize_file_key(path: &str) -> String {
    path.replace('\\', "/")
}

fn default_agent_iterations(profile: AgentProfile) -> usize {
    match profile {
        AgentProfile::Quick => 1,
        AgentProfile::Balanced => 2,
        AgentProfile::Strict => 3,
    }
}

fn default_verify_commands(profile: AgentProfile) -> Vec<String> {
    match profile {
        AgentProfile::Quick => vec!["cargo check -q".to_string()],
        AgentProfile::Balanced => vec!["cargo check -q".to_string()],
        AgentProfile::Strict => vec!["cargo check -q".to_string(), "cargo test -q".to_string()],
    }
}

struct VerifyReport {
    success: bool,
    output: String,
}

fn run_verify_commands(commands: &[String]) -> Result<VerifyReport> {
    if commands.is_empty() {
        return Ok(VerifyReport {
            success: true,
            output: String::new(),
        });
    }

    let mut output = String::new();
    let mut success = true;

    for cmd in commands {
        let result = run_shell_command(cmd)
            .with_context(|| format!("Verification command failed to run: {cmd}"))?;
        output.push_str(&format!("$ {cmd}\n{}\n", result));

        if !result.starts_with("exit=0") {
            success = false;
            break;
        }
    }

    Ok(VerifyReport { success, output })
}

fn run_shell_command(command: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("Failed to execute shell command: {command}"))?;

    let code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!("exit={code}\nstdout:\n{stdout}\nstderr:\n{stderr}"))
}

fn cmd_ollama(ollama: &OllamaClient, args: OllamaArgs) -> Result<()> {
    match args.command {
        OllamaCommand::Serve(s) => cmd_ollama_serve(s),
        OllamaCommand::Pull(p) => cmd_ollama_pull(ollama, p),
        OllamaCommand::Rm(m) => cmd_ollama_rm(ollama, m),
        OllamaCommand::Recommend(r) => cmd_ollama_recommend(ollama, r),
    }
}

fn cmd_ollama_serve(args: ServeArgs) -> Result<()> {
    if args.detach {
        let child = Command::new("ollama")
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to launch 'ollama serve' in background")?;

        println!("Ollama started in background (pid {}).", child.id());
        println!("Use 'clive doctor' to verify it's reachable.");
        return Ok(());
    }

    println!("Starting Ollama in foreground. Press Ctrl+C to stop.");
    let status = Command::new("ollama")
        .arg("serve")
        .status()
        .context("Failed to execute 'ollama serve'")?;

    if !status.success() {
        bail!("ollama serve exited with status {status}");
    }

    Ok(())
}

fn cmd_ollama_pull(ollama: &OllamaClient, args: PullArgs) -> Result<()> {
    println!("Pulling model: {}", args.model);
    pull_model_via_cli(&args.model)?;

    println!("Model pull completed: {}", args.model);

    if let Ok(tags) = ollama.tags()
        && let Some(model) = tags.models.into_iter().find(|m| m.name.starts_with(&args.model))
    {
        let size = model.size.map(human_size).unwrap_or_else(|| "unknown".to_string());
        println!("Installed: {} ({})", model.name, size);
    }

    Ok(())
}

fn pull_model_via_cli(model: &str) -> Result<()> {
    pull_model_with_runner(model, run_ollama_command)
}

fn pull_model_with_runner<F>(model: &str, runner: F) -> Result<()>
where
    F: FnOnce(&str, &[&str]) -> io::Result<ExitStatus>,
{
    let status = runner("pull", &[model]).context("Failed to execute 'ollama pull'")?;

    if !status.success() {
        bail!("ollama pull exited with status {status}");
    }

    Ok(())
}

fn run_ollama_command(command: &str, args: &[&str]) -> io::Result<ExitStatus> {
    Command::new("ollama").arg(command).args(args).status()
}

fn cmd_ollama_rm(ollama: &OllamaClient, args: ModelNameArg) -> Result<()> {
    ollama.delete(&args.model)?;
    println!("Removed model: {}", args.model);
    Ok(())
}

fn cmd_ollama_recommend(ollama: &OllamaClient, args: RecommendArgs) -> Result<()> {
    let installed_names = ollama.installed_model_names()?;
    let profile = recommendations_for_profile(args.profile);

    println!("Recommended models for profile: {:?}\n", args.profile);
    for (model, note) in profile {
        let installed = installed_names.iter().any(|name| name.starts_with(model));
        if args.installed_only && !installed {
            continue;
        }
        let state = if installed { "installed" } else { "not installed" };
        println!("- {model} [{state}]");
        println!("  {note}");
        if !installed {
            println!("  Install: clive ollama pull {model}");
        }
    }

    Ok(())
}

fn recommendations_for_profile(profile: RecommendProfile) -> Vec<(&'static str, &'static str)> {
    match profile {
        RecommendProfile::Coding => vec![
            (
                "qwen2.5-coder:latest",
                "Great default coding model with strong code synthesis and refactoring.",
            ),
            (
                "codellama:latest",
                "Solid coder with broad language support and reliable completions.",
            ),
            (
                "llama3.1:latest",
                "Generalist model useful for design discussion and code explanations.",
            ),
        ],
        RecommendProfile::Rust => vec![
            (
                "qwen2.5-coder:latest",
                "Very good at Rust refactors, traits, lifetimes, and error handling patterns.",
            ),
            (
                "codellama:latest",
                "Useful for idiomatic Rust scaffolding and incremental edits.",
            ),
            (
                "llama3.1:latest",
                "Strong for architecture review and documentation generation.",
            ),
        ],
        RecommendProfile::Fast => vec![
            (
                "llama3.1:8b",
                "Fast and capable for quick coding Q&A on local hardware.",
            ),
            (
                "qwen2.5-coder:7b",
                "Lower-latency coding model variant with good quality.",
            ),
            (
                "phi3:mini",
                "Very lightweight option for constrained machines.",
            ),
        ],
        RecommendProfile::Reasoning => vec![
            (
                "llama3.1:latest",
                "Good for complex reasoning and step-by-step design exploration.",
            ),
            (
                "qwen2.5-coder:latest",
                "Balances reasoning depth with concrete code generation.",
            ),
            (
                "mistral:latest",
                "Good alternative for planning and architectural trade-off analysis.",
            ),
        ],
    }
}

fn cmd_completions(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    let mut output = Vec::new();
    generate(args.shell, &mut cmd, name, &mut output);

    if let Err(err) = io::stdout().write_all(&output) {
        if err.kind() == io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(err).context("Failed writing completion script to stdout");
    }

    Ok(())
}

fn cmd_edit(
    ollama: &OllamaClient,
    args: EditArgs,
    patch_mode: bool,
    default_model: &Option<String>,
) -> Result<()> {
    if !args.file.exists() {
        bail!("File does not exist: {}", args.file.display());
    }

    if !args.file.is_file() {
        bail!("Path is not a file: {}", args.file.display());
    }

    let file_content = fs::read_to_string(&args.file)
        .with_context(|| format!("Failed to read {}", args.file.display()))?;

    let model = resolve_model(ollama, args.model, default_model);
    let prompt = build_edit_prompt(&args.file, &file_content, &args.instruction);

    let messages = vec![
        Message {
            role: "system".to_string(),
            content: "You are Clive, a careful coding assistant. Follow the requested change exactly and return only the updated file content in <updated_file>...</updated_file>.".to_string(),
            ..Default::default()
        },
        Message {
            role: "user".to_string(),
            content: prompt,
            ..Default::default()
        },
    ];

    let response = ollama.chat(&model, messages)?;
    let new_content = extract_updated_file(&response.message.content)?;

    if new_content == file_content {
        println!("No changes produced.");
        return Ok(());
    }

    if patch_mode {
        print_unified_diff(&args.file, &file_content, &new_content);
    } else {
        print_diff(&args.file, &file_content, &new_content);
    }

    if !args.write {
        println!("\nPreview only. Re-run with --write to apply changes.");
        return Ok(());
    }

    if args.require_clean_git {
        ensure_clean_git_for_file(&args.file)?;
    }

    if args.backup {
        let backup_path = args.file.with_extension("bak");
        fs::write(&backup_path, &file_content)
            .with_context(|| format!("Failed to write backup to {}", backup_path.display()))?;
        println!("Backup created: {}", backup_path.display());
    }

    fs::write(&args.file, new_content)
        .with_context(|| format!("Failed to write {}", args.file.display()))?;

    println!("Applied changes to {}", args.file.display());

    if args.stage {
        git_add_file(&args.file)?;
        println!("Staged {}", args.file.display());
    }

    Ok(())
}

fn build_edit_prompt(path: &Path, content: &str, instruction: &str) -> String {
    format!(
        "You must edit one file.\n\
File path: {}\n\
Instruction: {}\n\n\
Return format requirements:\n\
1) Return ONLY one XML block: <updated_file>...</updated_file>\n\
2) The block content must be the complete updated file\n\
3) Do not include markdown fences or explanations\n\n\
Current file content:\n\
<current_file>\n{}\n</current_file>",
        path.display(),
        instruction,
        content
    )
}

fn extract_updated_file(raw: &str) -> Result<String> {
    let start_tag = "<updated_file>";
    let end_tag = "</updated_file>";

    let start = raw
        .find(start_tag)
        .ok_or_else(|| anyhow!("Model response missing <updated_file> tag"))?;
    let end = raw
        .find(end_tag)
        .ok_or_else(|| anyhow!("Model response missing </updated_file> tag"))?;

    if end <= start {
        bail!("Malformed <updated_file> block");
    }

    let inner_start = start + start_tag.len();
    let updated = raw[inner_start..end].trim_matches('\n').to_string();
    Ok(updated)
}

fn print_diff(path: &Path, old: &str, new: &str) {
    println!("Proposed changes for {}", path.display());
    let diff = TextDiff::from_lines(old, new);
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            similar::ChangeTag::Delete => "-",
            similar::ChangeTag::Insert => "+",
            similar::ChangeTag::Equal => " ",
        };
        print!("{}{}", sign, change);
    }
}

fn print_unified_diff(path: &Path, old: &str, new: &str) {
    let diff = TextDiff::from_lines(old, new);
    let rendered = diff
        .unified_diff()
        .header(
            &format!("a/{}", path.display()),
            &format!("b/{}", path.display()),
        )
        .context_radius(3)
        .to_string();
    println!("{rendered}");
}

fn ensure_clean_git_for_file(path: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .arg("--")
        .arg(path)
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        bail!("git status failed; cannot enforce --require-clean-git");
    }

    let dirty = String::from_utf8_lossy(&output.stdout);
    if !dirty.trim().is_empty() {
        bail!(
            "Refusing to write because file has uncommitted git changes: {}",
            path.display()
        );
    }

    Ok(())
}

fn git_add_file(path: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("add")
        .arg(path)
        .status()
        .context("Failed to run git add")?;

    if !status.success() {
        bail!("git add failed for {}", path.display());
    }

    Ok(())
}

fn human_size(size: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut size_f = size as f64;
    let mut unit = 0usize;
    while size_f >= 1024.0 && unit < units.len() - 1 {
        size_f /= 1024.0;
        unit += 1;
    }
    format!("{size_f:.1} {}", units[unit])
}

/// A minimal terminal spinner shown while waiting on a blocking request.
/// It runs on a background thread and only animates when stderr is a TTY, so
/// piped/redirected output stays clean.
struct Spinner {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    active: bool,
}

impl Spinner {
    fn start(label: &str) -> Self {
        use std::io::IsTerminal;

        let active = io::stderr().is_terminal();
        if !active {
            return Self {
                running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                handle: None,
                active,
            };
        }

        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let thread_flag = running.clone();
        let label = label.to_string();
        let handle = std::thread::spawn(move || {
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut idx = 0usize;
            while thread_flag.load(std::sync::atomic::Ordering::Relaxed) {
                eprint!("\r{} {label}...", frames[idx % frames.len()]);
                let _ = io::stderr().flush();
                idx += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
        });

        Self {
            running,
            handle: Some(handle),
            active,
        }
    }

    fn stop(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.running
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let _ = handle.join();
            if self.active {
                // Clear the spinner line.
                eprint!("\r\x1b[2K");
                let _ = io::stderr().flush();
            }
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.finish();
    }
}

struct OllamaClient {
    /// Client used for streaming / long-running requests – no read timeout.
    client: Client,
    /// Client used for quick management requests (tags, version, pull progress).
    /// Times out after 30 seconds so network errors surface quickly.
    quick_client: Client,
    base_url: String,
}

impl OllamaClient {
    fn new(base_url: String) -> Result<Self> {
        // No read timeout: large models (deepseek, llama3, etc.) can take
        // many seconds to produce the first token, so we must not cut the
        // connection while waiting for inference to begin.
        let client = Client::builder()
            .timeout(None)
            .build()
            .context("Failed to initialize HTTP client")?;

        let quick_client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to initialize quick HTTP client")?;

        Ok(Self {
            client,
            quick_client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    fn chat(&self, model: &str, messages: Vec<Message>) -> Result<ChatResponse> {
        let url = format!("{}/api/chat", self.base_url);
        let req = ChatRequest {
            model: model.to_string(),
            messages,
            stream: false,
        };

        let response = self
            .client
            .post(url)
            .json(&req)
            .send()
            .context("Failed to call Ollama chat endpoint")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "<unreadable body>".to_string());
            bail!("Ollama chat failed: {status} - {body}");
        }

        response
            .json::<ChatResponse>()
            .context("Failed to parse Ollama chat response")
    }

    fn chat_stream<F>(&self, model: &str, messages: Vec<Message>, mut on_chunk: F) -> Result<String>
    where
        F: FnMut(&str),
    {
        let url = format!("{}/api/chat", self.base_url);
        let req = ChatRequest {
            model: model.to_string(),
            messages,
            stream: true,
        };

        let response = self
            .client
            .post(url)
            .json(&req)
            .send()
            .context("Failed to call Ollama chat endpoint")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "<unreadable body>".to_string());
            bail!("Ollama stream chat failed: {status} - {body}");
        }

        let mut full = String::new();
        let mut thinking_started = false;
        let reader = BufReader::new(response);
        for line in reader.lines() {
            let line = line.context("Failed to read streamed Ollama response")?;
            if line.trim().is_empty() {
                continue;
            }

            let chunk: StreamChatResponse = serde_json::from_str(&line)
                .with_context(|| format!("Invalid streamed JSON chunk: {line}"))?;

            if let Some(err) = chunk.error {
                bail!("Ollama streaming error: {err}");
            }

            if let Some(msg) = chunk.message {
                // Thinking/reasoning models (qwen3, deepseek-r1, etc.) stream
                // their reasoning in a separate `thinking` field while
                // `content` stays empty. Surface it to stderr so the user sees
                // progress instead of an apparent hang, but keep it out of the
                // returned answer so conversation history stays clean.
                if let Some(thinking) = msg.thinking.as_deref()
                    && !thinking.is_empty()
                {
                    if !thinking_started {
                        eprint!("\x1b[2m[thinking] ");
                        thinking_started = true;
                    }
                    eprint!("{thinking}");
                    let _ = io::stderr().flush();
                }

                if !msg.content.is_empty() {
                    if thinking_started {
                        // Close the thinking block before the real answer.
                        eprintln!("\x1b[0m");
                        thinking_started = false;
                    }
                    on_chunk(&msg.content);
                    full.push_str(&msg.content);
                }
            }

            if chunk.done {
                break;
            }
        }

        if thinking_started {
            eprintln!("\x1b[0m");
        }

        Ok(full)
    }

    fn tags(&self) -> Result<TagsResponse> {
        let url = format!("{}/api/tags", self.base_url);
        let response = self
            .quick_client
            .get(url)
            .send()
            .context("Failed to call Ollama tags endpoint")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "<unreadable body>".to_string());
            bail!("Ollama tags failed: {status} - {body}");
        }

        response
            .json::<TagsResponse>()
            .context("Failed to parse Ollama tags response")
    }

    fn installed_model_names(&self) -> Result<Vec<String>> {
        Ok(self.tags()?.models.into_iter().map(|m| m.name).collect())
    }

    fn version(&self) -> Result<String> {
        let url = format!("{}/api/version", self.base_url);
        let response = self
            .quick_client
            .get(url)
            .send()
            .context("Failed to call Ollama version endpoint")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "<unreadable body>".to_string());
            bail!("Ollama version failed: {status} - {body}");
        }

        let value: serde_json::Value = response
            .json()
            .context("Failed to parse Ollama version response")?;

        let version = value
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Ollama version response missing string field 'version'"))?;

        Ok(version.to_string())
    }

    fn delete(&self, model: &str) -> Result<()> {
        let url = format!("{}/api/delete", self.base_url);
        let response = self
            .quick_client
            .delete(url)
            .json(&serde_json::json!({ "model": model }))
            .send()
            .context("Failed to call Ollama delete endpoint")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "<unreadable body>".to_string());
            bail!("Ollama delete failed: {status} - {body}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn cli_parses_ollama_recommend_aliases_and_flags() {
        let cli = Cli::try_parse_from([
            "clive",
            "--no-banner",
            "ollama",
            "recommend",
            "--profile",
            "rust",
            "--installed-only",
        ])
        .expect("cli should parse");

        assert!(cli.no_banner);

        match cli.command {
            Commands::Ollama(args) => match args.command {
                OllamaCommand::Recommend(recommend) => {
                    assert!(matches!(recommend.profile, RecommendProfile::Rust));
                    assert!(recommend.installed_only);
                }
                _ => panic!("expected recommend command"),
            },
            _ => panic!("expected ollama command"),
        }
    }

    #[test]
    fn cli_parses_agent_command_with_safety_and_automation_flags() {
        let cli = Cli::try_parse_from([
            "clive",
            "agent",
            "refactor error handling",
            "--files",
            "src/main.rs",
            "--verify",
            "cargo check -q",
            "--apply",
            "--rollback-on-fail",
            "--json",
            "--profile",
            "strict",
            "--max-iterations",
            "3",
        ])
        .expect("agent args should parse");

        match cli.command {
            Commands::Agent(agent) => {
                assert!(agent.apply);
                assert!(agent.rollback_on_fail);
                assert!(agent.json);
                assert!(!agent.allow_agent_commands);
                assert!(matches!(agent.profile, AgentProfile::Strict));
                assert_eq!(agent.max_iterations, Some(3));
                assert_eq!(agent.files.len(), 1);
                assert_eq!(agent.verify, vec!["cargo check -q"]);
            }
            _ => panic!("expected agent command"),
        }
    }

    #[test]
    fn resolve_model_name_prefers_local_global_installed_default() {
        let installed = vec!["installed-model:latest".to_string()];

        assert_eq!(
            resolve_model_name(&installed, Some("local-model".to_string()), &None),
            "local-model"
        );
        assert_eq!(
            resolve_model_name(&installed, None, &Some("global-model".to_string())),
            "global-model"
        );
        assert_eq!(
            resolve_model_name(&installed, None, &None),
            "installed-model:latest"
        );
        assert_eq!(resolve_model_name(&[], None, &None), DEFAULT_MODEL);
    }

    #[test]
    fn recommendations_are_profile_specific_and_complete() {
        let coding = recommendations_for_profile(RecommendProfile::Coding);
        let rust = recommendations_for_profile(RecommendProfile::Rust);
        let fast = recommendations_for_profile(RecommendProfile::Fast);
        let reasoning = recommendations_for_profile(RecommendProfile::Reasoning);

        assert_eq!(coding.len(), 3);
        assert_eq!(rust.len(), 3);
        assert_eq!(fast.len(), 3);
        assert_eq!(reasoning.len(), 3);
        assert!(rust.iter().any(|(name, _)| *name == "qwen2.5-coder:latest"));
    }

    #[test]
    fn extract_updated_file_reads_expected_content() {
        let raw = "noise <updated_file>fn main() {}\n</updated_file> trailing";
        let updated = extract_updated_file(raw).expect("expected updated content");
        assert_eq!(updated, "fn main() {}");
    }

    #[test]
    fn extract_updated_file_rejects_missing_tags() {
        let err = extract_updated_file("no tags here").expect_err("should fail");
        assert!(err.to_string().contains("updated_file"));
    }

    #[test]
    fn build_edit_prompt_contains_path_instruction_and_content() {
        let prompt = build_edit_prompt(Path::new("src/lib.rs"), "old content", "make it better");
        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("make it better"));
        assert!(prompt.contains("old content"));
    }

    #[test]
    fn human_size_formats_common_values() {
        assert_eq!(human_size(0), "0.0 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn pull_model_with_runner_invokes_pull_command() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<String>)>::new()));
        let calls_clone = calls.clone();

        let result = pull_model_with_runner("qwen2.5-coder:latest", move |command, args| {
            calls_clone.lock().unwrap().push((
                command.to_string(),
                args.iter().map(|arg| arg.to_string()).collect(),
            ));
            Ok(success_status())
        });

        assert!(result.is_ok());
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "pull");
        assert_eq!(recorded[0].1, vec!["qwen2.5-coder:latest"]);
    }

    #[test]
    fn pull_model_with_runner_surfaces_non_zero_exit_status() {
        let err = pull_model_with_runner("broken-model", |_, _| Ok(failure_status()))
            .expect_err("expected failure");
        assert!(err.to_string().contains("ollama pull exited"));
    }

    #[test]
    fn extract_json_object_handles_wrapped_model_output() {
        let raw = "Here you go:\n```json\n{\"summary\":\"ok\",\"edits\":[],\"done\":true}\n```";
        let extracted = extract_json_object(raw).expect("json should be extracted");
        assert!(extracted.starts_with('{'));
        assert!(extracted.ends_with('}'));
    }

    #[test]
    fn parse_agent_plan_accepts_minimal_valid_payload() {
        let raw = r#"{"summary":"done","edits":[],"done":true}"#;
        let plan = parse_agent_plan(raw).expect("plan should parse");
        assert_eq!(plan.summary, "done");
        assert!(plan.done.unwrap_or(false));
        assert!(plan.edits.is_empty());
    }

    #[test]
    fn parse_agent_plan_accepts_action_payload() {
        let raw = r#"{"summary":"step","actions":[{"type":"run_command","command":"cargo check -q","reason":"verify"}],"done":false}"#;
        let plan = parse_agent_plan(raw).expect("action plan should parse");
        assert_eq!(plan.summary, "step");
        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            AgentAction::RunCommand { command, reason } => {
                assert_eq!(command, "cargo check -q");
                assert_eq!(reason.as_deref(), Some("verify"));
            }
            _ => panic!("expected run_command action"),
        }
    }

    #[test]
    fn default_agent_iterations_follow_profile() {
        assert_eq!(default_agent_iterations(AgentProfile::Quick), 1);
        assert_eq!(default_agent_iterations(AgentProfile::Balanced), 2);
        assert_eq!(default_agent_iterations(AgentProfile::Strict), 3);
    }

    #[test]
    fn default_verify_commands_include_tests_for_strict_profile() {
        let strict = default_verify_commands(AgentProfile::Strict);
        assert_eq!(strict.len(), 2);
        assert!(strict.iter().any(|c| c == "cargo test -q"));
    }

    #[test]
    fn ensure_clean_git_for_file_errors_out_for_non_repo_paths() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("file.txt");
        fs::write(&file_path, "hello").expect("write file");

        let result = ensure_clean_git_for_file(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn combine_prompt_with_stdin_returns_prompt_when_not_forced() {
        // A non-empty prompt without --stdin must never touch stdin, so this is
        // deterministic regardless of how the test harness wires up stdin.
        let combined = combine_prompt_with_stdin("hello", false).expect("combine");
        assert_eq!(combined, "hello");
    }

    #[test]
    fn message_deserializes_thinking_field_and_omits_it_when_absent() {
        // Thinking models stream a separate `thinking` field with empty content.
        let raw = r#"{"role":"assistant","content":"","thinking":"let me reason"}"#;
        let msg: Message = serde_json::from_str(raw).expect("deserialize");
        assert_eq!(msg.thinking.as_deref(), Some("let me reason"));
        assert_eq!(msg.content, "");

        // Missing content/thinking should still deserialize (defaults apply).
        let minimal: Message = serde_json::from_str(r#"{"role":"user"}"#).expect("deserialize");
        assert_eq!(minimal.content, "");
        assert!(minimal.thinking.is_none());

        // Requests must not include a null thinking field.
        let outgoing = Message {
            role: "user".to_string(),
            content: "hi".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&outgoing).expect("serialize");
        assert!(!json.contains("thinking"));
    }

    #[test]
    fn clive_config_serializes_only_set_fields() {
        let config = CliveConfig {
            model: Some("qwen2.5-coder:latest".to_string()),
            ollama_url: None,
            system: None,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        assert!(json.contains("qwen2.5-coder:latest"));
        assert!(!json.contains("ollama_url"));
        assert!(!json.contains("system"));
    }

    #[test]
    fn clive_config_round_trips_through_json() {
        let config = CliveConfig {
            model: Some("m".to_string()),
            ollama_url: Some("http://localhost:1234".to_string()),
            system: Some("be brief".to_string()),
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let parsed: CliveConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.model.as_deref(), Some("m"));
        assert_eq!(parsed.ollama_url.as_deref(), Some("http://localhost:1234"));
        assert_eq!(parsed.system.as_deref(), Some("be brief"));
    }

    #[test]
    fn config_file_path_honors_explicit_override() {
        // SAFETY: single-threaded test setting/clearing a process env var.
        unsafe { std::env::set_var("CLIVE_CONFIG", "/tmp/clive-test-config.json") };
        let path = config_file_path().expect("path");
        assert_eq!(path, PathBuf::from("/tmp/clive-test-config.json"));
        unsafe { std::env::remove_var("CLIVE_CONFIG") };
    }

    #[test]
    fn session_history_round_trips_through_disk() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("history.json");
        let file_str = file_path.to_string_lossy().to_string();

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: "sys".to_string(),
                ..Default::default()
            },
            Message {
                role: "user".to_string(),
                content: "hi".to_string(),
                ..Default::default()
            },
        ];

        let count = save_session_history(&file_str, &messages).expect("save");
        assert_eq!(count, 2);

        let loaded = load_session_history(&file_str).expect("load");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, "system");
        assert_eq!(loaded[1].content, "hi");
    }

    #[test]
    fn cli_parses_config_set_command() {
        let cli = Cli::try_parse_from([
            "clive",
            "--no-banner",
            "config",
            "set",
            "model",
            "qwen2.5-coder:latest",
        ])
        .expect("config set should parse");

        match cli.command {
            Commands::Config(args) => match args.command {
                ConfigCommand::Set(set) => {
                    assert!(matches!(set.key, ConfigKey::Model));
                    assert_eq!(set.value, "qwen2.5-coder:latest");
                }
                _ => panic!("expected config set command"),
            },
            _ => panic!("expected config command"),
        }
    }

    #[cfg(unix)]
    fn success_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    #[cfg(unix)]
    fn failure_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(1 << 8)
    }

    #[cfg(windows)]
    fn success_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn failure_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(1)
    }
}
