use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A loaded skill from a SKILL.md file.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub dir: PathBuf,
}

/// Registry of available skills.
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    /// Scan for skills in global and project-local directories.
    /// Project-local skills (`.infinity/skills/`) take precedence over global (`~/.infinity/skills/`).
    pub fn load() -> Self {
        let mut skills = HashMap::new();

        // Global skills: ~/.infinity/skills/*/SKILL.md
        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".infinity").join("skills");
            load_skills_from_dir(&global_dir, &mut skills);
        }

        // Project-local skills override global
        let local_dir = PathBuf::from(".infinity").join("skills");
        load_skills_from_dir(&local_dir, &mut skills);

        Self { skills }
    }

    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// List all skills as (name, description) pairs, sorted by name.
    pub fn list(&self) -> Vec<(&str, &str)> {
        let mut items: Vec<(&str, &str)> = self
            .skills
            .values()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect();
        items.sort_by_key(|(name, _)| *name);
        items
    }

    /// Generate a section to append to the system prompt describing available skills.
    pub fn system_prompt_section(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }

        let mut section = String::from("\n\n## Available Skills\n");
        section.push_str("The user can invoke these skills with /skillname [args]:\n");

        let mut items: Vec<&Skill> = self.skills.values().collect();
        items.sort_by_key(|s| &s.name);

        for skill in items {
            section.push_str(&format!("- /{} — {}\n", skill.name, skill.description));
        }

        Some(section)
    }

    /// Check if user input starts with a skill invocation.
    /// Returns (skill, remaining_args) if matched.
    pub fn matches_command<'a>(&self, input: &'a str) -> Option<(&Skill, &'a str)> {
        if !input.starts_with('/') {
            return None;
        }

        let without_slash = &input[1..];
        let (cmd, args) = match without_slash.find(char::is_whitespace) {
            Some(idx) => (&without_slash[..idx], without_slash[idx..].trim()),
            None => (without_slash, ""),
        };

        self.skills.get(cmd).map(|skill| (skill, args))
    }

    /// Whether any skills are loaded.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// Scan a directory for `*/SKILL.md` files and insert them into the map.
fn load_skills_from_dir(dir: &Path, skills: &mut HashMap<String, Skill>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let skill_file = entry.path().join("SKILL.md");
        if skill_file.is_file() {
            if let Some(skill) = parse_skill_md(&skill_file) {
                skills.insert(skill.name.clone(), skill);
            }
        }
    }
}

/// Parse a SKILL.md file with frontmatter.
///
/// Format:
/// ```
/// ---
/// name: my-skill
/// description: Does something useful
/// ---
/// Body content here...
/// ```
fn parse_skill_md(path: &Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();

    // Must start with ---
    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the second ---
    let after_first = &trimmed[3..].trim_start_matches(['\r', '\n']);
    let end_idx = after_first.find("\n---")?;
    let frontmatter = &after_first[..end_idx];
    let body_start = end_idx + 4; // skip "\n---"
    let body = if body_start < after_first.len() {
        after_first[body_start..].trim().to_string()
    } else {
        String::new()
    };

    let mut name = None;
    let mut description = None;

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("description:") {
            description = Some(val.trim().to_string());
        }
    }

    let name = name?;
    let description = description.unwrap_or_default();
    let dir = path.parent()?.to_path_buf();

    Some(Skill {
        name,
        description,
        body,
        dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_skill_md() {
        let dir = std::env::temp_dir().join("test_skill_parse");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let skill_path = dir.join("SKILL.md");
        fs::write(
            &skill_path,
            "---\nname: test-skill\ndescription: A test skill\n---\nThis is the body.\n",
        )
        .unwrap();

        let skill = parse_skill_md(&skill_path).unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "A test skill");
        assert_eq!(skill.body, "This is the body.");
        assert_eq!(skill.dir, dir);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_skill_md_no_frontmatter() {
        let dir = std::env::temp_dir().join("test_skill_nofm");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let skill_path = dir.join("SKILL.md");
        fs::write(&skill_path, "No frontmatter here").unwrap();

        assert!(parse_skill_md(&skill_path).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_matches_command() {
        let mut skills = HashMap::new();
        skills.insert(
            "review".to_string(),
            Skill {
                name: "review".to_string(),
                description: "Code review".to_string(),
                body: "Review body".to_string(),
                dir: PathBuf::from("/tmp"),
            },
        );

        let registry = SkillRegistry { skills };

        let (skill, args) = registry.matches_command("/review PR #123").unwrap();
        assert_eq!(skill.name, "review");
        assert_eq!(args, "PR #123");

        let (skill, args) = registry.matches_command("/review").unwrap();
        assert_eq!(skill.name, "review");
        assert_eq!(args, "");

        assert!(registry.matches_command("not a command").is_none());
        assert!(registry.matches_command("/unknown").is_none());
    }
}
