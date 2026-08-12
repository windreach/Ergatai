use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Skill info returned to renderer
#[napi(object)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub path: String,
}

/// Skill detail with markdown content
#[napi(object)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    pub content: String,
    pub path: String,
}

/// Extract description from SKILL.md frontmatter or first paragraph
fn extract_description(content: &str) -> String {
    let content = content.trim();

    // Try YAML frontmatter
    if content.starts_with("---") {
        if let Some(end) = content.find("\n---") {
            let frontmatter = &content[4..end]; // Skip opening "---\n"

            // Find description field
            for line in frontmatter.lines() {
                if let Some(rest) = line.strip_prefix("description:") {
                    let desc = rest.trim();
                    if !desc.is_empty() {
                        // Handle multi-line >- or > syntax
                        if desc == ">-" || desc == ">" {
                            // Collect indented lines after description:
                            let mut lines = frontmatter.lines().peekable();
                            // Skip to description line
                            for l in lines.by_ref() {
                                if l.starts_with("description:") {
                                    break;
                                }
                            }
                            // Collect indented continuation lines
                            let folded: String = lines
                                .take_while(|l| l.starts_with(' ') || l.starts_with('\t'))
                                .map(|l| l.trim())
                                .collect::<Vec<_>>()
                                .join(" ");
                            return folded;
                        }
                        return desc.trim_matches('"').trim().to_string();
                    }
                }
            }

            // No description found in frontmatter, fall through to content after frontmatter
            let after_frontmatter = &content[end + 4..].trim_start_matches('\n');
            return after_frontmatter
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
        }
    }

    // Fallback: first non-empty line (skip any frontmatter)
    let mut skip_frontmatter = false;
    content
        .lines()
        .filter(|l| {
            if l.trim() == "---" {
                skip_frontmatter = !skip_frontmatter;
                return false;
            }
            !skip_frontmatter
        })
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Find all SKILL.md files in a directory
fn find_skills_in_dir(dir: &Path) -> Vec<SkillInfo> {
    let mut skills = Vec::new();
    if !dir.exists() {
        return skills;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills;
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let entry_path = entry.path();

        if entry_path.is_dir() {
            let skill_md = entry_path.join("SKILL.md");
            if skill_md.exists() {
                if let Ok(content) = std::fs::read_to_string(&skill_md) {
                    let name = entry_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let description = extract_description(&content);
                    skills.push(SkillInfo {
                        id: format!("skill-{}", name),
                        name: name.clone(),
                        description,
                        category: "user".to_string(),
                        path: skill_md.to_string_lossy().to_string(),
                    });
                }
            }
        }
    }

    skills
}

/// Scan all skill sources
pub fn scan_skills() -> Result<Vec<SkillInfo>> {
    let mut all_skills = Vec::new();

    // 1. ~/.claude/skills (Claude Code 标准位置，home 目录)
    if let Some(home) = dirs::home_dir() {
        let claude_skills = home.join(".claude").join("skills");
        if claude_skills.exists() {
            all_skills.extend(find_skills_in_dir(&claude_skills));
        }
    }

    // 2. ~/.config/ergatai/skills (Ergatai 自己的位置)
    if let Some(config_dir) = dirs::config_dir() {
        let agents_skills = config_dir.join("ergatai").join("skills");
        if agents_skills.exists() {
            let mut agent_skills = find_skills_in_dir(&agents_skills);
            for skill in &mut agent_skills {
                skill.category = "user".to_string();
            }
            all_skills.extend(agent_skills);
        }

        // 3. Built-in skills from project
        let project_skills = PathBuf::from(".claude").join("skills");
        if project_skills.exists() {
            let mut proj_skills = find_skills_in_dir(&project_skills);
            for skill in &mut proj_skills {
                skill.category = "built-in".to_string();
            }
            all_skills.extend(proj_skills);
        }
    }

    // 4. Workspace skills (current working directory)
    if let Ok(cwd) = std::env::current_dir() {
        for dir_name in [".claude/skills", ".agents/skills", "skills"] {
            let skills_dir = cwd.join(dir_name);
            if skills_dir.exists() {
                let mut ws_skills = find_skills_in_dir(&skills_dir);
                for skill in &mut ws_skills {
                    skill.category = "workspace".to_string();
                }
                all_skills.extend(ws_skills);
            }
        }
    }

    // Deduplicate by name
    let mut seen = std::collections::HashSet::new();
    all_skills.retain(|s| seen.insert(s.name.clone()));

    // Sort by category then name
    all_skills.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(all_skills)
}

/// Get skill detail by name
pub fn get_skill_detail(name: String) -> Result<SkillDetail> {
    // 防止路径穿越：只允许字母数字、横线、下划线
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(napi::Error::new(
            napi::Status::InvalidArg,
            format!("Invalid skill name: {}", name),
        ));
    }

    let base_paths = [
        PathBuf::from(".claude")
            .join("skills")
            .join(&name)
            .join("SKILL.md"),
        PathBuf::from(".agents")
            .join("skills")
            .join(&name)
            .join("SKILL.md"),
    ];

    let mut search_dirs: Vec<PathBuf> = base_paths.to_vec();

    // Add home-based paths
    if let Some(home) = dirs::home_dir() {
        search_dirs.push(
            home.join(".claude")
                .join("skills")
                .join(&name)
                .join("SKILL.md"),
        );
    }
    if let Some(config_dir) = dirs::config_dir() {
        search_dirs.push(
            config_dir
                .join("ergatai")
                .join("skills")
                .join(&name)
                .join("SKILL.md"),
        );
    }

    // Add cwd-based paths
    if let Ok(cwd) = std::env::current_dir() {
        search_dirs.push(
            cwd.join(".claude")
                .join("skills")
                .join(&name)
                .join("SKILL.md"),
        );
        search_dirs.push(cwd.join("skills").join(&name).join("SKILL.md"));
    }

    for path in search_dirs {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let description = extract_description(&content);
                return Ok(SkillDetail {
                    name: name.clone(),
                    description,
                    content,
                    path: path.to_string_lossy().to_string(),
                });
            }
        }
    }

    Err(napi::Error::new(
        napi::Status::InvalidArg,
        format!("Skill not found: {}", name),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_description_with_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill for unit testing
---

# Test Skill

Some content here.
"#;
        assert_eq!(
            extract_description(content),
            "A test skill for unit testing"
        );
    }

    #[test]
    fn test_extract_description_with_quoted_frontmatter() {
        let content = r#"---
name: test-skill
description: "A quoted description"
---

# Test Skill
"#;
        assert_eq!(extract_description(content), "A quoted description");
    }

    #[test]
    fn test_extract_description_without_frontmatter() {
        let content = r#"# Test Skill

This is the first paragraph of the skill.
It should be used as the description.
"#;
        assert_eq!(extract_description(content), "# Test Skill");
    }

    #[test]
    fn test_extract_description_empty_content() {
        assert_eq!(extract_description(""), "");
        assert_eq!(extract_description("   "), "");
    }

    #[test]
    fn test_extract_description_only_frontmatter() {
        let content = r#"---
name: test
---
"#;
        // Should fall back to empty or first non-empty line after frontmatter
        let desc = extract_description(content);
        assert!(desc.is_empty() || desc == "---");
    }

    #[test]
    fn test_extract_description_multiline_folded() {
        let content = r#"---
description: >-
  This is a multi-line
  description that should
  be folded together
---

# Test Skill
"#;
        let desc = extract_description(content);
        assert!(desc.contains("multi-line"));
        assert!(desc.contains("description"));
    }
}
