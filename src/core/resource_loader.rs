use std::path::{Path, PathBuf};

/// A context file loaded from the filesystem (AGENTS.md, CLAUDE.md, etc).
#[derive(Debug, Clone)]
pub struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

/// Resources discovered from the filesystem at startup.
#[derive(Clone)]
pub struct LoadedResources {
    pub context_files: Vec<ContextFile>,
    pub system_prompt: Option<String>,
    pub append_prompts: Vec<String>,
    pub memory: Option<String>,
    pub skills: Vec<super::skills::Skill>,
}

/// Load all resources: context files (AGENTS.md/CLAUDE.md), system prompt
/// overrides, and append prompts.
pub fn load_resources(cwd: &Path, nerv_dir: &Path) -> LoadedResources {
    let mut context_files = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // 1. Global context from ~/.nerv/AGENTS.md
    if let Some(cf) = load_context_file(nerv_dir) {
        crate::log::info(&format!("loaded {}", cf.path.display()));
        seen.insert(cf.path.clone());
        context_files.push(cf);
    }

    // 2. Walk from cwd up to root, collecting AGENTS.md / CLAUDE.md
    let mut ancestor_files = Vec::new();
    let mut dir = cwd.to_path_buf();
    loop {
        if let Some(cf) = load_context_file(&dir)
            && !seen.contains(&cf.path)
        {
            seen.insert(cf.path.clone());
            ancestor_files.push(cf);
        }
        if !dir.pop() {
            break;
        }
    }
    // Reverse so root-level files come first, cwd-level last (closest wins)
    ancestor_files.reverse();
    for cf in &ancestor_files {
        crate::log::info(&format!("loaded {}", cf.path.display()));
    }
    context_files.extend(ancestor_files);

    // 3. System prompt override: ~/.nerv/system-prompt.md
    let system_prompt = load_text_file(&nerv_dir.join("system-prompt.md"));
    if system_prompt.is_some() {
        crate::log::info(&format!(
            "loaded {}",
            nerv_dir.join("system-prompt.md").display()
        ));
    }

    // 4. Append system prompt: ~/.nerv/append-system-prompt.md
    let mut append_prompts = Vec::new();
    if let Some(content) = load_text_file(&nerv_dir.join("append-system-prompt.md")) {
        crate::log::info(&format!(
            "loaded {}",
            nerv_dir.join("append-system-prompt.md").display()
        ));
        append_prompts.push(content);
    }

    // 5. Memory: ~/.nerv/memory.md
    let memory = load_text_file(&nerv_dir.join("memory.md"));
    if memory.is_some() {
        crate::log::info("loaded memory.md");
    }

    // 6. Skills from ~/.nerv/skills/
    let skills = super::skills::load_skills(&nerv_dir.join("skills"));

    LoadedResources {
        context_files,
        system_prompt,
        append_prompts,
        memory,
        skills,
    }
}

const CONTEXT_FILENAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

fn load_context_file(dir: &Path) -> Option<ContextFile> {
    for filename in CONTEXT_FILENAMES {
        let path = dir.join(filename);
        if path.is_file() {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    return Some(ContextFile { path, content });
                }
                Err(e) => {
                    crate::log::warn(&format!("could not read {}: {}", path.display(), e));
                }
            }
        }
    }
    None
}

fn load_text_file(path: &Path) -> Option<String> {
    if path.is_file() {
        match std::fs::read_to_string(path) {
            Ok(content) => Some(content),
            Err(e) => {
                crate::log::warn(&format!("could not read {}: {}", path.display(), e));
                None
            }
        }
    } else {
        None
    }
}
