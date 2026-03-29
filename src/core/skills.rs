use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub content: String,
}

/// Load skills from a directory. Each .md file with YAML frontmatter is a
/// skill.
pub fn load_skills(dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(skill) = load_skill(&path) {
            crate::log::info(&format!("loaded skill: {} ({})", skill.name, path.display()));
            skills.push(skill);
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn load_skill(path: &Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    let (frontmatter, body) = parse_frontmatter(&content);

    let name = frontmatter
        .get("name")
        .cloned()
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))?;

    let description = frontmatter.get("description").cloned().unwrap_or_default();

    Some(Skill { name, description, file_path: path.to_path_buf(), content: body.to_string() })
}

/// Format skills for inclusion in the system prompt.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "\n\n# Skills".to_string(),
        "Specialized instructions for specific tasks. Use /skill:<name> to invoke.".to_string(),
        String::new(),
    ];

    for skill in skills {
        lines.push(format!("- **{}**: {}", skill.name, skill.description));
    }

    lines.join("\n")
}

/// Simple YAML frontmatter parser. Returns (key-value map, body after
/// frontmatter).
fn parse_frontmatter(content: &str) -> (std::collections::HashMap<String, String>, &str) {
    let mut map = std::collections::HashMap::new();

    if !content.starts_with("---") {
        return (map, content);
    }

    let rest = &content[3..];
    let Some(end) = rest.find("\n---") else {
        return (map, content);
    };

    let frontmatter = &rest[..end];
    let body_start = 3 + end + 4; // skip opening ---, frontmatter, closing ---\n
    let body = if body_start < content.len() {
        content[body_start..].trim_start_matches('\n')
    } else {
        ""
    };

    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            map.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').trim_matches('\'').to_string(),
            );
        }
    }

    (map, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\nname: commit\ndescription: Create a git commit\n---\nDo the thing.";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("name").unwrap(), "commit");
        assert_eq!(fm.get("description").unwrap(), "Create a git commit");
        assert_eq!(body, "Do the thing.");
    }

    #[test]
    fn parse_frontmatter_missing() {
        let content = "Just some text.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_empty());
        assert_eq!(body, "Just some text.");
    }

    #[test]
    fn format_skills_empty() {
        assert_eq!(format_skills_for_prompt(&[]), "");
    }
}
