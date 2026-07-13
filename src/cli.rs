use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs};

use crate::agent::{AgentError, AgentRunner};
use crate::chat_client::{ChatClientError, ProviderChatClient};
use crate::clipboard::{
    AttachmentStore, ClipboardCapture, ClipboardError, ClipboardItem, StaticClipboard,
    SystemClipboard,
};
use crate::config::{ConfigError, ConfigStore};
use crate::diagnostics::{self, DiagnosticsOptions};
use crate::model_client::{ModelClientError, OpenAiCompatibleModelClient};
use crate::prompt::DEFAULT_SYSTEM_PROMPT;
use crate::providers::{
    AuthScheme, BuiltinProvider, CachePolicy, ChatApiFormat, ModelChoice, ModelDiscovery,
    ProviderConfig,
};
use crate::repl::{ReplError, ReplOptions, run_chat_tui, run_terminal_repl};
use crate::request::{CacheMode, ChatMessage, RequestEnvelope};
use crate::runtime::{RuntimeError, ToolCall, ToolRuntime, ToolScheduler};
use crate::tui::{TuiAction, TuiError, TuiProviderDraft, run_tui};

pub fn run<I, W>(args: I, output: &mut W) -> Result<(), CliError>
where
    I: IntoIterator<Item = String>,
    W: Write,
{
    let stdin = io::stdin();
    let mut input = stdin.lock();
    run_with_input(args, &mut input, output)
}

pub fn run_terminal<I>(args: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = String>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let command_name = args
        .first()
        .map(|program| command_name(program))
        .unwrap_or_else(|| "harness".to_string());
    let mut stdout = io::stdout();
    let stdin = io::stdin();
    let mut input = stdin.lock();

    if args.len() <= 1 && stdin.is_terminal() && stdout.is_terminal() {
        return default_terminal_launch(&command_name, &mut input, &mut stdout);
    }

    run_with_input(args, &mut input, &mut stdout)
}

