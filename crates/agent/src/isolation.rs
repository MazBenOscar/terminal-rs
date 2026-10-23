use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use std::process::Command;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::transcript::Transcript;
use crate::transport::{ToolHandler, Transport};

pub const FEATURE_REGISTRY: &str = include_str!("../schema/codex-0.163.0-features.txt");
pub const ALLOWED_FEATURES: &[&str] = &[
    "secret_auth_storage",
    "item_ids",
    "steer",
    "instant_interrupt",
    "content_item_kinds",
    "sqlite",
    "enable_request_compression",
];

/// The stage column can contain spaces. Reject unknown stages, duplicates and
/// missing rows rather than accepting a partial listing as startup verification.
pub fn parse_features(output: &str) -> Result<BTreeMap<String, bool>> {
    let mut features = BTreeMap::new();
    for line in output.lines() {
        let columns: Vec<_> = line.split_whitespace().collect();
        if columns.len() < 3 {
            bail!("invalid feature listing");
        }
        let name = columns[0];
        let stage = columns[1..columns.len() - 1].join(" ");
        if !matches!(stage.as_str(), "stable" | "experimental" | "under development" | "removed" | "deprecated") {
            bail!("invalid feature stage");
        }
        let enabled = match columns.last().copied() {
            Some("true") => true,
            Some("false") => false,
            _ => bail!("invalid feature state"),
        };
        if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
            || features.insert(name.to_owned(), enabled).is_some()
        {
            bail!("invalid or duplicate feature name");
        }
    }
    if features.is_empty() {
        bail!("empty feature listing");
    }
    Ok(features)
}

/// Enabled in 0.163.0 even with `--disable`/`-c features.<name>=false` (verified live
/// 2026-10-23). `unified_exec` only backs the shell tool, which is disabled; Seatbelt
/// allows exec of codex alone and the tripwire kills the session on any command item.
/// The canary turn re-proves this per version. Adding a name here needs review.
pub const UNDISABLEABLE_FEATURES: &[&str] = &["unified_exec"];

/// Features the registry lists at stage `removed`: retired, report `true`, do nothing.
fn removed_features() -> std::collections::BTreeSet<&'static str> {
    FEATURE_REGISTRY
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            words.any(|word| word == "removed").then_some(name)
        })
        .collect()
}

pub fn verify_features(output: &str) -> Result<()> {
    let actual = parse_features(output)?;
    let registry = parse_features(FEATURE_REGISTRY)?;
    if actual.keys().ne(registry.keys()) {
        bail!("feature registry changed");
    }
    let unapproved: Vec<_> = actual
        .iter()
        .filter(|(name, enabled)| {
            **enabled
                && !ALLOWED_FEATURES.contains(&name.as_str())
                && !UNDISABLEABLE_FEATURES.contains(&name.as_str())
                && !removed_features().contains(name.as_str())
        })
        .map(|(name, _)| name.as_str())
        .collect();
    if !unapproved.is_empty() {
        bail!("unapproved feature enabled: {}", unapproved.join(", "));
    }
    let missing: Vec<_> = ALLOWED_FEATURES.iter().filter(|name| actual.get(**name) != Some(&true)).copied().collect();
    if !missing.is_empty() {
        bail!("required feature disabled: {}", missing.join(", "));
    }
    Ok(())
}

pub fn feature_argv() -> Vec<OsString> {
    let mut args = Vec::new();
    for (name, default) in parse_features(FEATURE_REGISTRY).expect("committed feature registry") {
        if !ALLOWED_FEATURES.contains(&name.as_str()) {
            args.extend(["--disable".into(), name.into()]);
        } else if !default {
            args.extend(["--enable".into(), name.into()]);
        }
    }
    args
}

// Config and CLI feature names are not described by the app-server schema. Live verification required.
pub const CONFIG: &str = "sandbox_mode = \"read-only\"\nweb_search = \"disabled\"\nforced_login_method = \"chatgpt\"\ncli_auth_credentials_store = \"keyring\"\n\n[features]\nsecret_auth_storage = true\n\n[[skills.config]]\nname = \"imagegen\"\nenabled = false\n\n[[skills.config]]\nname = \"openai-docs\"\nenabled = false\n\n[[skills.config]]\nname = \"review-agent\"\nenabled = false\n\n[[skills.config]]\nname = \"skill-creator\"\nenabled = false\n\n[[skills.config]]\nname = \"skill-installer\"\nenabled = false\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

#[derive(Clone, Copy, Debug)]
pub struct VersionRange {
    pub min: Version,
    pub max_exclusive: Version,
}

impl Default for VersionRange {
    fn default() -> Self {
        Self { min: Version(0, 163, 0), max_exclusive: Version(0, 163, 1) }
    }
}

