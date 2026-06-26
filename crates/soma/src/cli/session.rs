//! `soma session` - shell-visible session scope for multi-terminal work.
//!
//! SOMA's durable memory already scopes episodes by `episodes.session_id` and
//! can bind multiple sessions into an operator-confirmed thread identity. This
//! command provides the missing front-door UX: generate or attach a session id
//! in the current shell so terminal capture, Claude Code hooks, Codex CLI
//! wrappers, and adapter spool writers all stamp the same local scope.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::cli::{SessionArgs, SessionClearArgs, SessionMode, SessionShell};
use crate::storage::EpisodeSource;

const SESSION_ENV: &str = "SOMA_SESSION_ID";
const CLIENT_ENV: &str = "SOMA_CLIENT";
const PROJECT_ENV: &str = "SOMA_PROJECT";
const THREAD_ENV: &str = "SOMA_THREAD_KEY";
const PERSONA_ENV: &str = "SOMA_PERSONA";
const PERSONA_HOME_ENV: &str = "SOMA_PERSONA_HOME";
const DB_ENV: &str = "SOMA_DB";
const ADAPTER_SPOOL_JSONL_ENV: &str = "SOMA_ADAPTER_SPOOL_JSONL";

const COMPAT_ENVS: &[(&str, &str)] = &[
    ("SOMA_ADAPTER_CAPTURE_SOURCE", CLIENT_ENV),
    ("SOMA_ADAPTER_CLIENT", CLIENT_ENV),
    ("SOMA_ADAPTER_PROJECT", PROJECT_ENV),
    ("SOMA_ADAPTER_SESSION_ID", SESSION_ENV),
    ("SOMA_ADAPTER_LIFECYCLE_CLIENT", CLIENT_ENV),
    ("SOMA_ADAPTER_LIFECYCLE_PROJECT", PROJECT_ENV),
    ("SOMA_ADAPTER_LIFECYCLE_SESSION", SESSION_ENV),
];

#[derive(Debug)]
pub enum SessionError {
    MalformedInput(String),
    Render(serde_json::Error),
}

impl SessionError {
    pub fn exit_code(&self) -> i32 {
        match self {
            SessionError::MalformedInput(_) => 1,
            SessionError::Render(_) => 2,
        }
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::MalformedInput(message) => write!(f, "malformed input: {message}"),
            SessionError::Render(err) => write!(f, "render: {err}"),
        }
    }
}