pub fn run_with_input<I, R, W>(args: I, input: &mut R, output: &mut W) -> Result<(), CliError>
where
    I: IntoIterator<Item = String>,
    R: BufRead,
    W: Write,
{
    let mut args = args.into_iter();
    let command_name = args
        .next()
        .map(|program| command_name(&program))
        .unwrap_or_else(|| "harness".to_string());
    let args = args.collect::<Vec<_>>();
    match args.as_slice() {
        [scope, command, rest @ ..] if scope == "provider" && command == "models" => {
            provider_models(rest, output)
        }
        [scope, command, rest @ ..] if scope == "provider" && command == "list" => {
            provider_list(rest, output)
        }
        [scope, command, rest @ ..] if scope == "provider" && command == "add" => {
            provider_add(rest, input, output)
        }
        [scope, command] if scope == "provider" && command == "subscriptions" => {
            provider_subscriptions(output)
        }
        [scope, command, rest @ ..] if scope == "tool" && command == "call" => {
            tool_call(rest, output)
        }
        [scope, command, rest @ ..] if scope == "tool" && command == "batch" => {
            tool_batch(rest, output)
        }
        [scope, command, rest @ ..] if scope == "chat" && command == "once" => {
            chat_once(rest, output)
        }
        [scope, command, rest @ ..] if scope == "chat" && command == "stream" => {
            chat_stream(rest, output)
        }
        [scope, command, rest @ ..] if scope == "agent" && command == "run" => {
            agent_run(rest, output)
        }
        [scope, command, rest @ ..] if scope == "clipboard" && command == "paste" => {
            clipboard_paste(rest, output)
        }
        [command, rest @ ..] if command == "diagnostics" => diagnostics_command(rest, output),
        [command, rest @ ..] if command == "repl" => repl(rest, output),
        [] => default_launch(&command_name, input, output),
        _ => Err(CliError::Usage(help(&command_name))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultLaunch {
    Repl(DefaultReplLaunch),
    Setup(DefaultSetupLaunch),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultReplLaunch {
    pub config_path: PathBuf,
    pub workspace: PathBuf,
    pub provider_name: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultSetupLaunch {
    pub config_path: PathBuf,
    pub workspace: PathBuf,
    pub command_name: String,
}

pub fn resolve_default_launch(
    config_path: impl Into<PathBuf>,
    workspace: impl Into<PathBuf>,
    command_name: &str,
) -> Result<DefaultLaunch, CliError> {
    let config_path = config_path.into();
    let workspace = workspace.into();
    let config = ConfigStore::new(&config_path).load()?;

    if let Some(provider) = config
        .providers()
        .find(|provider| !provider.models().is_empty())
    {
        return Ok(DefaultLaunch::Repl(DefaultReplLaunch {
            config_path,
            workspace,
            provider_name: provider.name().to_string(),
            model: provider.models()[0].clone(),
        }));
    }

    Ok(DefaultLaunch::Setup(DefaultSetupLaunch {
        config_path,
        workspace,
        command_name: command_name.to_string(),
    }))
}

pub fn run_default_setup_interface<R, W>(
    config_path: impl Into<PathBuf>,
    workspace: impl Into<PathBuf>,
    command_name: &str,
    input: &mut R,
    output: &mut W,
) -> Result<DefaultReplLaunch, CliError>
where
    R: BufRead,
    W: Write,
{
    let config_path = config_path.into();
    let workspace = workspace.into();
    render_setup_interface_screen(&config_path, &workspace, command_name, output)?;

    loop {
        write_setup_prompt(output)?;
        output.flush()?;

        let mut command = String::new();
        if input.read_line(&mut command)? == 0 {
            return Err(CliError::Usage(
                "interface closed before provider was configured".to_string(),
            ));
        }

        match command.trim() {
            "" => {}
            "/exit" | "/quit" => {
                return Err(CliError::Usage(
                    "provider setup cancelled from interface".to_string(),
                ));
            }
            "/help" => {
                render_setup_commands(output)?;
            }
            "/providers" | "/provider subscriptions" => {
                render_setup_section(output, "Built-in providers")?;
                provider_subscriptions(output)?;
            }
            "/provider add" => {
                render_setup_section(output, "Provider setup")?;
                return provider_add_from_setup_interface(
                    config_path,
                    workspace,
                    command_name,
                    input,
                    output,
                );
            }
            other => {
                render_setup_section(output, "Input")?;
                writeln!(output, " unknown command: {other}")?;
                writeln!(
                    output,
                    " type /help for commands or /provider add to configure a provider"
                )?;
            }
        }
    }
}

fn provider_add_from_setup_interface<R, W>(
    config_path: PathBuf,
    workspace: PathBuf,
    command_name: &str,
    input: &mut R,
    output: &mut W,
) -> Result<DefaultReplLaunch, CliError>
where
    R: BufRead,
    W: Write,
{
    provider_add_interactive(
        ProviderFlags {
            config: Some(config_path.clone()),
            interactive: true,
            ..ProviderFlags::default()
        },
        input,
        output,
    )?;
    default_repl_launch_from_config(config_path, workspace, command_name, output)
}

fn render_setup_interface_screen<W: Write>(
    config_path: &Path,
    workspace: &Path,
    command_name: &str,
    output: &mut W,
) -> Result<(), io::Error> {
    render_setup_rule(output, '=')?;
    render_setup_title(output, command_name, "no provider configured")?;
    render_setup_rule(output, '-')?;
    writeln!(output, " workspace: {}", workspace.display())?;
    writeln!(output, " config:    {}", config_path.display())?;
    render_setup_rule(output, '-')?;
    writeln!(output, " No provider is configured yet.")?;
    writeln!(
        output,
        " Configure one inside this interface, then this launch will continue into the REPL."
    )?;
    writeln!(output)?;
    render_setup_commands(output)?;
    render_setup_rule(output, '-')?;
    Ok(())
}

fn render_setup_commands<W: Write>(output: &mut W) -> Result<(), io::Error> {
    writeln!(output, " Commands")?;
    writeln!(
        output,
        "   /provider add   configure a provider inside this interface"
    )?;
    writeln!(output, "   /providers      list built-in provider profiles")?;
    writeln!(output, "   /help           show commands")?;
    writeln!(output, "   /exit           quit")?;
    Ok(())
}

fn render_setup_section<W: Write>(output: &mut W, title: &str) -> Result<(), io::Error> {
    render_setup_rule(output, '-')?;
    writeln!(output, " {title}")?;
    render_setup_rule(output, '-')?;
    Ok(())
}

fn write_setup_prompt<W: Write>(output: &mut W) -> Result<(), io::Error> {
    write!(output, "[no provider] > ")
}

fn render_setup_title<W: Write>(
    output: &mut W,
    command_name: &str,
    status: &str,
) -> Result<(), io::Error> {
    let title = format!(" {command_name}");
    let padding = SETUP_INTERFACE_WIDTH.saturating_sub(title.len() + status.len());
    writeln!(output, "{}{}{}", title, " ".repeat(padding), status)
}

fn render_setup_rule<W: Write>(output: &mut W, fill: char) -> Result<(), io::Error> {
    writeln!(output, "{}", fill.to_string().repeat(SETUP_INTERFACE_WIDTH))
}

const SETUP_INTERFACE_WIDTH: usize = 88;

fn default_repl_launch_from_config<W: Write>(
    config_path: PathBuf,
    workspace: PathBuf,
    command_name: &str,
    output: &mut W,
) -> Result<DefaultReplLaunch, CliError> {
    let config = ConfigStore::new(&config_path).load()?;
    let provider = config
        .providers()
        .find(|provider| !provider.models().is_empty())
        .ok_or_else(|| CliError::Usage("provider setup did not save any models".to_string()))?;
    writeln!(
        output,
        "Starting {command_name} with {}/{}",
        provider.name(),
        provider.models()[0]
    )?;

    Ok(DefaultReplLaunch {
        config_path,
        workspace,
        provider_name: provider.name().to_string(),
        model: provider.models()[0].clone(),
    })
}

pub fn finish_tui_setup_action<W: Write>(
    action: TuiAction,
    config_path: impl Into<PathBuf>,
    workspace: impl Into<PathBuf>,
    command_name: &str,
    output: &mut W,
) -> Result<DefaultReplLaunch, CliError> {
    let config_path = config_path.into();
    let workspace = workspace.into();

    match action {
        TuiAction::SaveProvider(draft) => {
            let provider = provider_config_from_tui_draft(draft);
            save_provider(provider, config_path.clone(), output)?;
            default_repl_launch_from_config(config_path, workspace, command_name, output)
        }
        TuiAction::Exit | TuiAction::Continue | TuiAction::Command(_) => Err(CliError::Usage(
            "provider setup cancelled from interface".to_string(),
        )),
    }
}

fn default_launch<R, W>(command_name: &str, input: &mut R, output: &mut W) -> Result<(), CliError>
where
    R: BufRead,
    W: Write,
{
    let config_path = default_config_path();
    let workspace = env::current_dir().map_err(CliError::Io)?;
    match resolve_default_launch(&config_path, &workspace, command_name)? {
        DefaultLaunch::Repl(launch) => run_repl_launch(launch, output),
        DefaultLaunch::Setup(launch) => {
            let launch = run_default_setup_interface(
                launch.config_path,
                launch.workspace,
                &launch.command_name,
                input,
                output,
            )?;
            run_repl_launch(launch, output)
        }
    }
}

fn default_terminal_launch<R, W>(
    command_name: &str,
    _input: &mut R,
    output: &mut W,
) -> Result<(), CliError>
where
    R: BufRead,
    W: Write,
{
    let config_path = default_config_path();
    let workspace = env::current_dir().map_err(CliError::Io)?;
    match resolve_default_launch(&config_path, &workspace, command_name)? {
        DefaultLaunch::Repl(launch) => run_repl_launch(launch, output),
        DefaultLaunch::Setup(launch) => {
            let action = run_tui(&launch.command_name, &launch.config_path, &launch.workspace)?;
            let launch = finish_tui_setup_action(
                action,
                launch.config_path,
                launch.workspace,
                &launch.command_name,
                output,
            )?;
            run_repl_launch(launch, output)
        }
    }
}

fn run_repl_launch<W: Write>(launch: DefaultReplLaunch, output: &mut W) -> Result<(), CliError> {
    let args = vec![
        "--config".to_string(),
        launch.config_path.display().to_string(),
        "--workspace".to_string(),
        launch.workspace.display().to_string(),
        "--provider".to_string(),
        launch.provider_name,
        "--model".to_string(),
        launch.model,
    ];
    repl(&args, output)
}

fn provider_models<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ProviderFlags::parse(args)?;
    let provider = flags.provider()?;
    let client = OpenAiCompatibleModelClient::new(flags.timeout.unwrap_or(Duration::from_secs(15)));
    let discovery = client.list_models(&provider)?;

    for choice in discovery.choices() {
        match choice {
            ModelChoice::AddAll => writeln!(output, "Add all")?,
            ModelChoice::Model(model) => writeln!(output, "{model}")?,
        }
    }

    Ok(())
}

fn provider_list<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ProviderListFlags::parse(args)?;
    let config = ConfigStore::new(flags.config_path).load()?;
    for provider in config.providers() {
        writeln!(
            output,
            "{}\tmodels={}\turl={}\tauth={}\tkey={}\tchat={}\tcache={}",
            provider.name(),
            provider.models().join(","),
            provider.base_url(),
            auth_scheme_label(&provider.auth_scheme()),
            key_source_label(provider),
            chat_api_label(provider.chat_api()),
            cache_policy_label(&provider.cache_policy()),
        )?;
    }

    Ok(())
}

fn provider_subscriptions<W: Write>(output: &mut W) -> Result<(), CliError> {
    for provider in [
        BuiltinProvider::Codex,
        BuiltinProvider::Xiaomi,
        BuiltinProvider::Glm,
        BuiltinProvider::Kimi,
        BuiltinProvider::Claude,
        BuiltinProvider::DeepSeek,
    ] {
        let profile = provider.profile();
        writeln!(
            output,
            "{}\t{}\t{}\t{}",
            profile.name,
            profile.key_env,
            profile
                .model_hints
                .first()
                .copied()
                .unwrap_or("model-config-required"),
            cache_policy_label(&profile.cache_policy)
        )?;
    }

    Ok(())
}

fn tool_call<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ToolCallFlags::parse(args)?;
    let arguments = serde_json::from_str(&flags.arguments).map_err(CliError::Json)?;
    let result = ToolRuntime::new(flags.workspace)
        .with_shell_profile(crate::platform::ShellProfile::detected().cloned())
        .execute(ToolCall::new("cli-tool-call", flags.tool_name, arguments))?;
    serde_json::to_writer(&mut *output, &result).map_err(CliError::Json)?;
    writeln!(output)?;
    Ok(())
}

fn tool_batch<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ToolBatchFlags::parse(args)?;
    let calls: Vec<ToolCall> = serde_json::from_str(&flags.calls).map_err(CliError::Json)?;
    let mut scheduler = ToolScheduler::new(
        ToolRuntime::new(flags.workspace)
            .with_shell_profile(crate::platform::ShellProfile::detected().cloned()),
    )
    .with_max_concurrency(flags.max_concurrency);
    if let Some(timeout) = flags.timeout {
        scheduler = scheduler.with_timeout(timeout);
    }
    let results = scheduler.execute_batch(calls);
    serde_json::to_writer(&mut *output, &results).map_err(CliError::Json)?;
    writeln!(output)?;
    Ok(())
}

fn chat_once<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ChatOnceFlags::parse(args)?;
    let config = ConfigStore::new(flags.config_path).load()?;
    let provider = config
        .provider(&flags.provider_name)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("unknown provider: {}", flags.provider_name)))?;
    let envelope = RequestEnvelope::new(provider.name(), flags.model)
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_tools(ToolRuntime::tool_specs_with_shell(
            crate::platform::ShellProfile::detected(),
        ))
        .with_messages(vec![ChatMessage::user(flags.message)]);

    let response = ProviderChatClient::new(flags.timeout).send(&provider, &envelope)?;
    serde_json::to_writer(&mut *output, &response).map_err(CliError::Json)?;
    writeln!(output)?;
    Ok(())
}

