//! Local named SOMA personas.
//!
//! A named persona is a local namespace for SOMA's learned state. It maps to
//! a dedicated `soma.db` under `~/.soma/personas/<name>/`. `soma call <name>`
//! emits shell exports that point `SOMA_DB` at that database, so every normal
//! capture, recall, context, MCP, and scheduler path uses the selected persona
//! without a separate storage abstraction.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cli::{PersonaCallArgs, PersonaCreateArgs, PersonaListArgs, SessionShell};
use crate::storage::{EpisodeSource, Storage};

const PERSONA_ENV: &str = "SOMA_PERSONA";
const PERSONA_HOME_ENV: &str = "SOMA_PERSONA_HOME";
const PERSONAS_DIR_ENV: &str = "SOMA_PERSONAS_DIR";
const DB_ENV: &str = "SOMA_DB";
const ADAPTER_SPOOL_JSONL_ENV: &str = "SOMA_ADAPTER_SPOOL_JSONL";
const ADAPTER_SPOOL_CHECKPOINT_ENV: &str = "SOMA_ADAPTER_SPOOL_CHECKPOINT";
const ADAPTER_LIFECYCLE_JSONL_ENV: &str = "SOMA_ADAPTER_LIFECYCLE_JSONL";
const CURSOR_EVENT_JSONL_ENV: &str = "SOMA_CURSOR_EVENT_JSONL";
const CURSOR_EVENT_CHECKPOINT_ENV: &str = "SOMA_CURSOR_EVENT_CHECKPOINT";
const SESSION_ENV: &str = "SOMA_SESSION_ID";
const CLIENT_ENV: &str = "SOMA_CLIENT";
const PROJECT_ENV: &str = "SOMA_PROJECT";
const THREAD_ENV: &str = "SOMA_THREAD_KEY";
const DEFAULT_PERSONA: &str = "default";
const METADATA_FILE: &str = "persona.json";
const DB_FILE: &str = "soma.db";
const SESSION_COMPAT_ENVS: &[(&str, &str)] = &[
    ("SOMA_ADAPTER_CAPTURE_SOURCE", CLIENT_ENV),
    ("SOMA_ADAPTER_CLIENT", CLIENT_ENV),
    ("SOMA_ADAPTER_PROJECT", PROJECT_ENV),
    ("SOMA_ADAPTER_SESSION_ID", SESSION_ENV),
    ("SOMA_ADAPTER_LIFECYCLE_CLIENT", CLIENT_ENV),
    ("SOMA_ADAPTER_LIFECYCLE_PROJECT", PROJECT_ENV),
    ("SOMA_ADAPTER_LIFECYCLE_SESSION", SESSION_ENV),
];

#[derive(Debug)]
pub enum PersonaRegistryError {
    InvalidName(String),
    InvalidScope(String),
    NotFound(String),
    AlreadyExists(String),
    Path(String),
    Io(std::io::Error),
    Storage(crate::storage::StorageError),
    Json(serde_json::Error),
}

impl PersonaRegistryError {
    pub fn exit_code(&self) -> i32 {
        match self {
            PersonaRegistryError::InvalidName(_)
            | PersonaRegistryError::InvalidScope(_)
            | PersonaRegistryError::NotFound(_)
            | PersonaRegistryError::AlreadyExists(_) => 1,
            PersonaRegistryError::Path(_) | PersonaRegistryError::Io(_) => 3,
            PersonaRegistryError::Storage(_) | PersonaRegistryError::Json(_) => 2,
        }
    }
}

impl std::fmt::Display for PersonaRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersonaRegistryError::InvalidName(message) => {
                write!(f, "invalid persona name: {message}")
            }
            PersonaRegistryError::InvalidScope(message) => {
                write!(f, "invalid session scope: {message}")
            }
            PersonaRegistryError::NotFound(name) => {
                write!(f, "persona `{name}` does not exist; run `soma create {name}` first")
            }
            PersonaRegistryError::AlreadyExists(name) => {
                write!(f, "persona `{name}` already exists; pass --if-not-exists to reuse it")
            }
            PersonaRegistryError::Path(message) => write!(f, "path: {message}"),
            PersonaRegistryError::Io(err) => write!(f, "io: {err}"),
            PersonaRegistryError::Storage(err) => write!(f, "storage: {err}"),
            PersonaRegistryError::Json(err) => write!(f, "json: {err}"),
        }
    }
}

