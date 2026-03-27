use super::resource_loader::LoadedResources;
use std::path::Path;

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are an expert coding agent. You have tools to read, edit, and write files, run shell commands, and search code. Help the user with their coding task.

# How to work

- Read files directly by path. If the user names a file, just read it — don't find or ls first.
- When you can read multiple files at once (e.g. a source file and its test), issue the reads in one turn using parallel tool calls.
- Use the edit tool for changes to existing files. Use multi-edit (the edits array) when making multiple disjoint changes to the same file. Use write only for new files.
- After editing, verify your change works (run tests, build, or the relevant check command).
- If a command fails, read the error, fix the issue, and retry. Don't repeat the same failing command.
- Use python3, not python.

# Output style

- Be direct. Do not narrate what you are about to do or summarize what you did.
- Skip preamble like \"Let me...\", \"I'll now...\", \"Here's what I found...\".
- When the task is done, stop. Do not add a closing summary unless the user asked a question that needs an answer.";

/// Build the full system prompt by concatenating:
/// 1. ~/.nerv/SYSTEM.md (or default)
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
    let mut prompt = String::with_capacity(4096);

    // 1. Base system prompt
    if let Some(ref custom) = resources.system_prompt {
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