fn chat_stream<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ChatOnceFlags::parse(args)?;
    let config = ConfigStore::new(flags.config_path).load()?;
    let provider = config
        .provider(&flags.provider_name)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("unknown provider: {}", flags.provider_name)))?;
    let envelope = RequestEnvelope::new(provider.name(), flags.model)
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_tools(ToolRuntime::tool_specs_with_shell(
            crate::platform::ShellProfile::detected(),
        ))
        .with_messages(vec![ChatMessage::user(flags.message)]);

    ProviderChatClient::new(flags.timeout).stream_text(&provider, &envelope, |delta| {
        let _ = write!(output, "{delta}");
        let _ = output.flush();
    })?;
    writeln!(output)?;
    Ok(())
}

fn agent_run<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = AgentRunFlags::parse(args)?;
    let config = ConfigStore::new(flags.config_path).load()?;
    let provider = config
        .provider(&flags.provider_name)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("unknown provider: {}", flags.provider_name)))?;

    let workspace = flags.workspace.clone();
    let mut runner = AgentRunner::new(provider, flags.model, flags.workspace)
        .with_timeout(flags.timeout)
        .with_max_tool_rounds(flags.max_rounds)
        .with_streaming(flags.stream);
    if let Some(max_tool_concurrency) = flags.max_tool_concurrency {
        runner = runner.with_max_tool_concurrency(max_tool_concurrency);
    }
    if let Some(tool_timeout) = flags.tool_timeout {
        runner = runner.with_tool_batch_timeout(tool_timeout);
    }

    let result = match runner.run(flags.message) {
        Ok(result) => result,
        Err(err) => {
            if let Some(trace) = err.trace() {
                auto_save_trace(&workspace, trace);
                if let Some(trace_path) = flags.trace_path.as_deref() {
                    write_json_file(trace_path, trace)?;
                }
                if let Some(tool_errors_path) = flags.tool_errors_path.as_deref() {
                    write_json_file(tool_errors_path, &trace.tool_errors())?;
                }
            }
            return Err(err.into());
        }
    };
    auto_save_trace(&workspace, &result.trace);
    if let Some(trace_path) = flags.trace_path {
        write_json_file(&trace_path, &result.trace)?;
    }
    if let Some(tool_errors_path) = flags.tool_errors_path {
        write_json_file(&tool_errors_path, &result.trace.tool_errors())?;
    }
    serde_json::to_writer(&mut *output, &result).map_err(CliError::Json)?;
    writeln!(output)?;
    Ok(())
}

/// Every agent run leaves a raw trace in the global store (`~/.harness` or
/// `HARNESS_HOME`) for offline analysis. One-shot `agent run` has no session,
/// so the wrapper's session id stays empty and the turn is always 1. Failures
/// only warn on stderr — persistence must never break the command itself.
fn auto_save_trace(workspace: &Path, trace: &crate::agent::AgentTrace) {
    let Some(store) = crate::session::SessionStore::for_workspace(workspace) else {
        return;
    };
    let wrapper = crate::session::TraceWrapper::new("", 1, trace.clone());
    if let Err(err) = store.write_trace(&wrapper) {
        eprintln!("warning: trace auto-save failed: {err}");
    }
}

fn write_json_file(path: &Path, value: &impl serde::Serialize) -> Result<(), CliError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(CliError::Io)?;
    }
    let mut file = fs::File::create(path).map_err(CliError::Io)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(CliError::Json)?;
    writeln!(file).map_err(CliError::Io)?;
    Ok(())
}

fn clipboard_paste<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ClipboardPasteFlags::parse(args)?;
    let capture = ClipboardCapture::new(AttachmentStore::new(&flags.workspace));
    let attachment = if let Some(text) = flags.text {
        capture.capture(&StaticClipboard::new(Some(ClipboardItem::Text(text))))?
    } else {
        capture.capture(&SystemClipboard)?
    };
    serde_json::to_writer(&mut *output, &attachment).map_err(CliError::Json)?;
    writeln!(output)?;
    Ok(())
}

/// Resolve which provider and model a REPL session should use. When `--provider`
/// or `--model` are omitted, fall back to the first configured provider and its
/// first model so `harness repl` works without flags once a provider is saved.
pub fn resolve_repl_provider(
    config: &crate::config::HarnessConfig,
    provider_flag: Option<&str>,
    model_flag: Option<&str>,
) -> Result<(ProviderConfig, String), CliError> {
    let provider = match provider_flag {
        Some(name) => config
            .provider(name)
            .cloned()
            .ok_or_else(|| CliError::Usage(format!("unknown provider: {name}")))?,
        None => config
            .providers()
            .find(|provider| !provider.models().is_empty())
            .cloned()
            .ok_or_else(|| {
                CliError::Usage(
                    "no provider configured. Run `harness` and use /provider add, or pass \
                     --provider NAME --model MODEL."
                        .to_string(),
                )
            })?,
    };

    let model = match model_flag {
        Some(model) => model.to_string(),
        None => provider.models().first().cloned().ok_or_else(|| {
            CliError::Usage(format!(
                "provider {} has no configured models; pass --model MODEL",
                provider.name()
            ))
        })?,
    };

    Ok((provider, model))
}