impl std::error::Error for SessionError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionScope {
    pub session_id: String,
    pub client: String,
    pub project: Option<String>,
    pub thread_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionReport {
    pub kind: &'static str,
    pub scope: SessionScope,
    pub scope_status: &'static str,
    pub ready_for_project_scoped_capture: bool,
    pub missing_scope_envs: Vec<&'static str>,
    pub persona_scope: SessionPersonaScope,
    pub shell: &'static str,
    pub exports: Vec<SessionExport>,
    pub usage: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionPersonaScope {
    pub active_persona: String,
    pub activation_status: &'static str,
    pub db_path: String,
    pub db_path_source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_spool_jsonl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionExport {
    pub name: String,
    pub value: String,
}

pub fn run(args: &SessionArgs) -> Result<String, SessionError> {
    match &args.mode {
        SessionMode::Start(start) => {
            let client = validate_client(&start.client)?;
            let project = resolve_project(start.project.as_deref());
            let session_id = crate::storage::session::managed_cli(
                &client,
                project.as_deref(),
                now_ns(),
                std::process::id(),
            );
            let scope = SessionScope {
                session_id,
                client,
                project,
                thread_key: nonempty_owned(start.thread_key.as_deref()),
            };
            render_scope_report("soma_session_start", scope, start.shell, start.json)
        }
        SessionMode::Attach(attach) => {
            let session_id = nonempty_owned(Some(&attach.session_id)).ok_or_else(|| {
                SessionError::MalformedInput("--session-id must be non-empty".to_string())
            })?;
            let client = match nonempty_owned(attach.client.as_deref()) {
                Some(client) => validate_client(&client)?,
                None => env_nonempty(CLIENT_ENV).unwrap_or_else(|| "terminal".to_string()),
            };
            let project = resolve_project(attach.project.as_deref());
            let scope = SessionScope {
                session_id,
                client,
                project,
                thread_key: nonempty_owned(attach.thread_key.as_deref())
                    .or_else(|| env_nonempty(THREAD_ENV)),
            };
            render_scope_report("soma_session_attach", scope, attach.shell, attach.json)
        }
        SessionMode::Status(status) => {
            let scope = SessionScope {
                session_id: env_nonempty(SESSION_ENV).unwrap_or_default(),
                client: env_nonempty(CLIENT_ENV).unwrap_or_default(),
                project: env_nonempty(PROJECT_ENV),
                thread_key: env_nonempty(THREAD_ENV),
            };
            if status.json {
                let report = SessionReport {
                    kind: "soma_session_status",
                    scope_status: scope_status(&scope),
                    ready_for_project_scoped_capture: ready_for_project_scoped_capture(&scope),
                    missing_scope_envs: missing_scope_envs(&scope),
                    shell: "env",
                    exports: current_exports(),
                    usage: usage_lines(),
                    scope,
                    persona_scope: current_persona_scope(),
                };
                return serde_json::to_string_pretty(&report)
                    .map(|mut text| {
                        text.push('\n');
                        text
                    })
                    .map_err(SessionError::Render);
            }
            Ok(render_human_status(&scope, &current_persona_scope()))
        }
        SessionMode::Clear(clear) => render_clear(clear),
    }
}

fn render_scope_report(
    kind: &'static str,
    scope: SessionScope,
    shell: SessionShell,
    json: bool,
) -> Result<String, SessionError> {
    let shell = resolve_shell(shell);
    let exports = exports_for_scope(&scope);
    let report = SessionReport {
        kind,
        scope_status: scope_status(&scope),
        ready_for_project_scoped_capture: ready_for_project_scoped_capture(&scope),
        missing_scope_envs: missing_scope_envs(&scope),
        scope,
        persona_scope: current_persona_scope(),
        shell: shell.as_str(),
        exports: exports.clone(),
        usage: usage_lines(),
    };
    if json {
        return serde_json::to_string_pretty(&report)
            .map(|mut text| {
                text.push('\n');
                text
            })
            .map_err(SessionError::Render);
    }
    Ok(render_exports(shell, &exports))
}

fn render_clear(clear: &SessionClearArgs) -> Result<String, SessionError> {
    let shell = resolve_shell(clear.shell);
    let names = all_env_names();
    if clear.json {
        let value = serde_json::json!({
            "kind": "soma_session_clear",
            "shell": shell.as_str(),
            "unset": names,
        });
        return serde_json::to_string_pretty(&value)
            .map(|mut text| {
                text.push('\n');
                text
            })
            .map_err(SessionError::Render);
    }
    Ok(render_unsets(shell, &names))
}

fn validate_client(client: &str) -> Result<String, SessionError> {
    let client = client.trim();
    if client.is_empty() {
        return Err(SessionError::MalformedInput("--client must be non-empty".to_string()));
    }
    client
        .parse::<EpisodeSource>()
        .map(|source| source.to_string())
        .map_err(|err| SessionError::MalformedInput(err.to_string()))
}

fn resolve_project(explicit: Option<&str>) -> Option<String> {
    nonempty_owned(explicit)
        .or_else(|| env_nonempty(PROJECT_ENV))
        .or_else(crate::project::current_name)
}

fn exports_for_scope(scope: &SessionScope) -> Vec<SessionExport> {
    let mut exports = vec![
        SessionExport { name: SESSION_ENV.to_string(), value: scope.session_id.clone() },
        SessionExport { name: CLIENT_ENV.to_string(), value: scope.client.clone() },
    ];
    if let Some(project) = &scope.project {
        exports.push(SessionExport { name: PROJECT_ENV.to_string(), value: project.clone() });
    }
    if let Some(thread_key) = &scope.thread_key {
        exports.push(SessionExport { name: THREAD_ENV.to_string(), value: thread_key.clone() });
    }
    for (alias, canonical) in COMPAT_ENVS {
        let value = match *canonical {
            CLIENT_ENV => Some(scope.client.clone()),
            PROJECT_ENV => scope.project.clone(),
            SESSION_ENV => Some(scope.session_id.clone()),
            _ => None,
        };
        if let Some(value) = value {
            exports.push(SessionExport { name: (*alias).to_string(), value });
        }
    }
    exports
}

fn scope_status(scope: &SessionScope) -> &'static str {
    if ready_for_project_scoped_capture(scope) {
        "ready_for_project_scoped_capture"
    } else {
        "capture_scope_incomplete"
    }
}

fn ready_for_project_scoped_capture(scope: &SessionScope) -> bool {
    missing_scope_envs(scope).is_empty()
}

fn missing_scope_envs(scope: &SessionScope) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if scope.session_id.trim().is_empty() {
        missing.push(SESSION_ENV);
    }
    if scope.client.trim().is_empty() {
        missing.push(CLIENT_ENV);
    }
    if scope.project.as_deref().unwrap_or_default().trim().is_empty() {
        missing.push(PROJECT_ENV);
    }
    missing
}

fn current_exports() -> Vec<SessionExport> {
    all_env_names()
        .into_iter()
        .filter_map(|name| std::env::var(&name).ok().map(|value| SessionExport { name, value }))
        .collect()
}

fn all_env_names() -> Vec<String> {
    let mut names = vec![
        SESSION_ENV.to_string(),
        CLIENT_ENV.to_string(),
        PROJECT_ENV.to_string(),
        THREAD_ENV.to_string(),
    ];
    names.extend(COMPAT_ENVS.iter().map(|(alias, _)| (*alias).to_string()));
    names.sort();
    names.dedup();
    names
}

fn usage_lines() -> Vec<String> {
    let project = crate::project::current_name().unwrap_or_else(|| "<project>".to_string());
    vec![
        "eval \"$(soma call <persona>)\"".to_string(),
        format!("eval \"$(soma session start --client codex-cli --project {project})\""),
        format!("eval \"$(soma session start --client claude-code --project {project})\""),
        format!(
            "eval \"$(soma session attach --session-id <id> --client terminal --project {project})\""
        ),
        "soma session status --json".to_string(),
        "soma projects --json".to_string(),
        format!(
            "soma context thread-identity --project {project} --confirm-session <id> --confirm ..."
        ),
    ]
}

fn render_exports(shell: ShellFlavor, exports: &[SessionExport]) -> String {
    let mut out = String::new();
    for export in exports {
        match shell {
            ShellFlavor::Fish => {
                out.push_str(&format!("set -gx {} {};\n", export.name, fish_quote(&export.value)));
            }
            ShellFlavor::Posix => {
                out.push_str(&format!("export {}={};\n", export.name, sh_quote(&export.value)));
            }
        }
    }
    out
}

fn render_unsets(shell: ShellFlavor, names: &[String]) -> String {
    let mut out = String::new();
    for name in names {
        match shell {
            ShellFlavor::Fish => out.push_str(&format!("set -e {name};\n")),
            ShellFlavor::Posix => out.push_str(&format!("unset {name};\n")),
        }
    }
    out
}

fn render_human_status(scope: &SessionScope, persona: &SessionPersonaScope) -> String {
    let mut out = String::new();
    out.push_str("SOMA session status\n");
    out.push_str(&format!("{}={}\n", SESSION_ENV, empty_marker(&scope.session_id)));
    out.push_str(&format!("{}={}\n", CLIENT_ENV, empty_marker(&scope.client)));
    out.push_str(&format!("{}={}\n", PROJECT_ENV, scope.project.as_deref().unwrap_or("<unset>")));
    out.push_str(&format!("{}={}\n", THREAD_ENV, scope.thread_key.as_deref().unwrap_or("<unset>")));
    out.push_str(&format!("{}={}\n", PERSONA_ENV, persona.active_persona));
    out.push_str(&format!("persona_activation_status={}\n", persona.activation_status));
    out.push_str(&format!("{}={}\n", DB_ENV, persona.db_path));
    out.push_str(&format!("db_path_source={}\n", persona.db_path_source));
    out.push_str(&format!(
        "{}={}\n",
        PERSONA_HOME_ENV,
        persona.persona_home.as_deref().unwrap_or("<unresolved>")
    ));
    out.push_str(&format!(
        "{}={}\n",
        ADAPTER_SPOOL_JSONL_ENV,
        persona.adapter_spool_jsonl.as_deref().unwrap_or("<unresolved>")
    ));
    out
}

fn empty_marker(value: &str) -> &str {
    if value.is_empty() {
        "<unset>"
    } else {
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellFlavor {
    Posix,
    Fish,
}

impl ShellFlavor {
    fn as_str(self) -> &'static str {
        match self {
            ShellFlavor::Posix => "posix",
            ShellFlavor::Fish => "fish",
        }
    }
}

fn resolve_shell(shell: SessionShell) -> ShellFlavor {
    match shell {
        SessionShell::Auto => {
            let shell = std::env::var("SHELL").unwrap_or_default();
            if shell.ends_with("fish") {
                ShellFlavor::Fish
            } else {
                ShellFlavor::Posix
            }
        }
        SessionShell::Sh | SessionShell::Bash | SessionShell::Zsh => ShellFlavor::Posix,
        SessionShell::Fish => ShellFlavor::Fish,
    }
}

fn sh_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fish_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| nonempty_owned(Some(&value)))
}

