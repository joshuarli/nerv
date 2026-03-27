use nerv::core::skills::*;
use tempfile::TempDir;

#[test]
fn load_skills_from_directory() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("commit.md"),
        "---\nname: commit\ndescription: Create a commit\n---\nDo git commit.",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("review.md"),
        "---\nname: review\ndescription: Review code\n---\nLook at the diff.",
    )
    .unwrap();
    // Non-md file should be ignored
    std::fs::write(tmp.path().join("notes.txt"), "not a skill").unwrap();

    let skills = load_skills(tmp.path());
    assert_eq!(skills.len(), 2);
    // Sorted by name
    assert_eq!(skills[0].name, "commit");
    assert_eq!(skills[1].name, "review");
    assert_eq!(skills[0].description, "Create a commit");
    assert!(skills[0].content.contains("git commit"));
}

#[test]
fn skill_without_frontmatter_uses_filename() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("deploy.md"), "Run deploy script.").unwrap();

    let skills = load_skills(tmp.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "deploy");
    assert!(skills[0].description.is_empty());
    assert_eq!(skills[0].content, "Run deploy script.");
}

#[test]
fn load_skills_empty_directory() {
    let tmp = TempDir::new().unwrap();
    let skills = load_skills(tmp.path());
    assert!(skills.is_empty());
}

#[test]
fn load_skills_nonexistent_directory() {
    let skills = load_skills(std::path::Path::new("/nonexistent/path"));
    assert!(skills.is_empty());
}

#[test]
fn format_skills_includes_all() {
    let skills = vec![
        Skill {
            name: "commit".into(),
            description: "Create a commit".into(),
            file_path: "/tmp/commit.md".into(),
            content: "...".into(),
        },
        Skill {
            name: "review".into(),
            description: "Review PR".into(),
            file_path: "/tmp/review.md".into(),
            content: "...".into(),
        },
    ];
    let prompt = format_skills_for_prompt(&skills);
    assert!(prompt.contains("commit"));
    assert!(prompt.contains("review"));
    assert!(prompt.contains("Create a commit"));
    assert!(prompt.contains("Skills"));
}

#[test]
fn frontmatter_with_quoted_values() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("test.md"),
        "---\nname: \"my-skill\"\ndescription: 'A skill with quotes'\n---\nBody.",
    )
    .unwrap();

    let skills = load_skills(tmp.path());
    assert_eq!(skills[0].name, "my-skill");
    assert_eq!(skills[0].description, "A skill with quotes");
}
