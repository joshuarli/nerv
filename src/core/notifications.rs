/// Notification hooks — fire user-configured shell commands on specific events.
///
/// Config format (in ~/.nerv/config.json):
/// ```json
/// "notifications": [
///   {
///     "matchers": ["onResponseComplete", "onPermissionDenied"],
///     "hooks": [{ "type": "command", "command": "terminal-notifier -title 'nerv' -message 'done'" }]
///   },
///   {
///     "matchers": ["onUserInput"],
///     "hooks": [{ "type": "command", "command": "..." }]
///   }
/// ]
/// ```
///
/// Matchers:
///   - `onPermissionDenied`  — a tool call was denied (user said no, or
///     permission check blocked it)
///   - `onCompactionDone`    — a compaction cycle completed (auto or manual)
///   - `onResponseComplete`  — the model finished a turn and is waiting for
///     user input
///   - `onUserInput`         — the user submitted a prompt (useful for clearing
///     "done" indicators)
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NotificationMatcher {
    OnPermissionDenied,
    OnCompactionDone,
    OnResponseComplete,
    OnUserInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationHook {
    /// Currently the only supported type is "command".
    #[serde(rename = "type")]
    pub hook_type: String,
    /// Shell command to run (passed to `/bin/sh -c`).
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRule {
    pub matchers: Vec<NotificationMatcher>,
    pub hooks: Vec<NotificationHook>,
}

/// Fire all hooks whose matchers include `event`.
/// Commands are spawned detached (fire-and-forget); we do not wait for them.
pub fn fire(event: NotificationMatcher, rules: &[NotificationRule]) {
    for rule in rules {
        if !rule.matchers.contains(&event) {
            continue;
        }
        for hook in &rule.hooks {
            if hook.hook_type != "command" {
                continue;
            }
            // Spawn detached — failures are silently ignored to avoid disrupting
            // the agent loop.
            let _ = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(&hook.command)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}