fn repl<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = ReplFlags::parse(args)?;
    let config = ConfigStore::new(flags.config_path).load()?;
    let (provider, model) = resolve_repl_provider(
        &config,
        flags.provider_name.as_deref(),
        flags.model.as_deref(),
    )?;
    let options = ReplOptions {
        provider,
        provider_catalog: config.providers().cloned().collect(),
        model,
        workspace: flags.workspace,
        timeout: flags.timeout,
        max_rounds: flags.max_rounds,
        max_tool_concurrency: flags.max_tool_concurrency,
        tool_timeout: flags.tool_timeout,
    };

    // On a real terminal, drive the Ratatui chat TUI; pipes and tests keep the
    // line-mode REPL so captured output and non-interactive runs stay stable.
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        run_chat_tui(options)?;
    } else {
        run_terminal_repl(options, output)?;
    }
    Ok(())
}

fn diagnostics_command<W: Write>(args: &[String], output: &mut W) -> Result<(), CliError> {
    let flags = DiagnosticsFlags::parse(args)?;
    let report = diagnostics::collect(flags.options()?)?;
    serde_json::to_writer(&mut *output, &report).map_err(CliError::Json)?;
    writeln!(output)?;
    Ok(())
}

fn provider_add<R, W>(args: &[String], input: &mut R, output: &mut W) -> Result<(), CliError>
where
    R: BufRead,
    W: Write,
{
    let flags = ProviderFlags::parse(args)?;
    if flags.interactive {
        return provider_add_interactive(flags, input, output);
    }

    let mut provider = flags.provider()?;
    let config_path = flags.config_path();

    if flags.add_all {
        let client =
            OpenAiCompatibleModelClient::new(flags.timeout.unwrap_or(Duration::from_secs(15)));
        let discovery = client.list_models(&provider)?;
        for model in discovery.add_all_model_ids() {
            provider = provider.with_model(model);
        }
    } else {
        for model in flags.models {
            provider = provider.with_model(model);
        }
    }

    if provider.models().is_empty() {
        // Preset flow: default to the first (bench-verified) model.
        let preset_model = BuiltinProvider::from_name(provider.name())
            .and_then(|builtin| builtin.profile().model_hints.first().copied());
        match preset_model {
            Some(model) => provider = provider.with_model(model),
            None => {
                return Err(CliError::Usage(
                    "provider add requires --model MODEL or --add-all".to_string(),
                ));
            }
        }
    }

    save_provider(provider, config_path, output)
}

fn provider_add_interactive<R, W>(
    mut flags: ProviderFlags,
    input: &mut R,
    output: &mut W,
) -> Result<(), CliError>
where
    R: BufRead,
    W: Write,
{
    if flags.name.as_deref().is_none_or(str::is_empty) {
        flags.name = Some(prompt_value(input, output, "Provider name: ")?);
    }
    if flags.url.as_deref().is_none_or(str::is_empty) {
        flags.url = Some(prompt_value(input, output, "Base URL: ")?);
    }
    if flags.key.as_deref().is_none_or(str::is_empty)
        && flags.key_env.as_deref().is_none_or(str::is_empty)
    {
        flags.key = Some(prompt_value(input, output, "API key: ")?);
    }

    let mut provider = flags.provider()?;
    let config_path = flags.config_path();
    let client = OpenAiCompatibleModelClient::new(flags.timeout.unwrap_or(Duration::from_secs(15)));
    let discovery = client.list_models(&provider)?;

    writeln!(output, "Available models:")?;
    for (index, choice) in discovery.choices().iter().enumerate() {
        match choice {
            ModelChoice::AddAll => writeln!(output, "0) Add all")?,
            ModelChoice::Model(model) => writeln!(output, "{index}) {model}")?,
        }
    }

    let selection = prompt_value(input, output, "Choose model number or id: ")?;
    for model in selected_models(&discovery, &selection)? {
        provider = provider.with_model(model);
    }

    save_provider(provider, config_path, output)
}

fn save_provider<W: Write>(
    provider: ProviderConfig,
    config_path: PathBuf,
    output: &mut W,
) -> Result<(), CliError> {
    let model_count = provider.models().len();
    let provider_name = provider.name().to_string();
    ConfigStore::new(config_path).save_provider(provider)?;
    writeln!(
        output,
        "saved provider {provider_name} with {model_count} models"
    )?;

    Ok(())
}

pub fn provider_config_from_tui_draft(draft: TuiProviderDraft) -> ProviderConfig {
    let mut provider = if let Some(builtin) = BuiltinProvider::from_name(&draft.name) {
        let profile = builtin.profile();
        ProviderConfig::new(profile.name, draft.base_url, draft.api_key)
            .with_auth_scheme(profile.auth_scheme)
            .with_cache_policy(profile.cache_policy)
            .with_chat_api(profile.chat_api)
            .with_key_env(profile.key_env)
    } else {
        ProviderConfig::new(draft.name, draft.base_url, draft.api_key)
    };

    provider = provider.with_model(draft.model);
    provider
}

fn selected_models(discovery: &ModelDiscovery, selection: &str) -> Result<Vec<String>, CliError> {
    let selection = selection.trim();
    if selection == "0"
        || selection.eq_ignore_ascii_case("all")
        || selection.eq_ignore_ascii_case("add all")
    {
        return Ok(discovery.add_all_model_ids());
    }

    if let Ok(index) = selection.parse::<usize>() {
        return match discovery.choices().get(index) {
            Some(ModelChoice::AddAll) => Ok(discovery.add_all_model_ids()),
            Some(ModelChoice::Model(model)) => Ok(vec![model.clone()]),
            None => Err(CliError::Usage(format!(
                "unknown model selection: {selection}"
            ))),
        };
    }

    discovery
        .choices()
        .iter()
        .find_map(|choice| match choice {
            ModelChoice::Model(model) if model == selection => Some(vec![model.clone()]),
            _ => None,
        })
        .ok_or_else(|| CliError::Usage(format!("unknown model selection: {selection}")))
}

fn prompt_value<R, W>(input: &mut R, output: &mut W, prompt: &str) -> Result<String, CliError>
where
    R: BufRead,
    W: Write,
{
    write!(output, "{prompt}")?;
    output.flush()?;

    let mut value = String::new();
    if input.read_line(&mut value)? == 0 {
        return Err(CliError::Usage(format!(
            "missing interactive input for {}",
            prompt.trim_end_matches(": ")
        )));
    }

    let value = value.trim_end_matches(&['\r', '\n'][..]).to_string();
    if value.trim().is_empty() {
        return Err(CliError::Usage(format!(
            "empty interactive input for {}",
            prompt.trim_end_matches(": ")
        )));
    }

    Ok(value)
}