impl std::error::Error for PersonaRegistryError {}

impl From<std::io::Error> for PersonaRegistryError {
    fn from(value: std::io::Error) -> Self {
        PersonaRegistryError::Io(value)
    }
}

impl From<crate::storage::StorageError> for PersonaRegistryError {
    fn from(value: crate::storage::StorageError) -> Self {
        PersonaRegistryError::Storage(value)
    }
}

impl From<serde_json::Error> for PersonaRegistryError {
    fn from(value: serde_json::Error) -> Self {
        PersonaRegistryError::Json(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersonaMetadata {
    schema_version: u32,
    name: String,
    description: Option<String>,
    created_at_ns: i64,
    updated_at_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct PersonaInfo {
    pub name: String,
    pub home: String,
    pub db_path: String,
    pub exists: bool,
    pub active: bool,
    pub built_in_default: bool,
    pub invalid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonaListReport {
    pub kind: &'static str,
    pub active_persona: Option<String>,
    pub personas_dir: String,
    pub personas: Vec<PersonaInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonaCreateReport {
    pub kind: &'static str,
    pub created: bool,
    pub persona: PersonaInfo,
    pub usage: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonaCallReport {
    pub kind: &'static str,
    pub persona: PersonaInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_scope: Option<PersonaSessionScope>,
    pub shell: &'static str,
    pub exports: Vec<PersonaExport>,
    pub usage: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonaSessionScope {
    pub session_id: String,
    pub client: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_key: Option<String>,
    pub scope_status: &'static str,
    pub ready_for_project_scoped_capture: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonaExport {
    pub name: String,
    pub value: String,
}

pub fn run_list(args: &PersonaListArgs) -> Result<String, PersonaRegistryError> {
    let root = RegistryPaths::resolve()?;
    let report = list_report(&root)?;
    if args.wants_json_output() {
        return render_json(&report);
    }
    Ok(render_list_human(&report))
}

pub fn run_create(args: &PersonaCreateArgs) -> Result<String, PersonaRegistryError> {
    let root = RegistryPaths::resolve()?;
    let name = validate_name(&args.name)?;
    let (persona, created) =
        create_persona(&root, &name, args.description.clone(), args.if_not_exists)?;
    let report = PersonaCreateReport {
        kind: "soma_persona_created",
        created,
        usage: usage_lines(&name),
        persona,
    };
    if args.json {
        return render_json(&report);
    }
    Ok(render_create_human(&report))
}

pub fn run_call(args: &PersonaCallArgs) -> Result<String, PersonaRegistryError> {
    let root = RegistryPaths::resolve()?;
    let name = validate_name(&args.name)?;
    let persona = if args.create {
        create_persona(&root, &name, None, true)?.0
    } else {
        load_persona(&root, &name)?
    };
    let shell = resolve_shell(args.shell);
    let mut exports = exports_for_persona(&persona);
    let session_scope = build_session_scope(args)?;
    if let Some(scope) = &session_scope {
        exports.extend(exports_for_session_scope(scope));
    }
    let report = PersonaCallReport {
        kind: "soma_persona_call",
        shell: shell.as_str(),
        exports: exports.clone(),
        usage: usage_lines(&persona.name),
        session_scope,
        persona,
    };
    if args.json {
        return render_json(&report);
    }
    Ok(render_exports(shell, &exports))
}

fn list_report(root: &RegistryPaths) -> Result<PersonaListReport, PersonaRegistryError> {
    let mut personas = vec![default_persona(root)?];
    let personas_dir = root.personas_dir();
    if personas_dir.exists() {
        for entry in std::fs::read_dir(&personas_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let info = load_named_persona(root, name)
                .or_else(|err| invalid_persona(root, name, err.to_string()))?;
            personas.push(info);
        }
    }
    personas.sort_by(|a, b| {
        (if a.built_in_default { 0 } else { 1 }, a.name.as_str())
            .cmp(&(if b.built_in_default { 0 } else { 1 }, b.name.as_str()))
    });
    Ok(PersonaListReport {
        kind: "soma_persona_list",
        active_persona: active_persona_name(),
        personas_dir: personas_dir.display().to_string(),
        personas,
    })
}

fn create_persona(
    root: &RegistryPaths,
    name: &str,
    description: Option<String>,
    if_not_exists: bool,
) -> Result<(PersonaInfo, bool), PersonaRegistryError> {
    if name == DEFAULT_PERSONA {
        let info = default_persona(root)?;
        if !info.exists {
            let _ = Storage::open(Path::new(&info.db_path))?;
        }
        return Ok((default_persona(root)?, !info.exists));
    }

    let home = root.persona_home(name);
    let metadata_path = home.join(METADATA_FILE);
    let db_path = home.join(DB_FILE);
    let existed = metadata_path.exists();
    if existed && !if_not_exists {
        return Err(PersonaRegistryError::AlreadyExists(name.to_string()));
    }

    std::fs::create_dir_all(&home)?;
    let now = now_ns();
    let metadata = if existed {
        let mut metadata = read_metadata(&metadata_path)?;
        if description.is_some() {
            metadata.description = description;
        }
        metadata.updated_at_ns = now;
        metadata
    } else {
        PersonaMetadata {
            schema_version: 1,
            name: name.to_string(),
            description,
            created_at_ns: now,
            updated_at_ns: now,
        }
    };
    let _ = Storage::open(&db_path)?;
    write_metadata(&metadata_path, &metadata)?;
    Ok((persona_info(root, name, Some(metadata), false)?, !existed))
}

fn load_persona(root: &RegistryPaths, name: &str) -> Result<PersonaInfo, PersonaRegistryError> {
    if name == DEFAULT_PERSONA {
        return default_persona(root);
    }
    load_named_persona(root, name)
}

fn load_named_persona(
    root: &RegistryPaths,
    name: &str,
) -> Result<PersonaInfo, PersonaRegistryError> {
    let home = root.persona_home(name);
    let metadata_path = home.join(METADATA_FILE);
    if !metadata_path.exists() {
        return Err(PersonaRegistryError::NotFound(name.to_string()));
    }
    let metadata = read_metadata(&metadata_path)?;
    persona_info(root, name, Some(metadata), false)
}

fn default_persona(root: &RegistryPaths) -> Result<PersonaInfo, PersonaRegistryError> {
    persona_info(root, DEFAULT_PERSONA, None, true)
}

fn invalid_persona(
    root: &RegistryPaths,
    name: &str,
    error: String,
) -> Result<PersonaInfo, PersonaRegistryError> {
    let home = root.persona_home(name);
    let db_path = home.join(DB_FILE);
    Ok(PersonaInfo {
        name: name.to_string(),
        home: home.display().to_string(),
        db_path: db_path.display().to_string(),
        exists: db_path.exists(),
        active: is_active(name, &db_path),
        built_in_default: false,
        invalid: true,
        description: None,
        created_at_ns: None,
        updated_at_ns: None,
        error: Some(error),
    })
}

fn persona_info(
    root: &RegistryPaths,
    name: &str,
    metadata: Option<PersonaMetadata>,
    built_in_default: bool,
) -> Result<PersonaInfo, PersonaRegistryError> {
    let home = if built_in_default { root.soma_home.clone() } else { root.persona_home(name) };
    let db_path = home.join(DB_FILE);
    let active = is_active(name, &db_path);
    Ok(PersonaInfo {
        name: metadata.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| name.to_string()),
        home: home.display().to_string(),
        db_path: db_path.display().to_string(),
        exists: db_path.exists(),
        active,
        built_in_default,
        invalid: false,
        description: metadata.as_ref().and_then(|m| m.description.clone()),
        created_at_ns: metadata.as_ref().map(|m| m.created_at_ns),
        updated_at_ns: metadata.as_ref().map(|m| m.updated_at_ns),
        error: None,
    })
}

fn read_metadata(path: &Path) -> Result<PersonaMetadata, PersonaRegistryError> {
    let text = std::fs::read_to_string(path)?;
    let metadata: PersonaMetadata = serde_json::from_str(&text)?;
    validate_name(&metadata.name)?;
    Ok(metadata)
}

fn write_metadata(path: &Path, metadata: &PersonaMetadata) -> Result<(), PersonaRegistryError> {
    let text = serde_json::to_string_pretty(metadata)?;
    std::fs::write(path, format!("{text}\n"))?;
    Ok(())
}

fn validate_name(name: &str) -> Result<String, PersonaRegistryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(PersonaRegistryError::InvalidName("name must be non-empty".to_string()));
    }
    if name == "." || name == ".." {
        return Err(PersonaRegistryError::InvalidName("`.` and `..` are reserved".to_string()));
    }
    if name.starts_with('.') {
        return Err(PersonaRegistryError::InvalidName("names must not start with `.`".to_string()));
    }
    if name.chars().count() > 80 {
        return Err(PersonaRegistryError::InvalidName(
            "name must be 80 characters or fewer".to_string(),
        ));
    }
    if name.chars().any(|ch| ch == '/' || ch == '\\' || ch == ':' || ch == '\0' || ch.is_control())
    {
        return Err(PersonaRegistryError::InvalidName(
            "names must not contain path separators, colons, NUL, or control characters"
                .to_string(),
        ));
    }
    Ok(name.to_string())
}

#[derive(Debug, Clone)]
struct RegistryPaths {
    soma_home: PathBuf,
    personas_dir_override: Option<PathBuf>,
}

impl RegistryPaths {
    fn resolve() -> Result<Self, PersonaRegistryError> {
        let home = dirs::home_dir()
            .ok_or_else(|| PersonaRegistryError::Path("home directory not resolvable".into()))?;
        let personas_dir_override = std::env::var_os(PERSONAS_DIR_ENV)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty());
        Ok(Self { soma_home: home.join(".soma"), personas_dir_override })
    }

    fn personas_dir(&self) -> PathBuf {
        self.personas_dir_override.clone().unwrap_or_else(|| self.soma_home.join("personas"))
    }

    fn persona_home(&self, name: &str) -> PathBuf {
        self.personas_dir().join(name)
    }
}

fn exports_for_persona(persona: &PersonaInfo) -> Vec<PersonaExport> {
    let adapter_jsonl = PathBuf::from(&persona.home).join("adapter").join("events.jsonl");
    let adapter_checkpoint = PathBuf::from(&persona.home).join("adapter").join("events.offset");
    let mut exports = Vec::new();
    if let Some(personas_dir) = env_nonempty(PERSONAS_DIR_ENV) {
        exports.push(PersonaExport { name: PERSONAS_DIR_ENV.to_string(), value: personas_dir });
    }
    exports.extend([
        PersonaExport { name: PERSONA_ENV.to_string(), value: persona.name.clone() },
        PersonaExport { name: PERSONA_HOME_ENV.to_string(), value: persona.home.clone() },
        PersonaExport { name: DB_ENV.to_string(), value: persona.db_path.clone() },
        PersonaExport {
            name: ADAPTER_SPOOL_JSONL_ENV.to_string(),
            value: adapter_jsonl.display().to_string(),
        },
        PersonaExport {
            name: ADAPTER_SPOOL_CHECKPOINT_ENV.to_string(),
            value: adapter_checkpoint.display().to_string(),
        },
        PersonaExport {
            name: ADAPTER_LIFECYCLE_JSONL_ENV.to_string(),
            value: adapter_jsonl.display().to_string(),
        },
        PersonaExport {
            name: CURSOR_EVENT_JSONL_ENV.to_string(),
            value: adapter_jsonl.display().to_string(),
        },
        PersonaExport {
            name: CURSOR_EVENT_CHECKPOINT_ENV.to_string(),
            value: adapter_checkpoint.display().to_string(),
        },
    ]);
    exports
}

fn build_session_scope(
    args: &PersonaCallArgs,
) -> Result<Option<PersonaSessionScope>, PersonaRegistryError> {
    let wants_session = args.client.as_ref().is_some_and(|value| !value.trim().is_empty())
        || args.project.as_ref().is_some_and(|value| !value.trim().is_empty())
        || args.session_id.as_ref().is_some_and(|value| !value.trim().is_empty())
        || args.thread_key.as_ref().is_some_and(|value| !value.trim().is_empty());
    if !wants_session {
        return Ok(None);
    }

    let client = args
        .client
        .as_deref()
        .and_then(nonempty_owned)
        .or_else(|| env_nonempty(CLIENT_ENV))
        .unwrap_or_else(|| "terminal".to_string());
    let client = validate_client(&client)?;
    let project = args
        .project
        .as_deref()
        .and_then(nonempty_owned)
        .or_else(|| env_nonempty(PROJECT_ENV))
        .or_else(crate::project::current_name);
    let session_id = args.session_id.as_deref().and_then(nonempty_owned).unwrap_or_else(|| {
        crate::storage::session::managed_cli(
            &client,
            project.as_deref(),
            now_ns(),
            std::process::id(),
        )
    });
    let thread_key =
        args.thread_key.as_deref().and_then(nonempty_owned).or_else(|| env_nonempty(THREAD_ENV));
    let scope = PersonaSessionScope {
        ready_for_project_scoped_capture: !session_id.trim().is_empty()
            && !client.trim().is_empty()
            && project.as_ref().is_some_and(|value| !value.trim().is_empty()),
        scope_status: if !session_id.trim().is_empty()
            && !client.trim().is_empty()
            && project.as_ref().is_some_and(|value| !value.trim().is_empty())
        {
            "ready_for_project_scoped_capture"
        } else {
            "capture_scope_incomplete"
        },
        session_id,
        client,
        project,
        thread_key,
    };
    Ok(Some(scope))
}

fn exports_for_session_scope(scope: &PersonaSessionScope) -> Vec<PersonaExport> {
    let mut exports = vec![
        PersonaExport { name: SESSION_ENV.to_string(), value: scope.session_id.clone() },
        PersonaExport { name: CLIENT_ENV.to_string(), value: scope.client.clone() },
    ];
    if let Some(project) = &scope.project {
        exports.push(PersonaExport { name: PROJECT_ENV.to_string(), value: project.clone() });
    }
    if let Some(thread_key) = &scope.thread_key {
        exports.push(PersonaExport { name: THREAD_ENV.to_string(), value: thread_key.clone() });
    }
    for (alias, canonical) in SESSION_COMPAT_ENVS {
        let value = match *canonical {
            CLIENT_ENV => Some(scope.client.clone()),
            PROJECT_ENV => scope.project.clone(),
            SESSION_ENV => Some(scope.session_id.clone()),
            _ => None,
        };
        if let Some(value) = value {
            exports.push(PersonaExport { name: (*alias).to_string(), value });
        }
    }
    exports
}

fn validate_client(client: &str) -> Result<String, PersonaRegistryError> {
    let client = client.trim();
    if client.is_empty() {
        return Err(PersonaRegistryError::InvalidScope("--client must be non-empty".to_string()));
    }
    client
        .parse::<EpisodeSource>()
        .map(|source| source.to_string())
        .map_err(|err| PersonaRegistryError::InvalidScope(err.to_string()))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| nonempty_owned(&value))
}

fn nonempty_owned(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn active_persona_name() -> Option<String> {
    std::env::var(PERSONA_ENV).ok().filter(|value| !value.trim().is_empty())
}

fn is_active(name: &str, db_path: &Path) -> bool {
    if std::env::var(PERSONA_ENV).ok().as_deref() == Some(name) {
        return true;
    }
    match std::env::var(DB_ENV).ok().filter(|value| !value.trim().is_empty()) {
        Some(value) => PathBuf::from(value) == db_path,
        None => name == DEFAULT_PERSONA && active_persona_name().is_none(),
    }
}

fn usage_lines(name: &str) -> Vec<String> {
    vec![
        format!("eval \"$(soma call {name})\""),
        format!("eval \"$(soma call {name} --client codex-cli --project <project>)\""),
        format!("eval \"$(soma activate {name})\""),
        "soma list".to_string(),
    ]
}

fn render_json<T: Serialize>(value: &T) -> Result<String, PersonaRegistryError> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(text)
}

fn render_list_human(report: &PersonaListReport) -> String {
    let mut out = String::new();
    out.push_str("SOMA personas\n");
    for persona in &report.personas {
        let marker = if persona.active { "*" } else { " " };
        let exists = if persona.invalid {
            "invalid"
        } else if persona.exists {
            "ready"
        } else {
            "empty"
        };
        let default = if persona.built_in_default { " default" } else { "" };
        let description =
            persona.description.as_ref().map(|d| format!(" - {d}")).unwrap_or_default();
        out.push_str(&format!(
            "{marker} {:<20} {exists}{default} {}\n",
            persona.name, persona.db_path
        ));
        if !description.is_empty() {
            out.push_str(&format!("  {description}\n"));
        }
        if let Some(error) = &persona.error {
            out.push_str(&format!("  error: {error}\n"));
        }
    }
    out
}

fn render_create_human(report: &PersonaCreateReport) -> String {
    let action = if report.created { "created" } else { "ready" };
    format!(
        "soma: persona `{}` {action}\n  home: {}\n  db: {}\n  activate: eval \"$(soma call {})\"\n",
        report.persona.name, report.persona.home, report.persona.db_path, report.persona.name
    )
}

fn render_exports(shell: ShellFlavor, exports: &[PersonaExport]) -> String {
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

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_name_rejects_path_segments() {
        assert!(validate_name("research").is_ok());
        assert!(validate_name("연구").is_ok());
        assert!(validate_name("../x").is_err());
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("bad/name").is_err());
    }

    #[test]
    fn call_renders_db_exports() {
        let tmp = TempDir::new().expect("tempdir");
        let root =
            RegistryPaths { soma_home: tmp.path().join(".soma"), personas_dir_override: None };
        let (persona, created) = create_persona(&root, "research", None, false).expect("create");
        assert!(created);
        let out = render_exports(ShellFlavor::Posix, &exports_for_persona(&persona));
        assert!(out.contains("export SOMA_PERSONA='research';"));
        assert!(out.contains(".soma/personas/research/soma.db"));
        assert!(out.contains("export SOMA_ADAPTER_SPOOL_JSONL='"));
        assert!(out.contains(".soma/personas/research/adapter/events.jsonl"));
    }

    #[test]
    fn list_includes_default_and_named_persona() {
        let tmp = TempDir::new().expect("tempdir");
        let root =
            RegistryPaths { soma_home: tmp.path().join(".soma"), personas_dir_override: None };
        create_persona(&root, "planner", Some("planning persona".to_string()), false)
            .expect("create");
        let report = list_report(&root).expect("list");
        assert!(report.personas.iter().any(|p| p.name == "default"));
        assert!(report.personas.iter().any(|p| p.name == "planner"));
    }

    #[test]
    fn list_surfaces_invalid_persona_metadata() {
        let tmp = TempDir::new().expect("tempdir");
        let root =
            RegistryPaths { soma_home: tmp.path().join(".soma"), personas_dir_override: None };
        let broken = root.persona_home("broken");
        std::fs::create_dir_all(&broken).expect("broken dir");
        std::fs::write(broken.join(METADATA_FILE), "{not json").expect("broken metadata");

        let report = list_report(&root).expect("list");
        let broken = report.personas.iter().find(|p| p.name == "broken").expect("broken row");
        assert!(broken.invalid);
        assert!(broken.error.as_deref().is_some_and(|err| err.contains("json")));
    }
}
