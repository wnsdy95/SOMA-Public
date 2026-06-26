//! SOMA CLI binary entrypoint. Parses the top-level subcommand and routes to
//! `crate::cli::*` handlers. Public commands expose the local context-layer
//! path; hidden commands keep legacy migration/debug surfaces out of discovery.

use clap::{CommandFactory, Parser};
use soma::cli::{Cli, Cmd, ColorMode, PersonaMode};
use tracing_appender::non_blocking::WorkerGuard;

/// Wrap an error with a fixed `op` tag in a single-line JSON
/// envelope so stop-hook / launchd log parsers can pick the
/// failure leg without `Display` collisions on quotes / newlines.
///
/// D89 §P2 — used by *every* subcommand. Pre-D89 several paths
/// hand-built JSON via `format!("{{\"error\":\"{e}\"}}")` which
/// produced invalid JSON the moment the error contained a quote
/// or newline.
fn error_json(op: &str, e: &dyn std::fmt::Display) -> String {
    let payload = serde_json::json!({ "op": op, "error": e.to_string() });
    serde_json::to_string(&payload).unwrap_or_else(|_| format!(r#"{{"op":"{op}"}}"#))
}

/// Build the command tree used for shell completions.
///
/// `clap_complete` 4.6 walks hidden subcommands, so feeding it the raw
/// `Cli::command()` would re-advertise legacy migration/debug verbs such as
/// `persona`. Completion is a discovery surface; keep it aligned
/// with the public context-layer CLI by cloning only visible subcommands.
fn completion_command() -> clap::Command {
    let source = Cli::command();
    let mut cmd = clap::Command::new("soma")
        .version(env!("CARGO_PKG_VERSION"))
        .about("SOMA — local context layer for cloud LLMs");

    for arg in source.get_arguments() {
        cmd = cmd.arg(arg.clone());
    }
    for subcommand in source.get_subcommands().filter(|subcommand| !subcommand.is_hide_set()) {
        cmd = cmd.subcommand(subcommand.clone());
    }

    cmd
}

/// D85-A (shell-init half) — inject the soma ingest hook into the
/// user's bashrc / zshrc / fish config. Each rc file is treated
/// independently — missing files are auto-created so a user who
/// hasn't touched their shell config still gets the hook.
#[cfg(unix)]
fn inject_shell_init() -> std::io::Result<()> {
    use soma::cli::shell_init::{default_rc_paths, inject_block};
    let home = dirs::home_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not resolvable")
    })?;
    let binary = std::env::current_exe()?;
    for rc in default_rc_paths(&home) {
        let changed = inject_block(&rc, &binary)?;
        let action = if changed { "wrote" } else { "kept" };
        println!("soma: shell-init {action} {}", rc.display());
    }
    println!("soma: re-source your shell rc (e.g. `source ~/.zshrc`) to activate the hook");
    Ok(())
}

#[cfg(unix)]
fn remove_shell_init() -> std::io::Result<()> {
    use soma::cli::shell_init::{default_rc_paths, remove_block};
    let home = dirs::home_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not resolvable")
    })?;
    for rc in default_rc_paths(&home) {
        let removed = remove_block(&rc)?;
        let action = if removed { "removed from" } else { "absent in" };
        println!("soma: shell-init {action} {}", rc.display());
    }
    Ok(())
}

/// `soma install --model <id>` backend for ONNX embedding models
/// used by ContextEnvelope evidence ranking. Returns
/// `Err(exit_code)` on failure. Recognized model ids:
///   * Embedder (requires `embed-onnx` feature):
///     - `paraphrase-multilingual-MiniLM-L12-v2` / `minilm-l12-v2-384d` (Mini)
///     - `multilingual-e5-large` / `multilingual-e5-large-1024d` (Studio, D69)
fn install_model(model_id: &str) -> Result<(), i32> {
    const MINI_NAMES: &[&str] =
        &["paraphrase-multilingual-MiniLM-L12-v2", soma::memory::embed::OnnxEmbedder::MODEL_ID];
    const STUDIO_NAMES: &[&str] =
        &["multilingual-e5-large", soma::memory::embed::E5LargeEmbedder::MODEL_ID];

    let is_mini = MINI_NAMES.contains(&model_id);
    let is_studio = STUDIO_NAMES.contains(&model_id);
    if !is_mini && !is_studio {
        let supported = [
            format!("{} (Mini)", MINI_NAMES.join(", ")),
            format!("{} (Studio)", STUDIO_NAMES.join(", ")),
        ];
        eprintln!(
            "{}",
            error_json(
                "install.model",
                &format!(
                    "unknown ONNX embedding model id `{model_id}` for ContextEnvelope evidence ranking. supported: {}",
                    supported.join(", ")
                ),
            )
        );
        return Err(2);
    }

    #[cfg(feature = "embed-onnx")]
    {
        let result: Result<std::path::PathBuf, String> = if is_studio {
            soma::memory::embed::E5LargeEmbedder::ensure_downloaded().map_err(|e| e.to_string())
        } else {
            soma::memory::embed::OnnxEmbedder::ensure_downloaded().map_err(|e| e.to_string())
        };
        match result {
            Ok(cache) => {
                println!("soma: model `{model_id}` ready at {}", cache.display());
                Ok(())
            }
            Err(e) => {
                eprintln!("{}", error_json("install.model", &e));
                Err(2)
            }
        }
    }
    #[cfg(not(feature = "embed-onnx"))]
    {
        let _ = model_id;
        let _ = (is_mini, is_studio);
        eprintln!(
            "{}",
            error_json(
                "install.model",
                &"`embed-onnx` feature is disabled; rebuild with \
                 `cargo install --path crates/soma --features embed-onnx` \
                 to enable ONNX embedding models for ContextEnvelope evidence ranking"
                    .to_string(),
            )
        );
        Err(2)
    }
}

