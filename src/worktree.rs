use std::path::{Path, PathBuf};

/// Create a git worktree with a new branch.
///
/// Branch name: `nerv/<session_prefix>/<slug>`
/// Worktree dir: `<nerv_dir>/worktrees/<repo>-<session_prefix>-<slug>`
pub fn create_worktree(
    repo_root: &Path,
    nerv_dir: &Path,
    branch_name: &str,
    session_prefix: &str,
) -> anyhow::Result<PathBuf> {
    let slug = slugify(branch_name);
    if slug.is_empty() {
        anyhow::bail!("branch name is empty after sanitization");
    }

    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());

    let dir_name = format!("{}-{}-{}", slugify(&repo_name), session_prefix, slug);
    let wt_path = nerv_dir.join("worktrees").join(&dir_name);
    let git_branch = format!("nerv/{}/{}", session_prefix, slug);

    std::fs::create_dir_all(nerv_dir.join("worktrees"))?;

    // Refuse to branch from a dirty tree — the worktree starts at HEAD,
    // so uncommitted changes would be silently left behind and the agent
    // would start from a state the user didn't intend.
    let status = git_output(repo_root, &["status", "--porcelain"])?;
    if !status.is_empty() {
        anyhow::bail!(
            "repository has uncommitted changes — commit or stash them before creating a worktree"
        );
    }

    let output = std::process::Command::new(crate::git())
        .args([
            "-c",
            "core.lockTimeout=30000", // wait up to 30s for .git/index.lock
            "worktree",
            "add",
            &wt_path.to_string_lossy(),
            "-b",
            &git_branch,
            "HEAD",
        ])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr.trim());
    }

    Ok(wt_path)
}

/// Merge worktree branch into the main worktree's HEAD, then remove the
/// worktree and branch.
///
/// Returns the path to the main worktree (original repo) on success.
pub fn merge_worktree(wt_path: &Path) -> anyhow::Result<PathBuf> {
    // Get current branch in the worktree
    let branch = git_output(wt_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        anyhow::bail!("worktree is in detached HEAD state");
    }

    // Find the main worktree (first entry in `git worktree list --porcelain`)
    let list_output = git_output(wt_path, &["worktree", "list", "--porcelain"])?;
    let main_wt = list_output
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("could not determine main worktree"))?;

    // Determine the main branch name for informational messages
    let main_branch = git_output(&main_wt, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|_| "HEAD".into());

    // Check for uncommitted changes in the worktree
    let status = git_output(wt_path, &["status", "--porcelain"])?;
    if !status.is_empty() {
        anyhow::bail!("worktree has uncommitted changes — commit or stash first");
    }

    // Merge from the main worktree
    let merge_out = std::process::Command::new(crate::git())
        .args(["merge", &branch])
        .current_dir(&main_wt)
        .output()?;
    if !merge_out.status.success() {
        // Abort the failed merge to restore clean state
        std::process::Command::new(crate::git())
            .args(["merge", "--abort"])
            .current_dir(&main_wt)
            .output()
            .ok();
        anyhow::bail!(
            "merge conflicts detected — aborted automatically.\n\
             To resolve manually, run from {}:\n\n\
             \x20 cd {} && git merge {}\n\n\
             Or paste this prompt into a coding agent:\n\n\
             \x20 Merge branch '{}' into '{}' in {}. \
             Resolve all conflicts, keeping the intent of both sides, then commit.",
            main_wt.display(),
            main_wt.display(),
            branch,
            branch,
            main_branch,
            main_wt.display(),
        );
    }

    // Remove the worktree
    let rm_out = std::process::Command::new(crate::git())
        .args(["worktree", "remove", &wt_path.to_string_lossy()])
        .current_dir(&main_wt)
        .output()?;
    if !rm_out.status.success() {
        let stderr = String::from_utf8_lossy(&rm_out.stderr);
        anyhow::bail!("worktree remove failed: {}", stderr.trim());
    }

    // Delete the branch
    let del_out = std::process::Command::new(crate::git())
        .args(["branch", "-d", &branch])
        .current_dir(&main_wt)
        .output()?;
    if !del_out.status.success() {
        // Non-fatal — branch might have already been cleaned up
        let stderr = String::from_utf8_lossy(&del_out.stderr);
        crate::log::warn(&format!("branch delete warning: {}", stderr.trim()));
    }

    Ok(main_wt)
}

fn git_output(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = std::process::Command::new(crate::git()).args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args[0], stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Sanitize a string into a URL/path-safe slug.
fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("feat/foo-bar"), "feat-foo-bar");
        assert_eq!(slugify("My Feature!!!"), "my-feature");
        assert_eq!(slugify("--a--b--"), "a-b");
        assert_eq!(slugify("UPPER_CASE"), "upper-case");
    }
}
