# Session Tree

## Overview

Every nerv session is a **single tree of entries**, not a flat list. When you
navigate back to an earlier point in the conversation and ask something new,
new entries are appended as children of that earlier node, creating a fork. The
original branch remains intact and you can switch back to it at any time.

One "session" can contain many independent lines of conversation that share a
common root, rather than having separate sessions for each experiment.

---

## Storage model

Everything lives in `~/.nerv/sessions.db` (SQLite, WAL mode).

### `sessions` table

One row per session. The session `id` is a random 128-bit hex UUID. Other
columns: `cwd`, `created_at`, `updated_at`, `preview` (first 80 chars of the
opening message), `name` (auto-generated title), `worktree`, `compact_threshold`.

### `entries` table

```
id         TEXT PRIMARY KEY   -- 8-char random hex
session_id TEXT               -- foreign key to sessions.id
parent_id  TEXT               -- id of parent entry, NULL for roots
seq        INTEGER            -- global append order within the session
data       TEXT               -- JSON-serialized SessionEntry
```

Every entry knows its parent. The **tree shape** is fully encoded in
`parent_id` — `seq` is only used for load ordering and is never relied on for
tree structure.

A **branch point** is any entry that has two or more children. A **leaf** is an
entry with no children.

---

## Entry types

Each entry is a `SessionEntry` variant serialized as tagged JSON (`"type": "..."`):

| type | what it records |
|---|---|
| `message` | a user or assistant turn (including tool calls and results) |
| `compaction` | a context-compaction event; marks the cut point and stores the summary |
| `branch_summary` | optional LLM summary written at a branch point |
| `model_change` | the user switched models mid-session |
| `thinking_level_change` | extended thinking toggled on/off |
| `label` | a user-created text label at a point in the tree |
| `session_info` | session name update |
| `system_prompt` | snapshot of the system prompt in use |
| `permission_accept` | a tool call the user permanently accepted |
| `custom_message` | injected message (e.g. from `/inject`) |

All entry types share `id`, `parent_id`, and `timestamp`.

---

## The active leaf

`SessionManager` keeps a `leaf_id: Option<String>` in memory. This is the
**current tip of the active branch** — the entry that the next appended entry
will point to as its `parent_id`.

- On a new session: `leaf_id = None`. The first appended entry has
  `parent_id = None` (it is a root).
- After each `append_entry` call: `leaf_id` is updated to the id of the entry
  just written.
- On `/resume`: `leaf_id` is set to the highest-`seq` leaf (entry with no
  children) found when loading.

---

## Branching

Branching is not a special operation — it is just **moving `leaf_id` backwards**
and then continuing to type.

```rust
SessionManager::branch(from_id)   // sets leaf_id = from_id
```

The next `append_entry` call writes a new entry with `parent_id = from_id`,
creating a new child of that node. The old branch is untouched; its entries
still exist in the DB and still point to each other through their own
`parent_id` chain.

Example — user asks "explain chemistry", gets an answer, then branches back to
the root to ask "explain physics":

```
[root: "hi"]
    ├─ [user: "explain chemistry"] → [assistant: ...]  ← branch A
    └─ [user: "explain physics"]   → [assistant: ...]  ← branch B (active)
```

Both branches share the same `session_id`. The only thing that changes when you
switch branches is `leaf_id`.

---

## Reading a branch

`get_branch()` walks the `parent_id` chain from `leaf_id` back to the root and
returns entries in root-first order:

```
leaf → parent → parent → … → root
// reversed to:
root → … → parent → leaf
```

This is the **linear history** for the current branch. It is what:
- Gets loaded into the agent's message context (`build_session_context`)
- Gets exported (`export_jsonl`, `export_entries_html`)
- Gets displayed in chat on a branch switch

`get_branch()` never crosses to a sibling branch — it only follows
`parent_id` pointers, which form a simple linked list from any leaf to the root.

---

## Context reconstruction

`build_session_context()` calls `get_branch()` and walks the result to rebuild:

1. **Messages** — `SessionEntry::Message` → `AgentMessage` pushed in order.
2. **Model** — the last `ModelChange` entry in the branch wins.
3. **Thinking level** — the last `ThinkingLevelChange` entry wins.
4. **Compaction** — if a `Compaction` entry exists, entries before the cut point
   are skipped and a synthetic `AgentMessage::CompactionSummary` is prepended.
   The `first_kept_entry_id` field on the `Compaction` entry marks where replay
   resumes.

