# Permissions

Tool calls are checked before execution. Operations within the git repo
root are auto-approved; everything else prompts for confirmation.

## Classification

```
core::permissions::check(tool, args, repo_root) → Allow | Ask(reason)
```

### Auto-approved

- **File tools** (read, edit, write, grep, find, ls, symbols, codemap): path resolves within repo
- **Bash**: no external path references, no dangerous patterns
- **Memory**: always (writes to ~/.nerv/memory.md)
- **Relative paths**: resolved against repo root — `src/main.rs` is fine,
  `../../etc/passwd` is caught

### Needs confirmation

- File tools targeting paths outside repo root
- Bash with absolute paths outside repo (extracted from redirects too)
- Bash with dangerous patterns: `sudo`, `rm -rf /`, `dd if=`, `mkfs`
- Bash using subshells `$()`, backticks, or `eval`
- Unknown tools

### Safe system paths (always allowed in bash)

Defined as constants in `permissions.rs`:

`/usr/bin`, `/usr/local`, `/bin`, `/opt`, `/tmp`, `/dev/null`,
`/dev/stdout`, `/dev/stderr`, `/proc/self`, `/etc/hosts`

Home subdirectories (`SAFE_HOME_DIRS`): `~/.nerv`, `~/.config`, `~/.cargo` —
shared between `check_bash` (token-based) and `is_safe_home_path`
(absolute-path-based) via a single const array.

## Path resolution

Paths are normalized without touching the filesystem (`normalize_path`
resolves `..` components). This catches traversal attacks like
`src/../../etc/passwd` which would escape the repo root.

For bash commands, path tokens are extracted after expanding redirect
operators (`>`, `>>`, `<`), so `echo x > /etc/foo` is caught.

## Cross-thread flow

```
session thread                         main thread
──────────────                         ───────────
execute_tools()
  permission_fn(tool, args)
    check() → Allow → proceed
    check() → Ask(reason)
      send PermissionRequest ──────>  show "⚠ Permission: ..." in status
      block on resp_rx.recv()         wait for y/n key
                              <──────  send true/false on resp_tx
    proceed or return "denied"
```

The `PermissionFn` closure is created in `prompt()`, capturing the repo
root and event channel. It's stored on `AgentState`.

## Key handling

The y/n prompt compares raw bytes (`b"y"`, `b"n"`) because `parse_key()`
only handles control keys and escape sequences. Enter defaults to deny
(safe default). All other input is ignored while the prompt is active.

## Token efficiency

When a tool call is denied, `transform_context` replaces its arguments
with `{}` before sending to the LLM. A denied 5KB file write becomes
~10 tokens instead of ~1.5k. The denial message is preserved so the
model understands what happened.

## Test harness

`permissions_enabled` defaults to `false` on `AgentSession`. Only
`main.rs` sets it to `true`. Integration tests skip permissions to
avoid blocking on a missing TUI.