fn current_persona_scope() -> SessionPersonaScope {
    let persona_env = env_nonempty(PERSONA_ENV);
    let active_persona = persona_env.clone().unwrap_or_else(|| "default".to_string());
    let db_env = env_nonempty(DB_ENV);
    let default_db_path = default_db_path();
    let db_path = db_env.clone().or(default_db_path).unwrap_or_else(|| "<unresolved>".to_string());
    let db_path_source = if db_env.is_some() { "SOMA_DB" } else { "default_home" };
    let persona_home = env_nonempty(PERSONA_HOME_ENV).or_else(|| {
        if let Some(db) = db_env.as_deref() {
            return PathBuf::from(db).parent().map(|path| path.display().to_string());
        }
        default_soma_home()
    });
    let adapter_spool_jsonl = env_nonempty(ADAPTER_SPOOL_JSONL_ENV).or_else(|| {
        persona_home.as_ref().map(|home| {
            PathBuf::from(home).join("adapter").join("events.jsonl").display().to_string()
        })
    });
    let activation_status = match (persona_env.is_some(), db_env.is_some()) {
        (true, true) => "active_persona_isolated_store",
        (true, false) => "active_persona_missing_soma_db_export",
        (false, true) => "db_path_override_without_persona_name",
        (false, false) => "default_persona_store",
    };
    SessionPersonaScope {
        active_persona,
        activation_status,
        db_path,
        db_path_source,
        persona_home,
        adapter_spool_jsonl,
    }
}