impl VersionRange {
    pub fn check(&self, output: &str) -> Result<Version> {
        let version = output.trim().strip_prefix("codex-cli ").context("unrecognized Codex version")?;
        let parts = version.split('.').map(str::parse::<u32>).collect::<std::result::Result<Vec<_>, _>>()?;
        let [major, minor, patch] = parts.as_slice() else { bail!("unrecognized Codex version") };
        if version != format!("{major}.{minor}.{patch}") {
            bail!("non-canonical Codex version");
        }
        let version = Version(*major, *minor, *patch);
        if version < self.min || version >= self.max_exclusive {
            bail!("unsupported Codex version");
        }
        Ok(version)
    }
}

pub fn scrub_environment(
    source: impl IntoIterator<Item = (OsString, OsString)>,
    private_home: &Path,
) -> BTreeMap<OsString, OsString> {
    let mut result = BTreeMap::new();
    for (key, value) in source {
        let name = key.to_string_lossy();
        let upper = name.to_ascii_uppercase();
        if (matches!(name.as_ref(), "PATH" | "HOME" | "TMPDIR" | "LANG") || name.starts_with("LC_"))
            && !upper.contains("TOKEN")
            && !upper.ends_with("_API_KEY")
        {
            result.insert(key, value);
        }
    }
    result.insert("CODEX_HOME".into(), private_home.as_os_str().to_owned());
    result
}

pub struct PrivateHome {
    pub path: PathBuf,
    pub cwd: PathBuf,
}

#[cfg(unix)]
fn owned_path(path: &Path) -> Result<std::fs::Metadata> {
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    // A private home must not be swappable by another local user.
    if metadata.uid() != unsafe { geteuid() } || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink() {
        bail!("Codex home paths must be owned, non-symlink, and not writable by others");
    }
    Ok(metadata)
}

impl PrivateHome {
    /// The supplied parent must be owned by the app/user, not shared or attacker-writable.
    pub fn create(parent: &Path) -> Result<Self> {
        #[cfg(not(unix))]
        bail!("private Codex home requires Unix permissions");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            if !owned_path(parent)?.is_dir() {
                bail!("private home parent is not a directory");
            }
            let path = parent.join("codex-agent");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .context("private home must be new; use PrivateHome::open for an existing login")?;
            let cwd = path.join("empty");
            std::fs::DirBuilder::new().mode(0o700).create(&cwd)?;
            std::fs::DirBuilder::new().mode(0o700).create(path.join("home"))?;
            let mut file =
                std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path.join("config.toml"))?;
            use std::io::Write;
            file.write_all(CONFIG.as_bytes())?;
            Ok(Self { path: path.canonicalize()?, cwd: cwd.canonicalize()? })
        }
    }

    pub fn open(parent: &Path) -> Result<Self> {
        #[cfg(not(unix))]
        bail!("private Codex home requires Unix permissions");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !owned_path(parent)?.is_dir() {
                bail!("private home parent is not a directory");
            }
            let path = parent.join("codex-agent");
            let metadata = owned_path(&path)?;
            if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
                bail!("private home must be a real 0700 directory");
            }
            validate_home_entries(&path, true)?;
            if !path.join("home").exists() {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(path.join("home"))?;
            }
            let config = path.join("config.toml");
            let config_metadata = owned_path(&config)?;
            if !config_metadata.is_file()
                || config_metadata.permissions().mode() & 0o777 != 0o600
                || std::fs::read_to_string(config)? != CONFIG
            {
                bail!("private home config changed");
            }
            let cwd = path.join("empty");
            let cwd_metadata = owned_path(&cwd)?;
            if !cwd_metadata.is_dir()
                || cwd_metadata.permissions().mode() & 0o777 != 0o700
                || std::fs::read_dir(&cwd)?.next().is_some()
            {
                bail!("private working directory must be empty");
            }
            Ok(Self { path: path.canonicalize()?, cwd: cwd.canonicalize()? })
        }
    }
}