#[derive(Debug, Default)]
struct ProviderFlags {
    config: Option<PathBuf>,
    name: Option<String>,
    url: Option<String>,
    key: Option<String>,
    key_env: Option<String>,
    auth: Option<String>,
    auth_header: Option<String>,
    chat_api: Option<ChatApiFormat>,
    cache: Option<String>,
    cache_header: Option<String>,
    cache_hit_field: Option<String>,
    cache_miss_field: Option<String>,
    cache_ttl: Option<String>,
    models: Vec<String>,
    add_all: bool,
    interactive: bool,
    timeout: Option<Duration>,
}

impl ProviderFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut flags = Self::default();
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--config" => {
                    flags.config = Some(PathBuf::from(required_value(args, index, "--config")?));
                    index += 2;
                }
                "--name" => {
                    flags.name = Some(required_value(args, index, "--name")?);
                    index += 2;
                }
                "--url" => {
                    flags.url = Some(required_value(args, index, "--url")?);
                    index += 2;
                }
                "--key" => {
                    flags.key = Some(required_value(args, index, "--key")?);
                    index += 2;
                }
                "--key-env" => {
                    flags.key_env = Some(required_value(args, index, "--key-env")?);
                    index += 2;
                }
                "--auth" => {
                    flags.auth = Some(required_value(args, index, "--auth")?);
                    index += 2;
                }
                "--auth-header" => {
                    flags.auth_header = Some(required_value(args, index, "--auth-header")?);
                    index += 2;
                }
                "--chat-api" => {
                    flags.chat_api = Some(parse_chat_api_format(required_value(
                        args,
                        index,
                        "--chat-api",
                    )?)?);
                    index += 2;
                }
                "--cache" => {
                    flags.cache = Some(required_value(args, index, "--cache")?);
                    index += 2;
                }
                "--cache-header" => {
                    flags.cache_header = Some(required_value(args, index, "--cache-header")?);
                    index += 2;
                }
                "--cache-hit-field" => {
                    flags.cache_hit_field = Some(required_value(args, index, "--cache-hit-field")?);
                    index += 2;
                }
                "--cache-miss-field" => {
                    flags.cache_miss_field =
                        Some(required_value(args, index, "--cache-miss-field")?);
                    index += 2;
                }
                "--cache-ttl" => {
                    flags.cache_ttl = Some(required_value(args, index, "--cache-ttl")?);
                    index += 2;
                }
                "--model" => {
                    flags.models.push(required_value(args, index, "--model")?);
                    index += 2;
                }
                "--add-all" => {
                    flags.add_all = true;
                    index += 1;
                }
                "--interactive" => {
                    flags.interactive = true;
                    index += 1;
                }
                "--timeout-ms" => {
                    flags.timeout = Some(parse_duration_ms_flag(
                        required_value(args, index, "--timeout-ms")?,
                        "--timeout-ms",
                    )?);
                    index += 2;
                }
                other => return Err(CliError::Usage(format!("unknown option: {other}"))),
            }
        }

        Ok(flags)
    }

    fn provider(&self) -> Result<ProviderConfig, CliError> {
        let name = required_flag(self.name.as_deref(), "--name")?;
        // Bench-verified presets carry their endpoint; everyone else must
        // state --url explicitly rather than trust an untested default.
        let preset_url = BuiltinProvider::from_name(&name)
            .and_then(|builtin| builtin.profile().base_url)
            .map(str::to_string);
        let url = match self.url.as_deref() {
            Some(url) => url.to_string(),
            None => preset_url.ok_or_else(|| CliError::Usage("missing --url".to_string()))?,
        };
        let key = self.key.clone().unwrap_or_default();
        let is_builtin = BuiltinProvider::from_name(&name).is_some();
        if key.is_empty() && self.key_env.as_deref().is_none_or(str::is_empty) && !is_builtin {
            return Err(CliError::Usage("missing --key or --key-env".to_string()));
        }

        let mut provider = if let Some(builtin) = BuiltinProvider::from_name(&name) {
            let profile = builtin.profile();
            ProviderConfig::new(profile.name, url, key)
                .with_auth_scheme(profile.auth_scheme)
                .with_cache_policy(profile.cache_policy)
                .with_chat_api(profile.chat_api)
                .with_key_env(profile.key_env)
        } else {
            ProviderConfig::new(name, url, key)
        };
        if let Some(key_env) = &self.key_env {
            provider = provider.with_key_env(key_env);
        }
        if let Some(auth_scheme) = self.auth_scheme()? {
            provider = provider.with_auth_scheme(auth_scheme);
        }
        if let Some(cache_policy) = self.cache_policy()? {
            provider = provider.with_cache_policy(cache_policy);
        }

        Ok(match self.chat_api {
            Some(chat_api) => provider.with_chat_api(chat_api),
            None => provider,
        })
    }

    fn auth_scheme(&self) -> Result<Option<AuthScheme>, CliError> {
        let Some(auth) = &self.auth else {
            return Ok(None);
        };

        let scheme = match auth.trim().to_ascii_lowercase().as_str() {
            "bearer" => AuthScheme::Bearer,
            "subscription" | "env" => AuthScheme::Subscription,
            "header" | "custom-header" => AuthScheme::Header {
                name: required_flag(self.auth_header.as_deref(), "--auth-header")?,
            },
            other => {
                return Err(CliError::Usage(format!(
                    "unknown --auth {other}; expected bearer, header, or subscription"
                )));
            }
        };

        Ok(Some(scheme))
    }

    fn cache_policy(&self) -> Result<Option<CachePolicy>, CliError> {
        let Some(cache) = &self.cache else {
            return Ok(None);
        };

        let policy = match cache.trim().to_ascii_lowercase().as_str() {
            "disabled" | "none" | "off" => CachePolicy::Disabled,
            "header" | "cache-header" => CachePolicy::Header {
                name: self
                    .cache_header
                    .clone()
                    .unwrap_or_else(|| "X-Harness-Cache-Key".to_string()),
            },
            "automatic" | "usage" | "metrics" => CachePolicy::Automatic {
                hit_tokens_field: required_flag(
                    self.cache_hit_field.as_deref(),
                    "--cache-hit-field",
                )?,
                miss_tokens_field: required_flag(
                    self.cache_miss_field.as_deref(),
                    "--cache-miss-field",
                )?,
            },
            "body-cache-control" | "body" | "cache-control" => CachePolicy::BodyCacheControl {
                ttl: self.cache_ttl.clone(),
            },
            "anthropic-automatic" | "anthropic" => CachePolicy::AnthropicAutomatic {
                ttl: self.cache_ttl.clone(),
            },
            other => {
                return Err(CliError::Usage(format!(
                    "unknown --cache {other}; expected disabled, header, automatic, body-cache-control, or anthropic-automatic"
                )));
            }
        };

        Ok(Some(policy))
    }

    fn config_path(&self) -> PathBuf {
        self.config.clone().unwrap_or_else(default_config_path)
    }
}

#[derive(Debug)]
struct ProviderListFlags {
    config_path: PathBuf,
}

