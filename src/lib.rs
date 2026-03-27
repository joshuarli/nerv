pub mod agent;
pub mod bootstrap;
pub mod compaction;
pub mod core;
pub mod errors;
pub mod export;
pub mod http;
pub mod interactive;
pub mod log;
pub mod session;
pub mod tools;
pub mod tui;

/// Get the user's home directory from $HOME.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Millisecond timestamp (used for message timestamps).
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