---

## Tree selector (`^S` / `/tree`)

The tree selector renders the full tree for the active session and lets you jump
to any node.

**How `get_tree()` works:**

1. All entries in the session are loaded into a flat map `id → node`.
2. `parent_id` links are walked to build `children` lists on each node.
3. Orphan nodes (whose `parent_id` doesn't exist in the DB, e.g. after
   compaction deleted their ancestors) become additional roots.
4. Children at each level are sorted by timestamp.
5. The result is a `Vec<SessionTreeNode>` of root nodes, each with nested
   `children`.

**Display:**

The tree is flattened depth-first (root → leaves) for display. The active
leaf is highlighted. Prefix lines (`│`, `├─`, `└─`) are drawn based on
sibling position so the branching structure is visible. Single-child chains
are drawn without connectors to reduce noise.

**Navigation:**

| key | action |
|---|---|
| `↑` / `↓` | move selection |
| `Enter` | switch to selected node (sets `leaf_id`, reloads context) |
| `ctrl+←` | fold selected node's children |
| `ctrl+→` | unfold selected node's children |
| `ctrl+u` | toggle filter (show only user messages) |
| `Esc` | close without switching |

Switching branches reloads the full message context for the new branch and
re-renders the chat pane.

---

## Full-text search and `/resume`

The FTS5 `search_index` table stores searchable text from user messages,
assistant messages, and tool results (under 2 KB). Each row carries
`session_id` and `entry_id` but no branch information.

`search_sessions(query)` returns **one result per session** — hits across all
branches of a session are deduplicated to the single best-ranking excerpt.
Selecting a result resumes the session at its **latest leaf** (the highest-`seq`
leaf across all branches), regardless of which branch contained the matching text.

This means:
- If two branches in one session cover different topics and you search for a
  term that only exists in branch A, the result will open the session but land
  you on branch B's leaf if B is newer. Use `^S` to navigate to the right
  branch from there.
- Text deleted by compaction is also removed from `search_index` and is no
  longer findable by search.

The `/resume` picker is intentionally session-granular — it finds the session.
Branch-level navigation is the job of the tree selector (`^S`).

---

## Export

Exports contain only the **current branch** (root → `leaf_id`), not the entire
tree. Sibling branches are excluded.

Filenames embed both the session id and the leaf id to avoid collisions between
branches of the same session:

```
~/.nerv/exports/{session8}-{leaf6}.html
~/.nerv/exports/{session8}-{leaf6}.jsonl
```

The JSONL header includes `"leaf_id"` so the file is self-describing.
Re-exporting from the same branch at the same leaf overwrites the same file
(idempotent). To export a different branch, switch to it in the tree selector
(`^S`) first, then run `/export`.

---

## Compaction

Compaction reduces the DB footprint and context size for the **current branch
only**. The steps are:

1. Walk `get_branch()` (root → `leaf_id`) — not the full flat `entries()` list.
2. Find the cut point: walk backwards from the leaf, accumulate token estimates,
   stop when the budget (default ~20 k tokens) is filled. The entry at that
   point becomes `first_kept_entry_id`.
3. Call the LLM to summarize all messages before the cut point.
4. Delete from the DB only the entries that are **on the current branch and
   precede the cut point** — by entry id, not by `seq`. Sibling branches keep
   their entries.
5. Append a `Compaction` entry to the active leaf pointing at
   `first_kept_entry_id`, which becomes the new anchor for context
   reconstruction.

**Effect on the tree:**

Entries shared by multiple branches (common ancestors above the cut point) are
deleted. If a sibling branch descends from one of those deleted entries, its
`get_branch()` walk will hit a missing id and stop — truncating its deep
history. This is an acceptable trade-off: shared ancestors that fall below the
compaction cut are by definition old. Recent entries on sibling branches are
always safe because compaction only deletes entries *before* the cut.

The `Compaction` entry lives only on the active branch. Sibling branches that
don't pass through it reconstruct without any summary.

In practice: compact from the branch you care about. Other branches that share
old ancestors may lose some deep history, but their recent entries are unaffected.

---

## Identity summary

| identifier | what it is | scope |
|---|---|---|
| session id | random 128-bit UUID | the whole tree |
| entry id | 8-char hex | a single node |
| leaf id | entry id of a leaf | a branch tip |
| branch | no stored entity | implied by `leaf → … → root` path |