fn default_db_path() -> Option<String> {
    default_soma_home().map(|home| PathBuf::from(home).join("soma.db").display().to_string())
}

fn default_soma_home() -> Option<String> {
    dirs::home_dir().map(|home| home.join(".soma").display().to_string())
}

fn nonempty_owned(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{SessionStartArgs, SessionStatusArgs};

    #[test]
    fn start_renders_codex_exports() {
        let out = run(&SessionArgs {
            mode: SessionMode::Start(SessionStartArgs {
                client: "codex-cli".to_string(),
                project: Some("SOMA".to_string()),
                thread_key: Some("thread-1".to_string()),
                shell: SessionShell::Sh,
                json: false,
            }),
        })
        .expect("session start");
        assert!(out.contains("export SOMA_SESSION_ID='soma-codex-cli-soma-"));
        assert!(out.contains("export SOMA_CLIENT='codex-cli';"));
        assert!(out.contains("export SOMA_PROJECT='SOMA';"));
        assert!(out.contains("export SOMA_THREAD_KEY='thread-1';"));
        assert!(out.contains("export SOMA_ADAPTER_SESSION_ID='soma-codex-cli-soma-"));
    }

    #[test]
    fn status_json_renders_without_env() {
        let out = run(&SessionArgs { mode: SessionMode::Status(SessionStatusArgs { json: true }) })
            .expect("session status");
        assert!(out.contains("\"kind\": \"soma_session_status\""));
        assert!(out.contains("\"persona_scope\""));
    }
}