impl ProviderListFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut config_path = None;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--config" => {
                    config_path = Some(PathBuf::from(required_value(args, index, "--config")?));
                    index += 2;
                }
                other => return Err(CliError::Usage(format!("unknown option: {other}"))),
            }
        }

        Ok(Self {
            config_path: config_path.unwrap_or_else(default_config_path),
        })
    }
}

fn required_value(args: &[String], index: usize, flag: &str) -> Result<String, CliError> {
    args.get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("missing value for {flag}")))
}

fn required_flag(value: Option<&str>, flag: &str) -> Result<String, CliError> {
    value
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| CliError::Usage(format!("missing {flag}")))
}

fn parse_chat_api_format(value: String) -> Result<ChatApiFormat, CliError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai-compatible" | "openai-chat" | "chat-completions" | "chat" => {
            Ok(ChatApiFormat::OpenAiCompatible)
        }
        "openai-responses" | "responses" | "response" => Ok(ChatApiFormat::OpenAiResponses),
        "openai-codex-responses" | "codex-responses" | "codex-response" => {
            Ok(ChatApiFormat::OpenAiCodexResponses)
        }
        "anthropic-messages" | "anthropic" | "messages" => Ok(ChatApiFormat::AnthropicMessages),
        other => Err(CliError::Usage(format!(
            "unknown --chat-api {other}; expected openai-compatible, openai-responses, openai-codex-responses, or anthropic-messages"
        ))),
    }
}

fn default_config_path() -> PathBuf {
    if let Ok(path) = env::var("HARNESS_CONFIG") {
        return PathBuf::from(path);
    }

    #[cfg(windows)]
    {
        if let Ok(appdata) = env::var("APPDATA") {
            return PathBuf::from(appdata)
                .join("harness-cli")
                .join("providers.json");
        }
    }

    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg)
            .join("harness-cli")
            .join("providers.json");
    }

    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("harness-cli")
            .join("providers.json");
    }

    PathBuf::from(".harness").join("providers.json")
}

#[derive(Debug)]
struct ToolCallFlags {
    workspace: PathBuf,
    tool_name: String,
    arguments: String,
}

impl ToolCallFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut workspace = env::current_dir().map_err(CliError::Io)?;
        let mut positionals = Vec::new();
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--workspace" => {
                    workspace = PathBuf::from(required_value(args, index, "--workspace")?);
                    index += 2;
                }
                other if other.starts_with("--") => {
                    return Err(CliError::Usage(format!("unknown option: {other}")));
                }
                _ => {
                    positionals.push(args[index].clone());
                    index += 1;
                }
            }
        }

        if positionals.len() != 2 {
            return Err(CliError::Usage(
                "tool call requires TOOL_NAME and JSON_ARGS".to_string(),
            ));
        }

        Ok(Self {
            workspace,
            tool_name: positionals.remove(0),
            arguments: positionals.remove(0),
        })
    }
}

#[derive(Debug)]
struct ToolBatchFlags {
    workspace: PathBuf,
    max_concurrency: usize,
    timeout: Option<Duration>,
    calls: String,
}

impl ToolBatchFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut workspace = env::current_dir().map_err(CliError::Io)?;
        let mut max_concurrency = None;
        let mut timeout = None;
        let mut positionals = Vec::new();
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--workspace" => {
                    workspace = PathBuf::from(required_value(args, index, "--workspace")?);
                    index += 2;
                }
                "--max-concurrency" => {
                    let value = required_value(args, index, "--max-concurrency")?;
                    let parsed = value.parse::<usize>().map_err(|_| {
                        CliError::Usage("--max-concurrency must be a positive integer".to_string())
                    })?;
                    if parsed == 0 {
                        return Err(CliError::Usage(
                            "--max-concurrency must be a positive integer".to_string(),
                        ));
                    }
                    max_concurrency = Some(parsed);
                    index += 2;
                }
                "--timeout-ms" => {
                    let value = required_value(args, index, "--timeout-ms")?;
                    let parsed = value.parse::<u64>().map_err(|_| {
                        CliError::Usage("--timeout-ms must be a positive integer".to_string())
                    })?;
                    if parsed == 0 {
                        return Err(CliError::Usage(
                            "--timeout-ms must be a positive integer".to_string(),
                        ));
                    }
                    timeout = Some(Duration::from_millis(parsed));
                    index += 2;
                }
                other if other.starts_with("--") => {
                    return Err(CliError::Usage(format!("unknown option: {other}")));
                }
                _ => {
                    positionals.push(args[index].clone());
                    index += 1;
                }
            }
        }

        if positionals.len() != 1 {
            return Err(CliError::Usage(
                "tool batch requires JSON_ARRAY_OF_TOOL_CALLS".to_string(),
            ));
        }

        Ok(Self {
            workspace,
            max_concurrency: max_concurrency.unwrap_or(4),
            timeout,
            calls: positionals.remove(0),
        })
    }
}

#[derive(Debug)]
struct ChatOnceFlags {
    config_path: PathBuf,
    provider_name: String,
    model: String,
    message: String,
    timeout: Duration,
}

impl ChatOnceFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut config_path = None;
        let mut provider_name = None;
        let mut model = None;
        let mut message = None;
        let mut timeout = None;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--config" => {
                    config_path = Some(PathBuf::from(required_value(args, index, "--config")?));
                    index += 2;
                }
                "--provider" => {
                    provider_name = Some(required_value(args, index, "--provider")?);
                    index += 2;
                }
                "--model" => {
                    model = Some(required_value(args, index, "--model")?);
                    index += 2;
                }
                "--message" => {
                    message = Some(required_value(args, index, "--message")?);
                    index += 2;
                }
                "--timeout-ms" => {
                    timeout = Some(parse_duration_ms_flag(
                        required_value(args, index, "--timeout-ms")?,
                        "--timeout-ms",
                    )?);
                    index += 2;
                }
                other => return Err(CliError::Usage(format!("unknown option: {other}"))),
            }
        }

        Ok(Self {
            config_path: config_path.unwrap_or_else(default_config_path),
            provider_name: required_flag(provider_name.as_deref(), "--provider")?,
            model: required_flag(model.as_deref(), "--model")?,
            message: required_flag(message.as_deref(), "--message")?,
            timeout: timeout.unwrap_or(Duration::from_secs(60)),
        })
    }
}

#[derive(Debug)]
struct AgentRunFlags {
    config_path: PathBuf,
    workspace: PathBuf,
    provider_name: String,
    model: String,
    message: String,
    timeout: Duration,
    max_rounds: usize,
    max_tool_concurrency: Option<usize>,
    tool_timeout: Option<Duration>,
    trace_path: Option<PathBuf>,
    tool_errors_path: Option<PathBuf>,
    stream: bool,
}