/// Map a verbosity score to the default `EnvFilter` directive.
///   * `< 0` → `warn` (operator passed `-q`)
///   * `0`   → `info` (default)
///   * `1`   → `debug` (`-v`)
///   * `≥ 2` → `trace` (`-vv` or more)
fn verbosity_to_filter(v: i8) -> &'static str {
    match v {
        i8::MIN..=-1 => "warn",
        0 => "info",
        1 => "debug",
        _ => "trace",
    }
}

/// Compute the effective verbosity score from the CLI flags.
/// `-q` subtracts 1, `-v`/`-vv`/`-vvv` add 1 / 2 / 3.
fn compute_verbosity(verbose: u8, quiet: bool) -> i8 {
    let v = verbose as i8;
    if quiet {
        v - 1
    } else {
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceFallback {
    Stderr,
    Silent,
}

/// D128 close — initialize the global tracing subscriber with a
/// rolling daily file appender at `~/.soma/log/soma.log`. The
/// LaunchAgent's `StandardErrorPath` still catches early-boot output
/// (anything before this function returns), but once it succeeds the
/// rolling appender takes over and the launchctl-managed err.log
/// stays bounded.
///
/// Behaviour matrix:
///
///   * Home dir resolves AND log dir creates clean → rolling daily
///     file appender. Filename pattern `soma.log.YYYY-MM-DD`.
///   * Home dir unresolvable, OR `create_dir_all` fails → fall back
///     to stderr writer so the resident still emits *something*.
///     Failure is reported via stderr (we cannot use `tracing::warn!`
///     yet — the subscriber is exactly what we are setting up).
///
/// `RUST_LOG` env always overrides the verbosity-derived directive
/// (matches the convention `cli/start.rs` already documents).
/// Idempotent — `try_init` swallows the "already-set" error so unit
/// tests that install their own subscriber do not panic.
///
/// Returns a `WorkerGuard` that the caller (`main`) MUST keep alive
/// for the entire process: the non-blocking writer's flush thread
/// stops the moment the guard drops, and any buffered log lines are
/// lost. Holding it on the stack frame of `main` (rather than in a
/// `static`) ensures `Drop::drop` actually runs at program exit
/// (Rust does not run static destructors on normal exit).
///
/// `None` is returned on the stderr-fallback path — there is no
/// background thread to keep alive in that case.
#[must_use = "the WorkerGuard must outlive main; dropping it cuts off the flush thread"]
fn init_tracing(verbosity: i8, fallback: TraceFallback) -> Option<WorkerGuard> {
    use tracing_subscriber::{fmt, EnvFilter};

    let base_directive = verbosity_to_filter(verbosity);
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(base_directive));

    // Try to set up the rolling file appender. On any failure we
    // fall back to stderr so the binary never silently loses logs.
    // Reuse `cli::logs::resolve_log_dir` so the writer + the reader
    // (`soma logs tail`) agree on the location bit-for-bit, including
    // the `SOMA_LOG_DIR` env override.
    let log_dir_result =
        soma::cli::logs::resolve_log_dir(None).map_err(|e| e.to_string()).and_then(|dir| {
            ensure_log_dir(&dir)?;
            Ok(dir)
        });

    match log_dir_result {
        Ok(log_dir) => {
            let file_appender = tracing_appender::rolling::daily(&log_dir, "soma.log");
            let (writer, guard) = tracing_appender::non_blocking(file_appender);
            let _ = fmt()
                .with_env_filter(env_filter)
                .with_writer(writer)
                .with_ansi(false) // file logs never need ANSI codes
                .with_target(true)
                .try_init();
            Some(guard)
        }
        Err(reason) => {
            match fallback {
                TraceFallback::Stderr => {
                    // Best-effort surface: stderr line so the operator can
                    // see why the file appender did not engage. Falls back
                    // to a stderr-writer subscriber so the resident still
                    // streams events somewhere.
                    eprintln!("soma: rolling log unavailable ({reason}); falling back to stderr");
                    let _ = fmt()
                        .with_env_filter(env_filter)
                        .with_writer(std::io::stderr)
                        .with_target(true)
                        .try_init();
                }
                TraceFallback::Silent => {
                    // Machine-readable client surfaces must keep JSON/stdout
                    // clean even in sandboxed apps where ~/.soma/log cannot be
                    // chmodded. Install a sink subscriber instead of emitting a
                    // fallback warning or stderr log stream.
                    let _ = fmt()
                        .with_env_filter(env_filter)
                        .with_writer(std::io::sink)
                        .with_target(true)
                        .try_init();
                }
            }
            None
        }
    }
}

fn trace_fallback_for_cli(cli: &Cli, verbosity: i8) -> TraceFallback {
    if verbosity > 0 || std::env::var_os("SOMA_LOG_FALLBACK_STDERR").is_some() {
        return TraceFallback::Stderr;
    }
    match &cli.cmd {
        Some(Cmd::List(_) | Cmd::Create(_) | Cmd::Call(_) | Cmd::Projects(_) | Cmd::Session(_)) => {
            TraceFallback::Silent
        }
        Some(Cmd::Persona(args))
            if matches!(
                &args.mode,
                PersonaMode::List(_) | PersonaMode::Create(_) | PersonaMode::Call(_)
            ) =>
        {
            TraceFallback::Silent
        }
        Some(Cmd::Clients(args)) if args.wants_json_output() || args.wants_brief_output() => {
            TraceFallback::Silent
        }
        Some(Cmd::Learning(args)) if args.wants_json_output() || args.wants_brief_output() => {
            TraceFallback::Silent
        }
        Some(Cmd::McpConfig(_)) | Some(Cmd::McpServe) => TraceFallback::Silent,
        Some(Cmd::AdapterCapture(_))
        | Some(Cmd::AdapterCloudOutput(_))
        | Some(Cmd::AdapterLifecycle(_))
        | Some(Cmd::AdapterSpool(_))
        | Some(Cmd::AdapterSpoolAppend(_))
        | Some(Cmd::AdapterBindingProof(_))
        | Some(Cmd::Config)
        | Some(Cmd::Diagnose(_)) => TraceFallback::Silent,
        _ => TraceFallback::Stderr,
    }
}

#[cfg(unix)]
fn ensure_log_dir(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("create_dir_all `{}`: {e}", path.display()))?;
    // Re-chmod to guarantee 0o700 even if an ancestor pre-existed
    // with looser perms (matches the runtime resident pattern).
    let mode = std::fs::metadata(path)
        .map_err(|e| format!("metadata `{}`: {e}", path.display()))?
        .permissions()
        .mode();
    if mode.trailing_zeros() >= 6 {
        return ensure_log_dir_writable(path);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("set_permissions `{}`: {e}", path.display()))?;
    ensure_log_dir_writable(path)
}