#[cfg(unix)]
fn validate_home_entries(path: &Path, root: bool) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("non-UTF8 private home entry")?;
        let metadata = owned_path(&entry.path())?;
        let directory = metadata.is_dir();
        if root && name == "tmp" && directory {
            validate_arg0_links(&entry.path())?;
            continue;
        }
        if root && name == "skills" && directory {
            validate_system_skills(&entry.path())?;
            continue;
        }
        if root && name == "home" && directory {
            // The private HOME must stay empty: anything Codex or a session leaves there
            // (dotfiles, AGENTS.md, skills) is refused until reviewed.
            if std::fs::read_dir(entry.path())?.next().is_some() {
                bail!("private HOME is not empty; refusing startup");
            }
            continue;
        }
        let allowed = if root {
            if directory {
                matches!(name, "empty" | "sessions" | "log" | "logs")
            } else {
                matches!(
                    name,
                    "config.toml"
                        | "installation_id"
                        | "goals_1.sqlite"
                        | "goals_1.sqlite-shm"
                        | "goals_1.sqlite-wal"
                        | "memories_1.sqlite"
                        | "memories_1.sqlite-shm"
                        | "memories_1.sqlite-wal"
                        | "queue_1.sqlite"
                        | "queue_1.sqlite-shm"
                        | "queue_1.sqlite-wal"
                        | "auth.json"
                        | "version.json"
                        | "models_cache.json"
                        | "state_5.sqlite"
                        | "state_5.sqlite-shm"
                        | "state_5.sqlite-wal"
                        | "logs_2.sqlite"
                        | "logs_2.sqlite-shm"
                        | "logs_2.sqlite-wal"
                        | ".codex-global-state.json"
                )
            }
        } else if directory {
            // Only dated session storage, not arbitrary extension directories.
            matches!(name.len(), 2 | 4) && name.bytes().all(|byte| byte.is_ascii_digit())
        } else {
            (name.starts_with("rollout-") && name.ends_with(".jsonl"))
                || matches!(name, "codex-tui.log" | "codex-app-server.log" | "codex.log")
        };
        if !allowed || (!directory && !metadata.is_file()) {
            bail!(
                "private home contains an unknown or instruction/extension entry; refusing startup (entry names omitted)"
            );
        }
        if directory {
            validate_home_entries(&entry.path(), false)?;
        }
    }
    Ok(())
}

/// Codex installs its bundled skills under `skills/.system` on every start. Accept only
/// that tree (regular files and directories, no links); the config disables each skill and
/// the client verifies with `skills/list` that none is enabled.
#[cfg(unix)]
fn validate_system_skills(skills: &Path) -> Result<()> {
    let refuse = || anyhow::anyhow!("private home skills contain non-bundled entries; refusing startup");
    for entry in std::fs::read_dir(skills)? {
        let entry = entry?;
        if entry.file_name() != ".system" || !std::fs::symlink_metadata(entry.path())?.is_dir() {
            return Err(refuse());
        }
        let mut stack = vec![entry.path()];
        while let Some(dir) = stack.pop() {
            for item in std::fs::read_dir(dir)? {
                let item = item?;
                let kind = std::fs::symlink_metadata(item.path())?.file_type();
                if kind.is_dir() {
                    stack.push(item.path());
                } else if !kind.is_file() {
                    return Err(refuse());
                }
            }
        }
    }
    Ok(())
}

/// Codex re-execs itself through `tmp/arg0-macos/codex-arg0*/{apply_patch,applypatch}`
/// symlinks created at startup. Accept exactly that shape: lock files and symlinks to an
/// absolute `codex` binary; anything else fails closed.
#[cfg(unix)]
fn validate_arg0_links(tmp: &Path) -> Result<()> {
    let refuse = || anyhow::anyhow!("private home tmp contains unexpected entries; refusing startup");
    for entry in std::fs::read_dir(tmp)? {
        let entry = entry?;
        if entry.file_name() != "arg0-macos" || !owned_path(&entry.path())?.is_dir() {
            return Err(refuse());
        }
        for dir in std::fs::read_dir(entry.path())? {
            let dir = dir?;
            let name = dir.file_name();
            if !name.to_str().is_some_and(|n| n.starts_with("codex-arg0")) || !owned_path(&dir.path())?.is_dir() {
                return Err(refuse());
            }
            for item in std::fs::read_dir(dir.path())? {
                let item = item?;
                let metadata = std::fs::symlink_metadata(item.path())?;
                let ok = match item.file_name().to_str() {
                    Some(".lock") => metadata.is_file(),
                    Some("apply_patch" | "applypatch") if metadata.file_type().is_symlink() => {
                        let target = std::fs::read_link(item.path())?;
                        target.is_absolute() && target.file_name().is_some_and(|n| n == "codex")
                    }
                    _ => false,
                };
                if !ok {
                    return Err(refuse());
                }
            }
        }
    }
    Ok(())
}

impl PrivateHome {
    pub fn recheck(&self) -> Result<()> {
        let parent = self.path.parent().context("private home has no parent")?;
        let checked = Self::open(parent)?;
        if checked.path != self.path || checked.cwd != self.cwd {
            bail!("private home paths changed");
        }
        Ok(())
    }
}

