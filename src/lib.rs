pub mod agent;
pub mod str;
pub mod bootstrap;
pub mod compaction;
pub mod core;
pub mod errors;
pub mod export;
pub mod http;
pub mod index;
pub mod interactive;
pub mod log;
pub mod session;
pub mod tools;
pub mod tui;
pub mod worktree;

/// Get the user's home directory from $HOME.
///
/// The result is resolved once and cached for the lifetime of the process via
/// `OnceLock`, so subsequent calls are a single atomic load rather than a
/// `getenv` syscall.
pub fn home_dir() -> Option<&'static std::path::Path> {
    use std::sync::OnceLock;
    static HOME: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| std::env::var_os("HOME").map(std::path::PathBuf::from)).as_deref()
}

/// Return the `~/.nerv` directory.
///
/// Cached alongside `home_dir()` — free after the first call.
/// Returns an empty path if `$HOME` is unset (uncommon in practice).
pub fn nerv_dir() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static NERV: OnceLock<std::path::PathBuf> = OnceLock::new();
    NERV.get_or_init(|| home_dir().map(|h| h.join(".nerv")).unwrap_or_default())
}

/// Millisecond timestamp (used for message timestamps).
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Stable fingerprint for a git repo: the SHA of the initial commit.
///
/// Uses `git rev-list --max-parents=0 HEAD` run at `repo_root`. This SHA is
/// permanent — it survives renames, moves, and re-clones from the same origin —
/// so it can be used as a stable identifier for session and cache lookups even
/// after the directory is relocated.
///
/// The result is cached per repo root for the lifetime of the process —
/// repeated calls for the same path are a single mutex + HashMap lookup.
///
/// Returns `None` if the path is not a git repository or git is unavailable.
pub fn repo_fingerprint(repo_root: &std::path::Path) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(cached) = map.get(repo_root) {
        return cached.clone();
    }
    let result = (|| {
        let out = std::process::Command::new(crate::git())
            .args(["rev-list", "--max-parents=0", "HEAD"])
            .current_dir(repo_root)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if sha.is_empty() { None } else { Some(sha) }
    })();
    map.insert(repo_root.to_path_buf(), result.clone());
    result
}

/// Find the repo root by walking up from `start` looking for `.git/`.
pub fn find_repo_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, Option<std::path::PathBuf>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(cached) = map.get(start) {
        return cached.clone();
    }
    let mut dir = start.to_path_buf();
    let result = loop {
        if dir.join(".git").exists() {
            break Some(dir);
        }
        if !dir.pop() {
            break None;
        }
    };
    map.insert(start.to_path_buf(), result.clone());
    result
}

/// Returns the per-repo data directory: `~/.nerv/repos/<repo_id>/`.
///
/// Falls back to `nerv_dir` when there is no stable fingerprint (non-git
/// directories, empty repos, or git unavailable).  Callers must handle both
/// cases — the directory is created if it does not yet exist.
pub fn repo_data_dir(cwd: &std::path::Path) -> std::path::PathBuf {
    find_repo_root(cwd)
        .and_then(|root| repo_fingerprint(&root))
        .map(|fpr| nerv_dir().join("repos").join(fpr))
        .unwrap_or_else(|| nerv_dir().to_path_buf())
}

/// Resolved absolute paths for external binaries, set once at startup.
///
/// Every binary that nerv shells out to is resolved at bootstrap time so:
/// - we fail fast with a clear error if a required tool is missing
/// - subsequent `Command::new(git())` calls use the absolute path, immune to
///   mid-session `$PATH` changes or a binary being swapped out
pub struct Binaries {
    pub git: std::path::PathBuf,
    pub rg: Option<std::path::PathBuf>,
    pub fd: Option<std::path::PathBuf>,
    /// macOS Keychain CLI — always `/usr/bin/security`; `None` on non-macOS.
    pub security: Option<std::path::PathBuf>,
}

static BINARIES: std::sync::OnceLock<Binaries> = std::sync::OnceLock::new();

/// Walk `$PATH` to find an executable named `name`.
pub fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(name);
            if full.is_file() { Some(full) } else { None }
        })
    })
}

/// Store the resolved binaries. Called once from `bootstrap::resolve_binaries`.
/// Silently no-ops if called more than once (e.g. subcommand + main both call resolve_binaries).
pub fn init_binaries(b: Binaries) {
    BINARIES.set(b).ok();
}

/// Get the resolved path for `git`. Falls back to `"git"` if called before `init_binaries`.
pub fn git() -> &'static std::path::Path {
    static GIT_FALLBACK: std::sync::LazyLock<std::path::PathBuf> =
        std::sync::LazyLock::new(|| std::path::PathBuf::from("git"));
    BINARIES.get().map(|b| b.git.as_path()).unwrap_or(&*GIT_FALLBACK)
}

/// Get the resolved path for `rg` (ripgrep), or `None` if not installed or not yet resolved.
pub fn rg() -> Option<&'static std::path::Path> {
    BINARIES.get()?.rg.as_deref()
}

/// Get the resolved path for `fd`, or `None` if not installed or not yet resolved.
pub fn fd() -> Option<&'static std::path::Path> {
    BINARIES.get()?.fd.as_deref()
}

/// Get the resolved path for the macOS `security` CLI, or `None` on non-macOS or unresolved.
pub fn security() -> Option<&'static std::path::Path> {
    BINARIES.get()?.security.as_deref()
}

/// Resolve a path string to an absolute `PathBuf`.
///
/// Handles three cases:
/// - `~/…` — expands to `$HOME/…`
/// - absolute paths — returned as-is
/// - relative paths — resolved against `cwd`
pub fn resolve_path(path: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = home_dir() {
            return home.join(rest.trim_start_matches('/'));
        }
    }
    let p = std::path::Path::new(path);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}