impl AgentRunFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut config_path = None;
        let mut workspace = None;
        let mut provider_name = None;
        let mut model = None;
        let mut message = None;
        let mut timeout = None;
        let mut max_rounds = None;
        let mut max_tool_concurrency = None;
        let mut tool_timeout = None;
        let mut trace_path = None;
        let mut tool_errors_path = None;
        let mut stream = false;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--config" => {
                    config_path = Some(PathBuf::from(required_value(args, index, "--config")?));
                    index += 2;
                }
                "--workspace" => {
                    workspace = Some(PathBuf::from(required_value(args, index, "--workspace")?));
                    index += 2;
                }
                "--provider" => {
                    provider_name = Some(required_value(args, index, "--provider")?);
                    index += 2;
                }
                "--model" => {
                    model = Some(required_value(args, index, "--model")?);
                    index += 2;
                }
                "--message" => {
                    message = Some(required_value(args, index, "--message")?);
                    index += 2;
                }
                "--timeout-ms" => {
                    timeout = Some(parse_duration_ms_flag(
                        required_value(args, index, "--timeout-ms")?,
                        "--timeout-ms",
                    )?);
                    index += 2;
                }
                "--max-rounds" => {
                    max_rounds = Some(parse_usize_flag(
                        required_value(args, index, "--max-rounds")?,
                        "--max-rounds",
                    )?);
                    index += 2;
                }
                "--max-tool-concurrency" => {
                    max_tool_concurrency = Some(parse_usize_flag(
                        required_value(args, index, "--max-tool-concurrency")?,
                        "--max-tool-concurrency",
                    )?);
                    index += 2;
                }
                "--tool-timeout-ms" => {
                    tool_timeout = Some(parse_duration_ms_flag(
                        required_value(args, index, "--tool-timeout-ms")?,
                        "--tool-timeout-ms",
                    )?);
                    index += 2;
                }
                "--trace" => {
                    trace_path = Some(PathBuf::from(required_value(args, index, "--trace")?));
                    index += 2;
                }
                "--tool-errors" => {
                    tool_errors_path =
                        Some(PathBuf::from(required_value(args, index, "--tool-errors")?));
                    index += 2;
                }
                "--stream" => {
                    stream = true;
                    index += 1;
                }
                other => return Err(CliError::Usage(format!("unknown option: {other}"))),
            }
        }

        Ok(Self {
            config_path: config_path.unwrap_or_else(default_config_path),
            workspace: workspace
                .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
            provider_name: required_flag(provider_name.as_deref(), "--provider")?,
            model: required_flag(model.as_deref(), "--model")?,
            message: required_flag(message.as_deref(), "--message")?,
            timeout: timeout.unwrap_or(Duration::from_secs(60)),
            max_rounds: max_rounds.unwrap_or(4),
            max_tool_concurrency,
            tool_timeout,
            trace_path,
            tool_errors_path,
            stream,
        })
    }
}

#[derive(Debug)]
struct ClipboardPasteFlags {
    workspace: PathBuf,
    text: Option<String>,
}

impl ClipboardPasteFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut workspace = None;
        let mut text = None;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--workspace" => {
                    workspace = Some(PathBuf::from(required_value(args, index, "--workspace")?));
                    index += 2;
                }
                "--text" => {
                    text = Some(required_value(args, index, "--text")?);
                    index += 2;
                }
                other => return Err(CliError::Usage(format!("unknown option: {other}"))),
            }
        }

        Ok(Self {
            workspace: workspace
                .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
            text,
        })
    }
}

#[derive(Debug)]
struct ReplFlags {
    config_path: PathBuf,
    workspace: PathBuf,
    provider_name: Option<String>,
    model: Option<String>,
    timeout: Duration,
    max_rounds: usize,
    max_tool_concurrency: Option<usize>,
    tool_timeout: Option<Duration>,
}

impl ReplFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut config_path = None;
        let mut workspace = None;
        let mut provider_name = None;
        let mut model = None;
        let mut timeout = None;
        let mut max_rounds = None;
        let mut max_tool_concurrency = None;
        let mut tool_timeout = None;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--config" => {
                    config_path = Some(PathBuf::from(required_value(args, index, "--config")?));
                    index += 2;
                }
                "--workspace" => {
                    workspace = Some(PathBuf::from(required_value(args, index, "--workspace")?));
                    index += 2;
                }
                "--provider" => {
                    provider_name = Some(required_value(args, index, "--provider")?);
                    index += 2;
                }
                "--model" => {
                    model = Some(required_value(args, index, "--model")?);
                    index += 2;
                }
                "--timeout-ms" => {
                    timeout = Some(parse_duration_ms_flag(
                        required_value(args, index, "--timeout-ms")?,
                        "--timeout-ms",
                    )?);
                    index += 2;
                }
                "--max-rounds" => {
                    max_rounds = Some(parse_usize_flag(
                        required_value(args, index, "--max-rounds")?,
                        "--max-rounds",
                    )?);
                    index += 2;
                }
                "--max-tool-concurrency" => {
                    max_tool_concurrency = Some(parse_usize_flag(
                        required_value(args, index, "--max-tool-concurrency")?,
                        "--max-tool-concurrency",
                    )?);
                    index += 2;
                }
                "--tool-timeout-ms" => {
                    tool_timeout = Some(parse_duration_ms_flag(
                        required_value(args, index, "--tool-timeout-ms")?,
                        "--tool-timeout-ms",
                    )?);
                    index += 2;
                }
                other => return Err(CliError::Usage(format!("unknown option: {other}"))),
            }
        }

        Ok(Self {
            config_path: config_path.unwrap_or_else(default_config_path),
            workspace: workspace
                .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
            provider_name,
            model,
            timeout: timeout.unwrap_or(Duration::from_secs(60)),
            // Interactive chat is not round-capped by default: an exploring
            // agent legitimately needs many tool rounds, and a hard error mid-
            // conversation loses the turn. `--max-rounds N` still applies a cap.
            max_rounds: max_rounds.unwrap_or(usize::MAX),
            max_tool_concurrency,
            tool_timeout,
        })
    }
}

#[derive(Debug)]
struct DiagnosticsFlags {
    binary_path: Option<PathBuf>,
    max_binary_bytes: Option<u64>,
    max_rss_bytes: Option<u64>,
}

impl DiagnosticsFlags {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut binary_path = None;
        let mut max_binary_bytes = None;
        let mut max_rss_bytes = None;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--binary" => {
                    binary_path = Some(PathBuf::from(required_value(args, index, "--binary")?));
                    index += 2;
                }
                "--max-binary-bytes" => {
                    max_binary_bytes = Some(parse_u64_flag(
                        required_value(args, index, "--max-binary-bytes")?,
                        "--max-binary-bytes",
                    )?);
                    index += 2;
                }
                "--max-rss-bytes" => {
                    max_rss_bytes = Some(parse_u64_flag(
                        required_value(args, index, "--max-rss-bytes")?,
                        "--max-rss-bytes",
                    )?);
                    index += 2;
                }
                other => return Err(CliError::Usage(format!("unknown option: {other}"))),
            }
        }

        Ok(Self {
            binary_path,
            max_binary_bytes,
            max_rss_bytes,
        })
    }

    fn options(self) -> Result<DiagnosticsOptions, CliError> {
        Ok(DiagnosticsOptions {
            binary_path: self
                .binary_path
                .map(Ok)
                .unwrap_or_else(env::current_exe)
                .map_err(CliError::Io)?,
            max_binary_bytes: self.max_binary_bytes,
            max_rss_bytes: self.max_rss_bytes,
        })
    }
}

