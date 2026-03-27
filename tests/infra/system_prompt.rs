//! System prompt construction tests.

use nerv::core::resource_loader::{ContextFile, LoadedResources};
use nerv::core::system_prompt::build_system_prompt;
use std::path::{Path, PathBuf};

fn empty_resources() -> LoadedResources {
    LoadedResources {
        context_files: Vec::new(),
        system_prompt: None,
        append_prompts: Vec::new(),
        memory: None,
        skills: Vec::new(),
    }
}

#[test]
fn default_prompt_includes_tools_and_cwd() {
    let resources = empty_resources();
    let prompt = build_system_prompt(
        Path::new("/home/user/project"),
        &resources,
        &["read", "bash", "edit", "write", "grep"],
        &[],
        &[],
    );

    assert!(prompt.contains("read"));
    assert!(prompt.contains("bash"));
    assert!(prompt.contains("edit"));
    assert!(prompt.contains("grep"));
    assert!(prompt.contains("cwd: /home/user/project"));
    assert!(prompt.contains("Date:"));
}

#[test]
fn custom_system_prompt_replaces_default() {
    let mut resources = empty_resources();
    resources.system_prompt = Some("You are a pirate assistant.".into());
    let prompt = build_system_prompt(Path::new("/tmp"), &resources, &["bash"], &[], &[]);

    assert!(prompt.contains("pirate assistant"));
    assert!(!prompt.contains("expert coding assistant"));
}

#[test]
fn tool_snippets_included() {
    let resources = empty_resources();
    let snippets = vec![(
        "read".to_string(),
        "read files with line numbers".to_string(),
    )];
    let prompt = build_system_prompt(Path::new("/tmp"), &resources, &["read"], &snippets, &[]);

    assert!(prompt.contains("read: read files with line numbers"));
}

#[test]
fn context_files_included() {
    let mut resources = empty_resources();
    resources.context_files = vec![ContextFile {
        path: PathBuf::from("AGENTS.md"),
        content: "Use cargo test to run tests.".into(),
    }];
    let prompt = build_system_prompt(Path::new("/tmp"), &resources, &["bash"], &[], &[]);

    assert!(prompt.contains("Project Context"));
    assert!(prompt.contains("AGENTS.md"));
    assert!(prompt.contains("Use cargo test to run tests."));
}

#[test]
fn memory_included() {
    let mut resources = empty_resources();
    resources.memory =
        Some("User prefers tabs over spaces.\nProject uses Rust 2024 edition.".into());
    let prompt = build_system_prompt(Path::new("/tmp"), &resources, &["bash"], &[], &[]);

    assert!(prompt.contains("# Memory"));
    assert!(prompt.contains("User prefers tabs over spaces"));
    assert!(prompt.contains("Rust 2024 edition"));
}

#[test]
fn append_prompts_concatenated() {
    let mut resources = empty_resources();
    resources.append_prompts = vec!["Always respond in haiku.".into()];
    let prompt = build_system_prompt(Path::new("/tmp"), &resources, &["bash"], &[], &[]);

    assert!(prompt.contains("Always respond in haiku"));
}
