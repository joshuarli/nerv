pub mod agent;
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
    HOME.get_or_init(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .as_deref()
}

/// Return the `~/.nerv` directory.
///
/// Cached alongside `home_dir()` — free after the first call.
/// Returns an empty path if `$HOME` is unset (uncommon in practice).
pub fn nerv_dir() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static NERV: OnceLock<std::path::PathBuf> = OnceLock::new();
    NERV.get_or_init(|| {
        home_dir()
            .map(|h| h.join(".nerv"))
            .unwrap_or_default()
    })
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
/// Returns `None` if the path is not a git repository or git is unavailable.
pub fn repo_fingerprint(repo_root: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Find the repo root by walking up from `start` looking for `.git/`.
pub fn find_repo_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}