fn parse_u64_flag(value: String, flag: &str) -> Result<u64, CliError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| CliError::Usage(format!("{flag} must be a positive integer")))?;
    if parsed == 0 {
        return Err(CliError::Usage(format!(
            "{flag} must be a positive integer"
        )));
    }
    Ok(parsed)
}

fn parse_usize_flag(value: String, flag: &str) -> Result<usize, CliError> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| CliError::Usage(format!("{flag} must be a positive integer")))?;
    if parsed == 0 {
        return Err(CliError::Usage(format!(
            "{flag} must be a positive integer"
        )));
    }
    Ok(parsed)
}

fn parse_duration_ms_flag(value: String, flag: &str) -> Result<Duration, CliError> {
    Ok(Duration::from_millis(parse_u64_flag(value, flag)?))
}

fn help(command_name: &str) -> String {
    [
        "usage:".to_string(),
        format!("  {command_name}"),
        format!("  {command_name} provider subscriptions"),
        format!("  {command_name} provider list [--config PATH]"),
        format!(
            "  {command_name} provider models --name NAME --url URL (--key KEY | --key-env ENV) [--auth SCHEME] [--timeout-ms N]"
        ),
        format!("  {command_name} provider add --interactive --config PATH [--timeout-ms N]"),
        format!(
            "  {command_name} provider add --config PATH --name NAME --url URL (--key KEY | --key-env ENV) [--auth SCHEME] [--chat-api FORMAT] [--cache POLICY] [--timeout-ms N] (--model MODEL | --add-all)"
        ),
        format!(
            "  {command_name} chat once --config PATH --provider NAME --model MODEL --message TEXT [--timeout-ms N]"
        ),
        format!(
            "  {command_name} chat stream --config PATH --provider NAME --model MODEL --message TEXT [--timeout-ms N]"
        ),
        format!(
            "  {command_name} agent run --config PATH --workspace PATH --provider NAME --model MODEL --message TEXT [--timeout-ms N] [--max-rounds N] [--max-tool-concurrency N] [--tool-timeout-ms N] [--trace PATH] [--tool-errors PATH] [--stream]"
        ),
        format!(
            "  {command_name} repl --config PATH --workspace PATH --provider NAME --model MODEL [--timeout-ms N] [--max-rounds N] [--max-tool-concurrency N] [--tool-timeout-ms N]"
        ),
        format!("  {command_name} diagnostics [--binary PATH] [--max-binary-bytes N] [--max-rss-bytes N]"),
        format!("  {command_name} clipboard paste --workspace PATH"),
        format!("  {command_name} tool call --workspace PATH TOOL_NAME JSON_ARGS"),
        format!(
            "  {command_name} tool batch --workspace PATH [--max-concurrency N] [--timeout-ms N] JSON_ARRAY_OF_TOOL_CALLS"
        ),
    ]
    .join("\n")
}

fn command_name(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(program)
        .to_string()
}

fn cache_policy_label(cache_policy: &CachePolicy) -> String {
    match cache_policy {
        CachePolicy::Disabled => "cache=disabled".to_string(),
        CachePolicy::Header { name } => format!("cache-header={name}"),
        CachePolicy::Automatic {
            hit_tokens_field,
            miss_tokens_field,
        } => format!("cache=automatic({hit_tokens_field},{miss_tokens_field})"),
        CachePolicy::AnthropicAutomatic { ttl } => match ttl {
            Some(ttl) => format!("cache=anthropic-automatic(ttl={ttl})"),
            None => "cache=anthropic-automatic".to_string(),
        },
        CachePolicy::BodyCacheControl { ttl } => match ttl {
            Some(ttl) => format!("cache=body-cache-control(ttl={ttl})"),
            None => "cache=body-cache-control".to_string(),
        },
    }
}

fn auth_scheme_label(auth_scheme: &AuthScheme) -> String {
    match auth_scheme {
        AuthScheme::Bearer => "bearer".to_string(),
        AuthScheme::Subscription => "subscription".to_string(),
        AuthScheme::Header { name } => format!("header:{name}"),
    }
}

fn chat_api_label(chat_api: ChatApiFormat) -> &'static str {
    match chat_api {
        ChatApiFormat::OpenAiCompatible => "openai-compatible",
        ChatApiFormat::OpenAiResponses => "openai-responses",
        ChatApiFormat::OpenAiCodexResponses => "openai-codex-responses",
        ChatApiFormat::AnthropicMessages => "anthropic-messages",
    }
}

fn key_source_label(provider: &ProviderConfig) -> String {
    if !provider.api_key().trim().is_empty() {
        return "inline".to_string();
    }

    provider
        .key_env()
        .map(|key_env| format!("env:{key_env}"))
        .unwrap_or_else(|| "missing".to_string())
}

#[derive(Debug)]
pub enum CliError {
    Usage(String),
    Config(ConfigError),
    Model(ModelClientError),
    Chat(ChatClientError),
    Agent(AgentError),
    Clipboard(ClipboardError),
    Repl(ReplError),
    Runtime(RuntimeError),
    Tui(TuiError),
    Json(serde_json::Error),
    Io(io::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(msg) => write!(f, "{msg}"),
            Self::Config(err) => write!(f, "{err}"),
            Self::Model(err) => write!(f, "{err}"),
            Self::Chat(err) => write!(f, "{err}"),
            Self::Agent(err) => write!(f, "{err}"),
            Self::Clipboard(err) => write!(f, "{err}"),
            Self::Repl(err) => write!(f, "{err}"),
            Self::Runtime(err) => write!(f, "{err}"),
            Self::Tui(err) => write!(f, "{err}"),
            Self::Json(err) => write!(f, "invalid json: {err}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Usage(_) => None,
            Self::Config(err) => Some(err),
            Self::Model(err) => Some(err),
            Self::Chat(err) => Some(err),
            Self::Agent(err) => Some(err),
            Self::Clipboard(err) => Some(err),
            Self::Repl(err) => Some(err),
            Self::Runtime(err) => Some(err),
            Self::Tui(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

impl From<ConfigError> for CliError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<ModelClientError> for CliError {
    fn from(value: ModelClientError) -> Self {
        Self::Model(value)
    }
}

impl From<ChatClientError> for CliError {
    fn from(value: ChatClientError) -> Self {
        Self::Chat(value)
    }
}

impl From<AgentError> for CliError {
    fn from(value: AgentError) -> Self {
        Self::Agent(value)
    }
}

impl From<ClipboardError> for CliError {
    fn from(value: ClipboardError) -> Self {
        Self::Clipboard(value)
    }
}

impl From<ReplError> for CliError {
    fn from(value: ReplError) -> Self {
        Self::Repl(value)
    }
}

impl From<RuntimeError> for CliError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<TuiError> for CliError {
    fn from(value: TuiError) -> Self {
        Self::Tui(value)
    }
}

impl From<io::Error> for CliError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
