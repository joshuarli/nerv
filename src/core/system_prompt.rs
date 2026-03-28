use super::resource_loader::LoadedResources;
use std::path::Path;

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are an expert coding agent. You have tools to read, edit, and write files, run shell commands, and search code. Help the user with their coding task.

# How to work

- `symbols` first (lists all definitions cheaply), then `codemap` to read implementations that matter. Never read entire files to find functions.
- `symbols` → where is X defined? `codemap` → how does X work? `grep` → where is X used? `read` → specific file/range you already know.
- When you can read multiple files at once (e.g. a source file and its test), issue the reads in one turn using parallel tool calls.
- Use the grep tool instead of bash + grep/rg.
- For mass edits (e.g. renaming a symbol across many files): read ALL affected files first, plan all changes, apply all edits, THEN run one verification. Do not interleave read-edit-check per file.
- Use the edit tool for changes to existing files. Use multi-edit (the edits array) when making multiple disjoint changes to the same file. Use write only for new files.
- After editing, verify your change works (run tests, build, or the relevant check command).
- If a tool call or command fails: (1) read the error message, (2) re-read the relevant file or state, (3) make one targeted fix, (4) retry once. If it fails again, explain the problem to the user rather than spiraling.
- Before starting edits that will touch 3+ files, briefly state the files and changes you plan to make. This lets the user course-correct before you've sunk tokens into the wrong approach.
- All tools run from the project root.

# Output style

- Be direct. Do not narrate what you are about to do or summarize what you did.
- Skip preamble like \"Let me...\", \"I'll now...\", \"Here's what I found...\".
- When the task is done, stop. Do not add a closing summary unless the user asked a question that needs an answer.";

/// Build the full system prompt by concatenating:
/// 1. Per-model prompt (~/.nerv/prompts/{model_id}.md) or
///    global (~/.nerv/system-prompt.md) or compiled default
/// 2. Tool list
/// 3. Project context (AGENTS.md, CLAUDE.md)
/// 4. Memory
/// 5. Skills
/// 6. Date + cwd
pub fn build_system_prompt(
    cwd: &Path,
    resources: &LoadedResources,
    tool_names: &[&str],
    tool_snippets: &[(String, String)],
    tool_guidelines: &[String],
) -> String {
    build_system_prompt_for_model(cwd, resources, tool_names, tool_snippets, tool_guidelines, None)
}

pub fn build_system_prompt_for_model(
    cwd: &Path,
    resources: &LoadedResources,
    tool_names: &[&str],
    tool_snippets: &[(String, String)],
    tool_guidelines: &[String],
    model_id: Option<&str>,
) -> String {
    let mut prompt = String::with_capacity(4096);

    // 1. Base system prompt: per-model → global override → compiled default
    let model_prompt = model_id.and_then(|id| {
        let path = crate::nerv_dir().join("prompts").join(format!("{}.md", id));
        if path.is_file() {
            crate::log::info(&format!("loaded per-model prompt: {}", path.display()));
            std::fs::read_to_string(&path).ok()
        } else {
            None
        }
    });

    if let Some(ref mp) = model_prompt {
        prompt.push_str(mp);
    } else if let Some(ref custom) = resources.system_prompt {
        prompt.push_str(custom);
    } else {
        prompt.push_str(DEFAULT_SYSTEM_PROMPT);
    }

    // 1b. Append prompts
    for ap in &resources.append_prompts {
        prompt.push_str("\n\n");
        prompt.push_str(ap);
    }

    // 2. Tools
    prompt.push_str("\n\n# Available Tools\n\n");
    for name in tool_names {
        if let Some((_, snippet)) = tool_snippets.iter().find(|(n, _)| n == *name) {
            prompt.push_str(&format!("- {}: {}\n", name, snippet));
        } else {
            prompt.push_str(&format!("- {}\n", name));
        }
    }
    for g in tool_guidelines {
        let trimmed = g.trim();
        if !trimmed.is_empty() {
            prompt.push_str(&format!("- {}\n", trimmed));
        }
    }

    // 3. Project context files (AGENTS.md, CLAUDE.md)
    if !resources.context_files.is_empty() {
        prompt.push_str("\n# Project Context\n\n");
        for cf in &resources.context_files {
            prompt.push_str(&format!("## {}\n\n{}\n\n", cf.path.display(), cf.content));
        }
    }

    // 4. Memory
    if let Some(ref memory) = resources.memory
        && !memory.trim().is_empty()
    {
        prompt.push_str("\n# Memory\n\nPersistent knowledge (use memory tool to update):\n\n");
        prompt.push_str(memory);
        prompt.push('\n');
    }

    // 5. Skills — just list names, content loaded on demand via /skillname
    if !resources.skills.is_empty() {
        prompt.push_str("\n# Skills\n\nAvailable via /name:\n");
        for s in &resources.skills {
            prompt.push_str(&format!("- /{}: {}\n", s.name, s.description));
        }
    }

    // 6. Metadata
    let date = crate::session::types::today_ymd();
    prompt.push_str(&format!("\nDate: {} | cwd: {}\n", date, cwd.display()));

    prompt
}