fn quote(path: &Path) -> Result<String> {
    let text = path.to_str().context("sandbox paths must be UTF-8")?;
    if !path.is_absolute()
        || text.chars().any(char::is_control)
        || path.components().any(|part| matches!(part, std::path::Component::ParentDir | std::path::Component::CurDir))
    {
        bail!("invalid sandbox path");
    }
    Ok(format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"")))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionMode {
    #[default]
    Normal,
    /// OAuth callback port; default for the spike is 1455 (live UNVERIFIED).
    Login { callback_port: u16 },
}

pub fn sandbox_profile(private_home: &Path, binary: &Path, user_home: &Path) -> Result<String> {
    sandbox_profile_for(private_home, binary, user_home, SessionMode::Normal)
}

pub fn sandbox_profile_for(private_home: &Path, binary: &Path, user_home: &Path, mode: SessionMode) -> Result<String> {
    let install_path = binary.parent().context("binary has no install directory")?;
    for protected in [
        user_home.to_path_buf(),
        private_home.to_path_buf(),
        user_home.join(".ssh"),
        user_home.join(".codex"),
        user_home.join("Documents"),
    ] {
        if protected.starts_with(install_path) || matches!(install_path.to_str(), Some("/" | "/Users")) {
            bail!("Codex install directory would grant access to a protected directory");
        }
    }
    let callback = match mode {
        SessionMode::Normal => String::new(),
        SessionMode::Login { callback_port: 0 } => bail!("callback port must be nonzero"),
        SessionMode::Login { callback_port } => format!(
            "(allow network-inbound (local tcp \"localhost:{callback_port}\"))\n(allow network-bind (local tcp \"localhost:{callback_port}\"))\n"
        ),
    };
    let home = quote(private_home)?;
    // Codex canonicalizes CODEX_HOME, which needs metadata (not contents) of each ancestor.
    let ancestors = private_home
        .ancestors()
        .skip(1)
        .map(|dir| quote(dir).map(|q| format!("(literal {q})")))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let executable = quote(binary)?;
    let ssh = quote(&user_home.join(".ssh"))?;
    let codex = quote(&user_home.join(".codex"))?;
    let documents = quote(&user_home.join("Documents"))?;
    Ok(format!(
        r#"(version 1)
(deny default)
(allow file-read* (subpath {home}) (literal {executable})
    (subpath "/System/Library") (subpath "/usr/lib") (subpath "/Library/Apple/System/Library")
    (literal "/") (literal "/dev/urandom") (literal "/dev/null"))
(allow file-read-metadata {ancestors} (literal "/etc") (literal "/private/etc") (literal "/private/etc/codex"))
(allow file-read* (literal "/private/etc/codex/requirements.toml") (literal "/private/etc/codex/managed_config.toml"))
(allow file-write* (subpath {home}))
(allow process-exec (literal {executable}))
(allow process-fork)
(allow sysctl-read)
(allow mach-lookup (global-name "com.apple.dnssd.service"))
(allow mach-lookup (global-name "com.apple.cfprefsd.daemon") (global-name "com.apple.cfprefsd.agent"))
(allow user-preference-read)
(allow ipc-posix-shm-read-data (ipc-posix-name-prefix "apple.cfprefs."))
(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo") (global-name "com.apple.system.notification_center"))
(allow mach-lookup (global-name "com.apple.SecurityServer"))
(allow mach-lookup (global-name "com.apple.securityd"))
(allow mach-lookup (global-name "com.apple.trustd"))
(allow mach-lookup (global-name "com.apple.trustd.agent"))
(allow network-outbound (remote tcp "*:443"))
{callback}(deny file-read* file-write* (subpath {ssh}) (subpath {codex}) (subpath {documents}))
"#
    ))
}

pub fn app_server_argv(profile: &str, binary: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> =
        vec!["-p".into(), profile.into(), binary.as_os_str().to_owned(), "--strict-config".into()];
    args.extend(feature_argv());
    args.push("app-server".into());
    args
}

/// Compute descendants even when they have escaped the original process group.
pub fn descendant_tree(table: &str, root: u32) -> Result<std::collections::BTreeSet<u32>> {
    let rows = parse_ps(table)?;
    let mut found = std::collections::BTreeSet::from([root]);
    loop {
        let before = found.len();
        for &(pid, parent, group) in &rows {
            if found.contains(&parent) || group == root {
                found.insert(pid);
            }
        }
        if found.len() == before {
            break;
        }
    }
    found.remove(&root);
    Ok(found)
}

fn parse_ps(table: &str) -> Result<Vec<(u32, u32, u32)>> {
    table
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split_whitespace().map(str::parse::<u32>).collect::<std::result::Result<Vec<_>, _>>()?;
            let [pid, parent, group] = fields.as_slice() else {
                bail!("invalid process table");
            };
            Ok((*pid, *parent, *group))
        })
        .collect()
}