#[cfg(not(unix))]
fn ensure_log_dir(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("create_dir_all `{}`: {e}", path.display()))?;
    ensure_log_dir_writable(path)
}

fn ensure_log_dir_writable(path: &std::path::Path) -> Result<(), String> {
    let probe = path.join(format!(".soma-log-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(err) => Err(format!("create log probe `{}`: {err}", probe.display())),
    }
}

/// R7 audit (2026-04-30) — emit a final tracing line on panic before
/// `panic = "abort"` (release profile, D118-cand) terminates the
/// process. Production debugging without this loses the panic
/// location to whatever default panic handler stderr prints.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".to_string());
        tracing::error!(
            thread = thread.name().unwrap_or("<unnamed>"),
            location = %info.location().map(|l| l.to_string()).unwrap_or_default(),
            payload = %payload,
            "soma panic — process will abort (release profile)"
        );
        prev(info);
    }));
}

fn main() -> anyhow::Result<()> {
    // Parse first so global flags (`--color`, `-v/-q`) feed
    // `init_tracing`. Pre-D128 the order was reversed (init then
    // parse) which made `-v` etc. impossible — the subscriber was
    // already locked at default before we knew what level the user
    // asked for.
    let cli = Cli::parse();
    let verbosity = compute_verbosity(cli.verbose, cli.quiet);
    // Keep the WorkerGuard on the stack frame of `main` so it drops
    // (and flushes the rolling-appender's pending lines) when this
    // function returns. `static` storage does not run destructors
    // on normal Rust process exit, so parking it there would silently
    // truncate the log tail.
    let _log_guard = init_tracing(verbosity, trace_fallback_for_cli(&cli, verbosity));
    install_panic_hook();

    // First post-init line so the rolling log file is non-empty even
    // for short-lived CLI verbs that otherwise emit no tracing of
    // their own. Helps `soma logs tail` surface "the resident
    // started" / "the cli ran" without the operator having to wait
    // for the first warn-level event.
    tracing::info!(version = env!("CARGO_PKG_VERSION"), verbosity, "soma cli init",);

    // D134 close — `--color never` is propagated via `NO_COLOR` env
    // so any downstream colored output (clap's own help renderer,
    // future markdown highlighters) respects the policy. `auto`
    // already honours `NO_COLOR` by convention, so we only need to
    // *set* `NO_COLOR` here for the explicit-never case. `always`
    // intentionally does not unset `NO_COLOR` — the operator who
    // pre-set it wins.
    if matches!(cli.color, ColorMode::Never) {
        // Single-threaded section before any tokio runtime is spun
        // up. `set_var` mutating the process env here is safe
        // because no other thread can read it concurrently. (Rust
        // 2024 edition will require `unsafe { }` here; 2021 — our
        // pinned edition — does not, so we keep the call plain.)
        std::env::set_var("NO_COLOR", "1");
    }

    match cli.cmd.unwrap_or_else(|| Cmd::Status(Default::default())) {
        Cmd::List(args) => match soma::cli::persona_registry::run_list(&args) {
            Ok(rendered) => print!("{rendered}"),
            Err(e) => {
                eprintln!("{}", error_json("list", &e));
                std::process::exit(e.exit_code());
            }
        },
        Cmd::Create(args) => match soma::cli::persona_registry::run_create(&args) {
            Ok(rendered) => print!("{rendered}"),
            Err(e) => {
                eprintln!("{}", error_json("create", &e));
                std::process::exit(e.exit_code());
            }
        },
        Cmd::Call(args) => match soma::cli::persona_registry::run_call(&args) {
            Ok(rendered) => print!("{rendered}"),
            Err(e) => {
                eprintln!("{}", error_json("call", &e));
                std::process::exit(e.exit_code());
            }
        },
        Cmd::Start => {
            #[cfg(unix)]
            {
                use soma::cli::start::{exit_code_for, run_blocking, StartError};
                if let Err(e) = run_blocking() {
                    eprintln!("{}", error_json("start", &e));
                    let code = match &e {
                        e @ (StartError::Path(_)
                        | StartError::Storage(_)
                        | StartError::Resident(_)
                        | StartError::Runtime(_)) => exit_code_for(e),
                    };
                    std::process::exit(code);
                }
            }
            #[cfg(not(unix))]
            {
                eprintln!("soma start: unix only (POSIX socket + signal handlers)");
                std::process::exit(2);
            }
        }
        Cmd::Stop => {
            #[cfg(unix)]
            {
                use soma::cli::stop::{exit_code_for, run_blocking, StopError};
                match run_blocking() {
                    Ok(()) => println!("soma: resident stopped"),
                    Err(StopError::NotRunning) => {
                        println!("soma: resident not running");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("stop", &e));
                        std::process::exit(exit_code_for(&e));
                    }
                }
            }
            #[cfg(not(unix))]
            {
                eprintln!("soma stop: unix only");
                std::process::exit(2);
            }
        }
        Cmd::Status(args) => {
            #[cfg(unix)]
            {
                use soma::cli::status::{exit_code_for, run_blocking};
                if let Err(e) = run_blocking(&args) {
                    eprintln!("{}", error_json("status", &e));
                    std::process::exit(exit_code_for(&e));
                }
            }
            #[cfg(not(unix))]
            {
                let profile = soma::profile::detect();
                if args.wants_json_output() {
                    println!(
                        "{}",
                        serde_json::json!({
                            "schema": "soma.status.v1",
                            "source": "soma_status",
                            "state": "not_supported",
                            "resident": {
                                "running": false,
                                "status": "non_unix_scaffold"
                            },
                            "profile_detected": format!("{profile:?}"),
                            "trust_boundary": "soma_status_is_read_only: reports resident runtime state only; records no proof row, creates no verification event, installs no hook, and promotes no cloud draft"
                        })
                    );
                } else {
                    println!("soma: status (non-unix scaffold)");
                    println!("  profile (detected): {profile:?}");
                }
            }
        }
        Cmd::Session(args) => match soma::cli::session::run(&args) {
            Ok(rendered) => print!("{rendered}"),
            Err(e) => {
                eprintln!("{}", error_json("session", &e));
                std::process::exit(e.exit_code());
            }
        },
        Cmd::Install(args) => {
            use soma::cli::install::{default_install_config, install};
            #[cfg(target_os = "macos")]
            let ctl: Box<dyn soma::cli::install::LaunchCtl> =
                Box::new(soma::cli::install::platform::SystemLaunchCtl);
            #[cfg(not(target_os = "macos"))]
            let ctl: Box<dyn soma::cli::install::LaunchCtl> =
                Box::new(soma::cli::install::NoopLaunchCtl);

            if !args.no_launch_agent {
                match default_install_config().and_then(|cfg| install(&cfg, ctl.as_ref())) {
                    Ok(plist) => {
                        println!("soma: installed LaunchAgent at {}", plist.display());
                        #[cfg(not(target_os = "macos"))]
                        println!("  (non-macOS host: plist written but launchctl skipped)");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("install", &e));
                        std::process::exit(2);
                    }
                }
            }
            #[cfg(unix)]
            if args.shell_init {
                if let Err(e) = inject_shell_init() {
                    eprintln!("{}", error_json("install.shell_init", &e));
                    std::process::exit(2);
                }
            }
            if let Some(model_id) = args.model.as_deref() {
                if let Err(code) = install_model(model_id) {
                    std::process::exit(code);
                }
            }
        }
        Cmd::Uninstall(args) => {
            use soma::cli::install::{default_install_config, uninstall};
            #[cfg(target_os = "macos")]
            let ctl: Box<dyn soma::cli::install::LaunchCtl> =
                Box::new(soma::cli::install::platform::SystemLaunchCtl);
            #[cfg(not(target_os = "macos"))]
            let ctl: Box<dyn soma::cli::install::LaunchCtl> =
                Box::new(soma::cli::install::NoopLaunchCtl);

            if !args.no_launch_agent {
                match default_install_config().and_then(|cfg| uninstall(&cfg, ctl.as_ref())) {
                    Ok(()) => println!("soma: uninstalled LaunchAgent"),
                    Err(e) => {
                        eprintln!("{}", error_json("uninstall", &e));
                        std::process::exit(2);
                    }
                }
            }
            #[cfg(unix)]
            if args.shell_init {
                if let Err(e) = remove_shell_init() {
                    eprintln!("{}", error_json("uninstall.shell_init", &e));
                    std::process::exit(2);
                }
            }
        }
        Cmd::Ingest(args) => {
            use soma::capture::ai_cli::{
                emit_error_json, exit_code_for, resolve_db_path, run_ingest, IngestContext,
            };
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    emit_error_json(&e);
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = IngestContext { db_path };
            match run_ingest(&args, &ctx) {
                Ok(soma::capture::ai_cli::IngestOutcome::Stored { episode_id }) => {
                    println!("{episode_id}");
                }
                Err(e) => {
                    emit_error_json(&e);
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        Cmd::AdapterCapture(args) => {
            use soma::capture::ai_cli::{emit_error_json, exit_code_for, resolve_db_path};
            use soma::cli::adapter_capture::{run_blocking, AdapterCaptureContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    emit_error_json(&e);
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = AdapterCaptureContext { db_path };
            match run_blocking(&args, &ctx) {
                Ok(soma::capture::ai_cli::IngestOutcome::Stored { episode_id }) => {
                    println!("{episode_id}");
                }
                Err(e) => {
                    emit_error_json(&e);
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        Cmd::AdapterCloudOutput(args) => {
            use soma::capture::ai_cli::{emit_error_json, exit_code_for, resolve_db_path};
            use soma::cli::adapter_cloud_output::{run_blocking, AdapterCloudOutputContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    emit_error_json(&e);
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = AdapterCloudOutputContext { db_path };
            match run_blocking(&args, &ctx) {
                Ok(outcome) => {
                    let text = serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                    println!("{text}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("adapter_cloud_output", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::AdapterLifecycle(args) => {
            use soma::cli::adapter_lifecycle::run_blocking;
            match run_blocking(&args) {
                Ok(outcome) => {
                    let value = match args.format.trim().to_ascii_lowercase().as_str() {
                        "event" => outcome.emitted_event.clone(),
                        "report" => {
                            serde_json::to_value(&outcome).unwrap_or_else(|_| serde_json::json!({}))
                        }
                        other => {
                            eprintln!(
                                "{}",
                                error_json(
                                    "adapter_lifecycle",
                                    &format!("unknown format `{other}`; expected event or report")
                                )
                            );
                            std::process::exit(1);
                        }
                    };
                    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
                    println!("{text}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("adapter_lifecycle", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::AdapterSpool(args) => {
            use soma::capture::ai_cli::{emit_error_json, exit_code_for, resolve_db_path};
            use soma::cli::adapter_spool::{run_blocking, AdapterSpoolContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    emit_error_json(&e);
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = AdapterSpoolContext { db_path };
            match run_blocking(&args, &ctx) {
                Ok(outcome) => {
                    let text = serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                    println!("{text}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("adapter_spool", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::AdapterSpoolAppend(args) => {
            use soma::cli::adapter_spool::run_append_blocking;
            match run_append_blocking(&args) {
                Ok(outcome) => {
                    let text = serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                    println!("{text}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("adapter_spool_append", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::AdapterBindingProof(args) => {
            use soma::capture::ai_cli::{emit_error_json, exit_code_for, resolve_db_path};
            use soma::cli::adapter_binding_proof::{
                render_proof_session_brief, run_blocking, run_discover_installed_config_blocking,
                run_evidence_bundle_blocking, run_installed_config_check_blocking,
                run_list_blocking, run_prepare_installed_config_blocking,
                run_proof_session_blocking, run_real_app_proof_kit_blocking,
                run_render_evidence_packet_blocking, run_render_installed_config_blocking,
                run_status_blocking, run_verify_evidence_artifacts_blocking,
                AdapterBindingProofContext,
            };
            if args.real_app_proof_kit {
                match run_real_app_proof_kit_blocking(&args).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode real app proof kit outcome: {e}"),
                        )
                    })
                }) {
                    Ok(outcome) => {
                        let text =
                            serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                        println!("{text}");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("adapter_binding_proof", &e));
                        std::process::exit(e.exit_code());
                    }
                }
                return Ok(());
            }
            if args.discover_installed_config {
                match run_discover_installed_config_blocking(&args).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode installed config discovery outcome: {e}"),
                        )
                    })
                }) {
                    Ok(outcome) => {
                        let text =
                            serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                        println!("{text}");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("adapter_binding_proof", &e));
                        std::process::exit(e.exit_code());
                    }
                }
                return Ok(());
            }
            if args.render_installed_config || args.write_installed_config.is_some() {
                match run_render_installed_config_blocking(&args).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode installed config render outcome: {e}"),
                        )
                    })
                }) {
                    Ok(outcome) => {
                        let text =
                            serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                        println!("{text}");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("adapter_binding_proof", &e));
                        std::process::exit(e.exit_code());
                    }
                }
                return Ok(());
            }
            if args.render_render_evidence || args.write_render_evidence.is_some() {
                match run_render_evidence_packet_blocking(&args).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode render evidence packet outcome: {e}"),
                        )
                    })
                }) {
                    Ok(outcome) => {
                        let text =
                            serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                        println!("{text}");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("adapter_binding_proof", &e));
                        std::process::exit(e.exit_code());
                    }
                }
                return Ok(());
            }
            if args.prepare_installed_config {
                match run_prepare_installed_config_blocking(&args).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode installed config preparation outcome: {e}"),
                        )
                    })
                }) {
                    Ok(outcome) => {
                        let text =
                            serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                        println!("{text}");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("adapter_binding_proof", &e));
                        std::process::exit(e.exit_code());
                    }
                }
                return Ok(());
            }
            if args.check_installed_config {
                match run_installed_config_check_blocking(&args).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode installed config check outcome: {e}"),
                        )
                    })
                }) {
                    Ok(outcome) => {
                        let text =
                            serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                        println!("{text}");
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("adapter_binding_proof", &e));
                        std::process::exit(e.exit_code());
                    }
                }
                return Ok(());
            }
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    emit_error_json(&e);
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = AdapterBindingProofContext { db_path };
            if args.proof_session && args.wants_brief_output() {
                if args.json {
                    let e =
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            "--json cannot be combined with --brief or --format brief".to_string(),
                        );
                    eprintln!("{}", error_json("adapter_binding_proof", &e));
                    std::process::exit(e.exit_code());
                }
                match run_proof_session_blocking(&args, &ctx) {
                    Ok(outcome) => {
                        print!("{}", render_proof_session_brief(&outcome));
                    }
                    Err(e) => {
                        eprintln!("{}", error_json("adapter_binding_proof", &e));
                        std::process::exit(e.exit_code());
                    }
                }
                return Ok(());
            }
            let result = if args.list {
                run_list_blocking(&args, &ctx).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode list outcome: {e}"),
                        )
                    })
                })
            } else if args.evidence_bundle {
                run_evidence_bundle_blocking(&args, &ctx).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode evidence bundle outcome: {e}"),
                        )
                    })
                })
            } else if args.proof_session {
                run_proof_session_blocking(&args, &ctx).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode proof session outcome: {e}"),
                        )
                    })
                })
            } else if args.status {
                run_status_blocking(&args, &ctx).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode status outcome: {e}"),
                        )
                    })
                })
            } else if args.verify_evidence_artifacts {
                run_verify_evidence_artifacts_blocking(&args, &ctx).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode evidence artifact verification outcome: {e}"),
                        )
                    })
                })
            } else {
                run_blocking(&args, &ctx).and_then(|outcome| {
                    serde_json::to_value(outcome).map_err(|e| {
                        soma::cli::adapter_binding_proof::AdapterBindingProofError::MalformedInput(
                            format!("encode proof outcome: {e}"),
                        )
                    })
                })
            };
            match result {
                Ok(outcome) => {
                    let text = serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".to_string());
                    println!("{text}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("adapter_binding_proof", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::Clients(args) => {
            use soma::cli::client_status::{render_brief, render_json, render_text, run};
            match run(&args).and_then(|outcome| {
                if args.wants_json_output() {
                    render_json(&outcome)
                } else if args.brief {
                    Ok(render_brief(&outcome))
                } else {
                    Ok(render_text(&outcome))
                }
            }) {
                Ok(rendered) => print!("{rendered}"),
                Err(e) => {
                    eprintln!("{}", error_json("clients", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::Learning(args) => {
            use soma::cli::learning_status::{render_brief, render_json, render_text, run};
            match run(&args).and_then(|outcome| {
                if args.wants_json_output() {
                    render_json(&outcome)
                } else if args.wants_brief_output() {
                    Ok(render_brief(&outcome))
                } else {
                    Ok(render_text(&outcome))
                }
            }) {
                Ok(rendered) => print!("{rendered}"),
                Err(e) => {
                    eprintln!("{}", error_json("learning", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::Recall(args) => {
            use soma::cli::recall::{exit_code_for, resolve_db_path, run_recall, RecallContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("recall.path", &e));
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = RecallContext { db_path };
            match run_recall(&args, &ctx) {
                Ok((rendered, _)) => {
                    print!("{rendered}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("recall", &e));
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        Cmd::Profile(args) => {
            use soma::cli::profile::{exit_code_for, resolve_db_path, run_profile, ProfileContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("profile.path", &e));
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = ProfileContext { db_path };
            match run_profile(&args, &ctx) {
                Ok(rendered) => {
                    print!("{rendered}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("profile", &e));
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        Cmd::Projects(args) => {
            use soma::cli::projects::{resolve_db_path, run_projects, ProjectExperienceContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("projects.path", &e));
                    std::process::exit(e.exit_code());
                }
            };
            let ctx = ProjectExperienceContext { db_path };
            match run_projects(&args, &ctx) {
                Ok(rendered) => {
                    print!("{rendered}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("projects", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::Context(args) => {
            use soma::cli::context::{
                exit_code_for, resolve_db_path, run_context, ContextCliContext,
            };
            let db_path = match resolve_db_path(match &args.mode {
                soma::cli::ContextMode::Render(render) => render.db_path.as_deref(),
                soma::cli::ContextMode::Prompt(prompt) => prompt.db_path.as_deref(),
                soma::cli::ContextMode::TaskFrame(task_frame) => task_frame.db_path.as_deref(),
                soma::cli::ContextMode::TaskFrames(task_frames) => match &task_frames.mode {
                    soma::cli::ContextTaskFramesMode::Retention(retention) => {
                        retention.db_path.as_deref()
                    }
                    soma::cli::ContextTaskFramesMode::Outcomes(outcomes) => {
                        outcomes.db_path.as_deref()
                    }
                },
                soma::cli::ContextMode::TaskFrameOutcome(outcome) => outcome.db_path.as_deref(),
                soma::cli::ContextMode::L3Decay(decay) => decay.db_path.as_deref(),
                soma::cli::ContextMode::L2Promote(promote) => promote.db_path.as_deref(),
                soma::cli::ContextMode::LatentPredict(predict) => predict.db_path.as_deref(),
                soma::cli::ContextMode::LatentPacket(packet) => packet.db_path.as_deref(),
                soma::cli::ContextMode::LatentEval(eval) => eval.db_path.as_deref(),
                soma::cli::ContextMode::ThreadIdentity(identity) => identity.db_path.as_deref(),
                soma::cli::ContextMode::Correct(correct) => correct.db_path.as_deref(),
                soma::cli::ContextMode::VerifyClaim(verify) => verify.db_path.as_deref(),
                soma::cli::ContextMode::LearningProposals(proposals) => match &proposals.mode {
                    soma::cli::ContextLearningProposalMode::List(list) => list.db_path.as_deref(),
                    soma::cli::ContextLearningProposalMode::Apply(apply) => {
                        apply.db_path.as_deref()
                    }
                    soma::cli::ContextLearningProposalMode::ApplyReady(apply_ready) => {
                        apply_ready.db_path.as_deref()
                    }
                    soma::cli::ContextLearningProposalMode::SetStatus(status) => {
                        status.db_path.as_deref()
                    }
                },
                soma::cli::ContextMode::ReviewQueue(review) => review.db_path.as_deref(),
                soma::cli::ContextMode::ReviewActions(actions) => actions.db_path.as_deref(),
                soma::cli::ContextMode::ReviewBatchTemplate(template) => {
                    template.db_path.as_deref()
                }
                soma::cli::ContextMode::ReviewReport(report) => report.db_path.as_deref(),
                soma::cli::ContextMode::ReviewDigest(digest) => digest.db_path.as_deref(),
                soma::cli::ContextMode::ReviewDigestAck(ack) => ack.db_path.as_deref(),
                soma::cli::ContextMode::ReviewRender(render) => render.db_path.as_deref(),
                soma::cli::ContextMode::ReviewDrain(drain) => drain.db_path.as_deref(),
                soma::cli::ContextMode::SchedulerRun(scheduler) => scheduler.db_path.as_deref(),
                soma::cli::ContextMode::SemanticProposals(proposals) => {
                    proposals.db_path.as_deref()
                }
                soma::cli::ContextMode::OpenDecisionProposals(proposals) => {
                    proposals.db_path.as_deref()
                }
                soma::cli::ContextMode::ReviewAction(action) => action.db_path.as_deref(),
                soma::cli::ContextMode::ReviewBatch(batch) => batch.db_path.as_deref(),
                soma::cli::ContextMode::Audit(audit) => audit.db_path.as_deref(),
                soma::cli::ContextMode::TrustAudit(audit) => audit.db_path.as_deref(),
                soma::cli::ContextMode::HardeningReport(report) => report.db_path.as_deref(),
                soma::cli::ContextMode::Why(why) => why.db_path.as_deref(),
                #[cfg(feature = "cognitive")]
                soma::cli::ContextMode::CompareRanking(compare) => compare.db_path.as_deref(),
            }) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("context.path", &e));
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = ContextCliContext { db_path };
            match run_context(&args, &ctx) {
                Ok(rendered) => {
                    print!("{rendered}");
                }
                Err(e) => {
                    eprintln!("{}", error_json("context", &e));
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        Cmd::Config => {
            // D94-cand — show the *effective* config including any
            // on-disk override at `~/.soma/config.toml`. Falls back
            // to defaults when the file is absent. Render with current
            // context-layer names; legacy on-disk keys stay accepted.
            let cfg = match dirs::home_dir() {
                Some(home) => soma::config::Config::load_or_default(&home.join(".soma")),
                None => soma::config::Config::default_v1(),
            };
            println!("{}", cfg.render_context_layer_json());
        }
        Cmd::McpConfig(args) => {
            use soma::cli::mcp_config::{render_brief, render_json, run};
            match run(&args).and_then(|outcome| {
                if args.wants_brief_output() {
                    Ok(render_brief(&outcome))
                } else {
                    render_json(&outcome)
                }
            }) {
                Ok(rendered) => print!("{rendered}"),
                Err(e) => {
                    eprintln!("{}", error_json("mcp_config", &e));
                    std::process::exit(e.exit_code());
                }
            }
        }
        Cmd::Inspect(args) => {
            use soma::cli::inspect::{exit_code_for, resolve_db_path, run_inspect, InspectContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("inspect.path", &e));
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = InspectContext { db_path };
            match run_inspect(&args, &ctx) {
                Ok(rendered) => println!("{rendered}"),
                Err(e) => {
                    eprintln!("{}", error_json("inspect", &e));
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        Cmd::Forget(args) => {
            use soma::cli::forget::{exit_code_for, resolve_db_path, run_forget, ForgetContext};
            let db_path = match resolve_db_path(args.db_path.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("forget.path", &e));
                    std::process::exit(exit_code_for(&e));
                }
            };
            let ctx = ForgetContext { db_path };
            match run_forget(&args, &ctx) {
                Ok(outcome) => {
                    println!("soma: forgot {} episode(s)", outcome.forgotten_count);
                }
                Err(e) => {
                    eprintln!("{}", error_json("forget", &e));
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        #[cfg(feature = "pty-capture")]
        Cmd::Capture(args) => {
            use soma::cli::capture::{exit_code_for, run_pty_capture, CaptureContext};
            if !args.pty {
                eprintln!(
                    "{}",
                    error_json("capture", &"only --pty mode is wired in v1.1; use --pty")
                );
                std::process::exit(1);
            }
            let db_path = match soma::capture::ai_cli::resolve_db_path(None) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("capture.path", &e));
                    std::process::exit(3);
                }
            };
            let session = args.session.unwrap_or_else(|| format!("pty-{}", std::process::id()));
            let ctx = CaptureContext { db_path, project: args.project, session_id: session };
            match run_pty_capture(&ctx) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("{}", error_json("capture", &e));
                    std::process::exit(exit_code_for(&e));
                }
            }
        }
        Cmd::SynthesizeNarrative => {
            use soma::runtime::scheduler::slow_loop::run_narrative_synthesis;
            use soma::storage::Storage;
            use std::sync::{Arc, Mutex};
            let db_path = match soma::capture::ai_cli::resolve_db_path(None) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("synthesize.path", &e));
                    std::process::exit(3);
                }
            };
            let store = match Storage::open(&db_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{}", error_json("synthesize.open", &e));
                    std::process::exit(2);
                }
            };
            let storage = Arc::new(Mutex::new(store));
            let updated = run_narrative_synthesis(&storage);
            if !updated {
                println!("soma: narrative synthesis returned empty (need ≥3 episodes)");
                std::process::exit(0);
            }
            // Read back what was just written so the user sees the
            // resulting paragraph immediately.
            let store = Storage::open(&db_path).unwrap();
            match store.get_narrative() {
                Ok(Some((paragraph, ts, kind))) => {
                    println!("# Narrative ({kind}, synthesized_at_ns={ts})\n");
                    println!("{paragraph}");
                }
                _ => {
                    println!("soma: narrative row not readable post-write");
                }
            }
        }
        Cmd::McpServe => {
            use soma::runtime::mcp::run_stdio_default;
            // Reuse the ingest CLI's DB path resolution so all
            // `soma` subcommands share the same layered override
            // rules (CLI override is unused here — MCP server is
            // spawned by the MCP client with no args).
            let db_path = match soma::capture::ai_cli::resolve_db_path(None) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", error_json("mcp.path", &e));
                    std::process::exit(3);
                }
            };
            // D91 §B — on unix, try the resident socket first so all
            // MCP fetches accumulate in the resident's single
            // MemoryPackCache. Pre-fix every child held its own
            // cache and `soma status` showed `0 fetches` forever.
            //
            // Codex 2차 review #3 Q2 — preflight (connect + Hello)
            // BEFORE stdin is read so a stale socket / dead resident
            // / version mismatch falls back to standalone instead
            // of consuming the first JSON-RPC request and surfacing
            // a half-broken state to the user's first MCP session.
            //
            // On Windows the resident plane is gated out (D56-cand
            // tracks named-pipe parity), so we always take the
            // standalone path.
            let result = {
                #[cfg(unix)]
                {
                    use soma::runtime::mcp::{
                        resident_default_db_matches, resident_preflight, run_stdio_via_resident,
                    };
                    let resident_root = soma::cli::start::resolve_home_root().ok();
                    let socket_path =
                        resident_root.as_ref().map(|root| root.join("run").join("soma.sock"));
                    let resident_db_matches = resident_root
                        .as_ref()
                        .is_some_and(|root| resident_default_db_matches(root, &db_path));
                    let prefer_resident = match (resident_db_matches, socket_path.as_deref()) {
                        (true, Some(p)) => resident_preflight(p),
                        _ => false,
                    };
                    let stdin = std::io::stdin();
                    let stdout = std::io::stdout();
                    if prefer_resident {
                        let path = socket_path.unwrap();
                        run_stdio_via_resident(stdin.lock(), stdout.lock(), &path)
                    } else {
                        // D158 close — fallback 이 silent 였던 거
                        // audible. MCP child 의 stderr 는 cloud LLM
                        // 클라이언트 (Claude Code) 의 hook log 로
                        // 흘러서 operator 가 "왜 cache fetches 가
                        // 0 이지?" cause 한 줄로 추적 가능.
                        eprintln!(
                            "{}",
                            error_json(
                                "mcp.fallback",
                                &if resident_db_matches {
                                    "resident preflight failed — using standalone MCP cache (소켓 미존재 / 응답 없음 / 버전 mismatch). `soma status` 로 resident 상태 확인."
                                } else {
                                    "resident DB path differs from active SOMA_DB/persona; using standalone MCP cache to preserve persona isolation."
                                }
                                    .to_string()
                            )
                        );
                        run_stdio_default(db_path)
                    }
                }
                #[cfg(not(unix))]
                {
                    run_stdio_default(db_path)
                }
            };
            if let Err(e) = result {
                eprintln!("{}", error_json("mcp.stdio", &e));
                std::process::exit(2);
            }
        }
        Cmd::Diagnose(_args) => {
            // D129-cand close (R9 audit) — diagnostic JSON dump.
            // Always exits 0; sub-step failures append to `_errors`.
            if let Err(e) = soma::cli::diagnose::run_blocking() {
                eprintln!("{}", error_json("diagnose", &e));
                std::process::exit(2);
            }
        }
        Cmd::Backfill => {
            // D70 close — operator-triggered primary-model backfill.
            use soma::cli::backfill::{exit_code_for, run_blocking};
            if let Err(e) = run_blocking() {
                eprintln!("{}", error_json("backfill", &e.to_string()));
                std::process::exit(exit_code_for(&e));
            }
        }
        Cmd::Logs(args) => {
            // D27 close — tail the rolling log file.
            use soma::cli::logs::{exit_code_for, run_blocking};
            if let Err(e) = run_blocking(&args) {
                eprintln!("{}", error_json("logs", &e));
                std::process::exit(exit_code_for(&e));
            }
        }
        Cmd::Completions(args) => {
            // Completion scripts are a public discovery surface, so
            // they intentionally omit hidden legacy migration/debug
            // verbs even though those verbs remain manually callable.
            let mut cmd = completion_command();
            clap_complete::generate(args.shell, &mut cmd, "soma", &mut std::io::stdout());
        }
        Cmd::Persona(args) => {
            // Compatibility namespace: local named persona registry
            // aliases plus legacy context/profile helper artifacts for
            // inspection and disabled prompt-injection migration.
            use soma::cli::persona::{exit_code_for, run_blocking, Mode};
            use soma::cli::PersonaMode;
            match args.mode {
                PersonaMode::List(args) => match soma::cli::persona_registry::run_list(&args) {
                    Ok(rendered) => print!("{rendered}"),
                    Err(e) => {
                        eprintln!("{}", error_json("persona.list", &e));
                        std::process::exit(e.exit_code());
                    }
                },
                PersonaMode::Create(args) => match soma::cli::persona_registry::run_create(&args) {
                    Ok(rendered) => print!("{rendered}"),
                    Err(e) => {
                        eprintln!("{}", error_json("persona.create", &e));
                        std::process::exit(e.exit_code());
                    }
                },
                PersonaMode::Call(args) => match soma::cli::persona_registry::run_call(&args) {
                    Ok(rendered) => print!("{rendered}"),
                    Err(e) => {
                        eprintln!("{}", error_json("persona.call", &e));
                        std::process::exit(e.exit_code());
                    }
                },
                PersonaMode::Read => {
                    if let Err(e) = run_blocking(Mode::Read) {
                        eprintln!("{}", error_json("persona", &e));
                        std::process::exit(exit_code_for(&e));
                    }
                }
                PersonaMode::Regen => {
                    if let Err(e) = run_blocking(Mode::Regen) {
                        eprintln!("{}", error_json("persona", &e));
                        std::process::exit(exit_code_for(&e));
                    }
                }
                PersonaMode::Inject => {
                    if let Err(e) = run_blocking(Mode::Inject) {
                        eprintln!("{}", error_json("persona", &e));
                        std::process::exit(exit_code_for(&e));
                    }
                }
            }
        }
        // D152 chunk 1.1 (ADR 0012) — dashboard GUI server. The
        // verb itself is feature-gated in the CLI enum; default
        // builds never see the variant.
        #[cfg(feature = "dashboard")]
        Cmd::Serve(args) => {
            use soma::cli::serve::{exit_code_for as serve_exit_code, run_blocking};
            if let Err(e) = run_blocking(args) {
                eprintln!("{}", error_json("serve", &e));
                std::process::exit(serve_exit_code(&e));
            }
        }
        // D153 phase 4.5 (ADR 0013) — `soma sync-claudemd` splice
        // SOMA-section into `<cwd>/CLAUDE.md`. user-side trigger,
        // markers-bounded.
        Cmd::SyncClaudemd(args) => {
            use soma::cli::sync::{exit_code_for as sync_exit_code, run_sync_claudemd};
            if let Err(e) = run_sync_claudemd(args.project) {
                eprintln!("{}", error_json("sync-claudemd", &e));
                std::process::exit(sync_exit_code(&e));
            }
        }
    }
    Ok(())
}
