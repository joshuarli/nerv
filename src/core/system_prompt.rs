use super::resource_loader::LoadedResources;
use std::path::Path;

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are an expert coding assistant. You help users by reading files, executing commands, editing code, and writing new files.

# Guidelines

- Be concise in your responses
- Read files before modifying them to understand existing code
- Use the edit tool for targeted changes; use write only for new files
- Prefer grep/find/ls tools over bash for file exploration
- Show file paths clearly when working with files
- If you make an error, acknowledge it and fix it";

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