fn process_table() -> Result<String> {
    let output = Command::new("/bin/ps").args(["-A", "-o", "pid=,ppid=,pgid="]).env_clear().output()?;
    if !output.status.success() {
        bail!("process enumeration failed");
    }
    Ok(String::from_utf8(output.stdout)?)
}

struct ProcessGroup {
    pid: std::sync::atomic::AtomicU32,
    descendants: std::sync::Mutex<std::collections::BTreeSet<u32>>,
    findings: std::sync::Mutex<Vec<String>>,
}
impl ProcessGroup {
    fn new(pid: u32) -> Self {
        Self {
            pid: std::sync::atomic::AtomicU32::new(pid),
            descendants: Default::default(),
            findings: Default::default(),
        }
    }
    fn refresh(&self) {
        let pid = self.pid.load(std::sync::atomic::Ordering::SeqCst);
        if pid == 0 {
            return;
        }
        let scan = || -> Result<()> {
            let table = process_table()?;
            let rows = parse_ps(&table)?;
            let mut tracked = self.descendants.lock().unwrap();
            // Forget exited pids so a later scan cannot target a reused pid.
            tracked.retain(|pid| rows.iter().any(|row| row.0 == *pid));
            let anchors: Vec<_> = std::iter::once(pid).chain(tracked.iter().copied()).collect();
            for anchor in anchors {
                tracked.extend(descendant_tree(&table, anchor)?);
            }
            Ok(())
        };
        if scan().is_err() {
            let mut findings = self.findings.lock().unwrap();
            if findings.is_empty() {
                findings.push("descendant enumeration failed".into());
            }
        }
    }
    fn kill(&self) {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            self.refresh();
            let pid = self.pid.swap(0, std::sync::atomic::Ordering::SeqCst);
            if pid == 0 {
                return;
            }
            if let Ok(pid) = i32::try_from(pid) {
                unsafe {
                    kill(-pid, 9);
                }
            }
            let tracked = self.descendants.lock().unwrap();
            for &child in tracked.iter() {
                if let Ok(child) = i32::try_from(child)
                    && child > 1
                {
                    unsafe {
                        kill(child, 9);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
            match process_table().and_then(|table| parse_ps(&table)) {
                Ok(rows) => {
                    let survivors: Vec<_> =
                        rows.iter().filter(|(child, _, _)| tracked.contains(child)).map(|row| row.0).collect();
                    if !survivors.is_empty() {
                        self.findings
                            .lock()
                            .unwrap()
                            .push(format!("descendant survivors after shutdown: {survivors:?}"));
                    }
                }
                Err(_) => self.findings.lock().unwrap().push("cannot verify descendant cleanup".into()),
            }
        }
    }
    fn check(&self) -> Result<()> {
        let findings = self.findings.lock().unwrap();
        if !findings.is_empty() {
            bail!("{}", findings.join("; "));
        }
        Ok(())
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

fn isolated_command(program: &Path, home: &PrivateHome) -> Command {
    let mut command = Command::new(program);
    command.env_clear().envs(scrub_environment(std::env::vars_os(), &home.path)).current_dir(&home.cwd);
    // A private HOME keeps Codex from discovering the user's own skills, AGENTS.md and
    // agent configuration (it scans ~ for user-scope skills even with CODEX_HOME set).
    command.env("HOME", home.path.join("home"));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

#[derive(Debug)]
pub struct WrongVersion;
impl std::fmt::Display for WrongVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unsupported Codex version")
    }
}
impl std::error::Error for WrongVersion {}

struct KillOnDrop(Arc<ProcessGroup>);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.0.kill();
    }
}

async fn probe(binary: &Path, home: &PrivateHome, profile: &str, subcommand: &[&str]) -> Result<String> {
    let mut args = app_server_argv(profile, binary);
    args.pop();
    if subcommand.first() == Some(&"features") {
        // `codex features` rejects --strict-config; it only reads the flags we pass.
        args.retain(|arg| arg != "--strict-config");
    }
    args.extend(subcommand.iter().map(OsString::from));
    let mut command = isolated_command(Path::new("/usr/bin/sandbox-exec"), home);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = command.spawn()?;
    let group = Arc::new(ProcessGroup::new(child.id()));
    let _guard = KillOnDrop(group.clone());
    let monitor = group.clone();
    let mut wait = tokio::task::spawn_blocking(move || {
        let output = child.wait_with_output();
        monitor.kill();
        output
    });
    let monitored_wait = async {
        loop {
            tokio::select! {
                result = &mut wait => break result,
                _ = tokio::time::sleep(Duration::from_millis(10)) => group.refresh(),
            }
        }
    };
    let output = match tokio::time::timeout(Duration::from_secs(5), monitored_wait).await {
        Ok(result) => {
            result.context("startup probe worker failed").and_then(|output| output.context("startup probe wait failed"))
        }
        Err(_) => {
            group.kill();
            Err(anyhow::anyhow!("Codex startup probe timed out"))
        }
    };
    // Inspect both, including failed/timed-out probes, before propagating errors.
    let process_result = group.check();
    let home_result = home.recheck();
    process_result?;
    home_result?;
    let output = output?;
    if !output.status.success() {
        // A scrubbed, bounded excerpt so a fail-closed probe still says why.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let excerpt: String = crate::transcript::scrub_text(stderr.trim()).chars().take(400).collect();
        bail!("Codex startup probe failed ({}): {excerpt}", output.status);
    }
    String::from_utf8(output.stdout).map_err(|_| anyhow::anyhow!("invalid startup probe output"))
}

pub async fn spawn(
    binary: &Path,
    home: &PrivateHome,
    user_home: &Path,
    range: VersionRange,
    handler: ToolHandler,
    transcript: Option<Transcript>,
) -> Result<Transport> {
    spawn_session(binary, home, user_home, range, SessionMode::Normal, handler, transcript).await
}

pub async fn spawn_session(
    binary: &Path,
    home: &PrivateHome,
    user_home: &Path,
    range: VersionRange,
    mode: SessionMode,
    handler: ToolHandler,
    transcript: Option<Transcript>,
) -> Result<Transport> {
    if !cfg!(target_os = "macos") {
        bail!("Codex isolation requires macOS sandbox-exec");
    }
    home.recheck()?;
    // Resolve exactly once, then use this path for both probes and the server.
    let binary = binary.canonicalize()?;
    let user_home = user_home.canonicalize()?;
    let profile = sandbox_profile_for(&home.path, &binary, &user_home, mode)?;
    let version = probe(&binary, home, &profile, &["--version"]).await?;
    range.check(&version).map_err(|_| WrongVersion)?;
    // No caller can widen the pinned protocol version through VersionRange.
    VersionRange::default().check(&version).map_err(|_| WrongVersion)?;
    verify_features(&probe(&binary, home, &profile, &["features", "list"]).await?)?;
    // Codex's own diagnostics go to a private 0600 log so failures stay explainable.
    let log = {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let dir = home.path.join("log");
        if !dir.exists() {
            std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        }
        std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(dir.join("codex-app-server.log"))?
    };
    let mut command = isolated_command(Path::new("/usr/bin/sandbox-exec"), home);
    command
        .args(app_server_argv(&profile, &binary))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(log);
    let mut child = command.spawn()?;
    let group = Arc::new(ProcessGroup::new(child.id()));
    let kill_group = group.clone();
    let stdout = child.stdout.take().context("missing stdout")?;
    let stdin = child.stdin.take().context("missing stdin")?;
    let (read, write) = stdio_bridge(stdout, stdin);
    let transport = Transport::connect(read, write, handler, Arc::new(move || kill_group.kill()), transcript);
    let check_group = group.clone();
    let final_home = PrivateHome { path: home.path.clone(), cwd: home.cwd.clone() };
    transport.set_shutdown_check(move || {
        let process_result = check_group.check();
        let home_result = final_home.recheck();
        process_result.and(home_result)
    });
    transport.set_login_only(matches!(mode, SessionMode::Login { .. }));
    let exited = transport.child_exited();
    let report = transport.cleanup_reporter();
    let closed = transport.closed();
    let checked_home = PrivateHome { path: home.path.clone(), cwd: home.cwd.clone() };
    std::thread::spawn(move || {
        loop {
            if closed.borrow().is_some() {
                group.kill();
                let _ = child.wait();
                break;
            }
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => {
                    exited();
                    group.kill();
                    let _ = child.wait();
                    break;
                }
                Ok(None) => {
                    group.refresh();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        if let Err(error) = group.check().and_then(|()| checked_home.recheck()) {
            report(format!("session cleanup findings: {error}"));
        }
    });
    Ok(transport)
}

type WriteAck = tokio::sync::oneshot::Receiver<std::io::Result<()>>;
type WritePacket = (Vec<u8>, tokio::sync::oneshot::Sender<std::io::Result<()>>);

struct ChildWriter {
    sender: std::sync::mpsc::SyncSender<WritePacket>,
    pending: Option<WriteAck>,
}

impl AsyncWrite for ChildWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        use std::task::Poll;
        match self.as_mut().poll_flush(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        if self.sender.try_send((bytes.to_vec(), sender)).is_err() {
            return Poll::Ready(Err(std::io::Error::other("child stdin bridge closed or overflowed")));
        }
        self.pending = Some(receiver);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::future::Future;
        use std::task::Poll;
        let Some(receiver) = self.pending.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match std::pin::Pin::new(receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.pending = None;
                Poll::Ready(result.unwrap_or_else(|_| Err(std::io::Error::other("child stdin worker stopped"))))
            }
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}

fn child_writer(mut stdin: impl std::io::Write + Send + 'static) -> ChildWriter {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<WritePacket>(1);
    std::thread::spawn(move || {
        for (bytes, ack) in receiver {
            let result = stdin.write_all(&bytes).and_then(|()| stdin.flush());
            let failed = result.is_err();
            let _ = ack.send(result);
            if failed {
                break;
            }
        }
    });
    ChildWriter { sender, pending: None }
}

fn stdio_bridge(
    mut stdout: std::process::ChildStdout,
    stdin: std::process::ChildStdin,
) -> (tokio::io::DuplexStream, ChildWriter) {
    let (read, mut incoming) = tokio::io::duplex(65536);
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buffer = [0; 8192];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if runtime.block_on(incoming.write_all(&buffer[..count])).is_err() {
                        break;
                    }
                }
            }
        }
    });
    (read, child_writer(stdin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tripwire_flushes_child_stdin_before_group_kill() {
        use crate::transport::tests::{no_tools, send};
        use std::sync::Mutex;
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let captured = bytes.clone();
        let (read, mut peer) = tokio::io::duplex(4096);
        let client = Transport::connect(
            read,
            child_writer(Capture(bytes)),
            no_tools(),
            Arc::new(move || {
                let output = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
                assert!(output.contains("decline"));
                assert!(output.contains("turn/interrupt"));
            }),
            None,
        );
        let mut events = client.events();
        send(&mut peer, serde_json::json!({"id":1,"method":"item/fileChange/requestApproval","params":{"threadId":"t","turnId":"u"}})).await;
        assert!(matches!(events.recv().await, Some(crate::transport::WireEvent::Error(_))));
    }

    #[cfg(unix)]
    #[test]
    fn private_home_permissions_and_config_are_enforced() {
        use std::os::unix::fs::PermissionsExt;
        let parent =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("private-home-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        let home = PrivateHome::create(&parent).unwrap();
        assert_eq!(std::fs::metadata(&home.path).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::read_to_string(home.path.join("config.toml")).unwrap(), CONFIG);
        assert!(PrivateHome::open(&parent).is_ok());
        std::fs::write(home.path.join("config.toml"), "notify = ['bad']").unwrap();
        assert!(PrivateHome::open(&parent).is_err());
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_home_allowlist_and_post_session_recheck() {
        use std::os::unix::fs::PermissionsExt;
        let parent =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("home-allowlist-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = PrivateHome::create(&parent).unwrap();
        for file in
            ["auth.json", "version.json", "models_cache.json", "state_5.sqlite", "state_5.sqlite-wal", "logs_2.sqlite"]
        {
            std::fs::write(home.path.join(file), "{}").unwrap();
        }
        std::fs::create_dir(home.path.join("sessions")).unwrap();
        std::fs::write(home.path.join("sessions/rollout-test.jsonl"), "{}").unwrap();
        home.recheck().unwrap();
        for entry in ["plugins", "prompts", "mcp.json", "unknown-cache", "AGENTS.md", "hooks.json"] {
            let path = home.path.join(entry);
            std::fs::write(&path, "bad").unwrap();
            assert!(home.recheck().is_err(), "{entry}");
            std::fs::remove_file(path).unwrap();
        }
        std::fs::write(home.path.join("sessions/AGENTS.md"), "bad").unwrap();
        assert!(home.recheck().is_err());
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn install_directory_cannot_cover_any_protected_tree() {
        let private = Path::new("/private/app/codex-agent");
        let user = Path::new("/Users/test");
        for binary in [
            "/codex",
            "/Users/codex",
            "/Users/test/codex",
            "/private/codex",
            "/private/app/codex",
            "/private/app/codex-agent/codex",
            "/Users/test/.ssh/codex",
            "/Users/test/.codex/codex",
            "/Users/test/Documents/codex",
        ] {
            assert!(sandbox_profile(private, Path::new(binary), user).is_err(), "{binary}");
        }
        assert!(sandbox_profile(private, Path::new("/opt/codex/bin/codex"), user).is_ok());
    }

    #[test]
    fn environment_is_allowlisted_not_inherited() {
        let source = [
            "PATH",
            "HOME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "LC_TOKEN",
            "LC_X_API_KEY",
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "SECRET",
            "CODEX_HOME",
        ]
        .map(|name| (name.into(), "secret".into()));
        let env = scrub_environment(source, Path::new("/private/agent"));
        assert_eq!(env.len(), 6);
        assert_eq!(env.get(&OsString::from("CODEX_HOME")).unwrap(), "/private/agent");
        assert!(!env.contains_key(&OsString::from("OPENAI_API_KEY")));
    }

    #[test]
    fn config_and_argv_disable_every_planned_feature() {
        assert!(CONFIG.contains("sandbox_mode = \"read-only\""));
        assert!(CONFIG.contains("web_search = \"disabled\""));
        assert!(CONFIG.contains("forced_login_method = \"chatgpt\""));
        assert!(CONFIG.contains("secret_auth_storage = true"));
        assert!(CONFIG.contains("cli_auth_credentials_store = \"keyring\""));
        assert!(!CONFIG.contains("mcp_servers"));
        assert!(!CONFIG.contains("notify"));
        let args = app_server_argv("profile", Path::new("/opt/codex"));
        assert_eq!(args[3], "--strict-config");
        let registry = parse_features(FEATURE_REGISTRY).unwrap();
        assert_eq!(registry.len(), 175);
        for (feature, default) in &registry {
            if !ALLOWED_FEATURES.contains(&feature.as_str()) {
                assert!(args.windows(2).any(|pair| pair[0] == "--disable" && pair[1] == feature.as_str()));
            } else if !default {
                assert!(args.windows(2).any(|pair| pair[0] == "--enable" && pair[1] == feature.as_str()));
            }
        }
        assert!(!args.iter().any(|arg| arg == "--analytics-default-enabled"));
        assert!(verify_features(FEATURE_REGISTRY).is_err());
        let effective = registry
            .keys()
            .map(|name| format!("{name} stable {}\n", ALLOWED_FEATURES.contains(&name.as_str())))
            .collect::<String>();
        verify_features(&effective).unwrap();
        assert!(verify_features(&effective.replace("fast_mode stable false", "fast_mode stable true")).is_err());
        for invalid in ["", "x true", "x bogus false", "x stable maybe", "x stable false\nx stable true"] {
            assert!(parse_features(invalid).is_err());
        }
        assert!(verify_features("item_ids removed true").is_err());
        assert_eq!(args.last().unwrap(), "app-server");
    }

    #[test]
    fn profile_denies_personal_secrets_and_documents() {
        let profile =
            sandbox_profile(Path::new("/private/agent"), Path::new("/opt/codex/bin/codex"), Path::new("/Users/test"))
                .unwrap();
        assert!(profile.contains("(deny default)"));
        for denied in ["/Users/test/.ssh", "/Users/test/.codex", "/Users/test/Documents"] {
            assert!(profile.lines().last().unwrap().contains(denied));
        }
        assert!(profile.contains("(allow process-exec (literal \"/opt/codex/bin/codex\"))"));
        assert!(profile.contains("*:443"));
        assert!(!profile.contains("network-inbound"));
        assert!(!profile.contains("network-bind"));
        let login = sandbox_profile_for(
            Path::new("/private/agent"),
            Path::new("/opt/codex/bin/codex"),
            Path::new("/Users/test"),
            SessionMode::Login { callback_port: 1455 },
        )
        .unwrap();
        assert!(login.contains("localhost:1455"));
        assert!(!login.contains("localhost:*"));
        for service in [
            "com.apple.dnssd.service",
            "com.apple.SecurityServer",
            "com.apple.securityd",
            "com.apple.trustd",
            "com.apple.trustd.agent",
        ] {
            assert!(profile.contains(&format!("(allow mach-lookup (global-name \"{service}\"))")));
        }
        assert!(!profile.contains("(allow network*)"));
        assert!(sandbox_profile(Path::new("relative"), Path::new("/bin/codex"), Path::new("/home/u")).is_err());
    }

    #[test]
    fn descendants_include_escaped_groups_and_transitive_children() {
        let table = "1 0 1\n10 1 10\n11 10 10\n12 11 12\n13 12 13\n14 1 10\n99 1 99\n";
        assert_eq!(descendant_tree(table, 10).unwrap(), std::collections::BTreeSet::from([11, 12, 13, 14]));
        assert!(descendant_tree("bad table", 10).is_err());
        assert!(descendant_tree("1 0", 10).is_err());
        assert!(descendant_tree("1 0 1", 99).unwrap().is_empty());
    }

    #[test]
    fn version_gate_rejects_unknown_and_out_of_range() {
        let range = VersionRange::default();
        assert_eq!(range.check("codex-cli 0.163.0\n").unwrap(), Version(0, 163, 0));
        for version in ["codex-cli 0.162.0", "codex-cli 0.164.0", "codex-cli 0.163.0-beta", "something 0.163.0"] {
            assert!(range.check(version).is_err());
        }
    }
}
