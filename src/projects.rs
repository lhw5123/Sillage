//! Discovers local git working copies to show as projects in the sidebar.
//!
//! A directory with a `.git` entry is a project. Recursion stops there so
//! nested modules are not listed as separate workspaces.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::matching::contains_ignoring_case;

/// One user message and the agent reply it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub prompt: String,
    pub output: String,
}

impl Turn {
    /// True when the message or the agent reply contains `query`.
    pub fn matches_query(&self, query: &str) -> bool {
        contains_ignoring_case(&self.prompt, query) || contains_ignoring_case(&self.output, query)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub branch: String,
    pub turns: Vec<Turn>,
    /// Unix timestamp (seconds) when the session was created.
    pub created_at: u64,
}

impl Task {
    pub fn new(id: String, title: String, prompt: String, branch: String) -> Self {
        Self {
            id,
            title,
            branch,
            turns: vec![Turn {
                prompt,
                output: String::new(),
            }],
            created_at: now_secs(),
        }
    }

    /// Start another turn; its output stays empty until the agent replies.
    pub fn push_turn(&mut self, prompt: String) {
        self.turns.push(Turn {
            prompt,
            output: String::new(),
        });
    }

    pub fn last_turn_mut(&mut self) -> Option<&mut Turn> {
        self.turns.last_mut()
    }

    /// True when the session labels or anything in its transcript, including
    /// agent replies, contains `query`.
    pub fn matches_query(&self, query: &str) -> bool {
        contains_ignoring_case(&self.title, query)
            || contains_ignoring_case(&self.branch, query)
            || self.turns.iter().any(|turn| turn.matches_query(query))
    }

    /// Prompt for the pending turn, with earlier turns replayed as context.
    ///
    /// Agent CLIs run one-shot here, so the transcript travels with every
    /// follow-up to keep the session coherent.
    pub fn agent_prompt(&self) -> String {
        let Some((current, earlier)) = self.turns.split_last() else {
            return String::new();
        };
        if earlier.is_empty() {
            return current.prompt.clone();
        }
        let mut text = String::from("Earlier in this session:\n\n");
        for turn in earlier {
            text.push_str("User: ");
            text.push_str(turn.prompt.trim());
            text.push_str("\n\nAssistant: ");
            text.push_str(turn.output.trim());
            text.push_str("\n\n");
        }
        text.push_str("Now: ");
        text.push_str(current.prompt.trim());
        text
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Compact relative time label, e.g. "now", "5m", "2h", "3d".
pub fn format_relative_time(created_at: u64) -> String {
    let elapsed = now_secs().saturating_sub(created_at);
    if elapsed < 60 {
        "now".into()
    } else if elapsed < 3600 {
        format!("{}m", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{}h", elapsed / 3600)
    } else {
        format!("{}d", elapsed / 86_400)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub tasks: Vec<Task>,
    pub expanded: bool,
}

impl Project {
    /// Any local directory can be a workspace. Git metadata is optional.
    pub fn from_dir(path: PathBuf) -> Option<Self> {
        if !path.is_dir() {
            return None;
        }
        let path = fs::canonicalize(&path).unwrap_or(path);
        let name = path.file_name()?.to_string_lossy().into_owned();
        let branch = git_branch(&path).unwrap_or_else(|| "main".into());
        Some(Self {
            id: path.to_string_lossy().into_owned(),
            name,
            path,
            branch,
            tasks: Vec::new(),
            expanded: true,
        })
    }

    pub fn from_git_dir(path: PathBuf) -> Option<Self> {
        Self::from_dir(path)
    }

    /// True when the project name, its path, or any of its session transcripts
    /// contains `query` (case-insensitive). An empty query matches every project.
    pub fn matches_query(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        contains_ignoring_case(&self.name, query)
            || contains_ignoring_case(&self.path.to_string_lossy(), query)
            || self.tasks.iter().any(|task| task.matches_query(query))
    }

    /// Sessions shown in the sidebar before the user expands the project.
    pub fn visible_task_count(&self) -> usize {
        if self.expanded {
            self.tasks.len()
        } else {
            self.tasks.len().min(3)
        }
    }
}

/// Scan well-known developer folders plus the current directory.
pub fn scan() -> Vec<Project> {
    let mut projects = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for root in scan_roots() {
        collect_git_projects(&root, 0, 3, &mut projects, &mut seen);
    }

    projects.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    projects.truncate(40);
    projects
}

fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
        if let Some(parent) = roots[0].parent() {
            roots.push(parent.to_path_buf());
        }
    }
    if let Some(home) = dirs::home_dir() {
        for rel in [
            "workspace",
            "workspace/github",
            "src",
            "dev",
            "code",
            "projects",
        ] {
            let candidate = home.join(rel);
            if candidate.is_dir() {
                roots.push(candidate);
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn collect_git_projects(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<Project>,
    seen: &mut std::collections::HashSet<PathBuf>,
) {
    if depth > max_depth || out.len() >= 40 {
        return;
    }
    if dir.join(".git").exists() {
        let canonical = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if seen.insert(canonical.clone())
            && let Some(project) = Project::from_git_dir(canonical)
        {
            out.push(project);
        }
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && (name.starts_with('.') || name == "node_modules" || name == "target")
            {
                continue;
            }
            collect_git_projects(&path, depth + 1, max_depth, out, seen);
        }
    }
}

fn git_branch(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", path.to_str()?, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

pub fn title_from_prompt(prompt: &str) -> String {
    let compact: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 36 {
        compact
    } else {
        let mut title: String = compact.chars().take(33).collect();
        title.push('…');
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_truncates_long_prompts() {
        let title =
            title_from_prompt("a very long prompt that should be shortened for the sidebar");
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= 36);
    }

    #[test]
    fn follow_up_prompt_carries_the_transcript() {
        let mut task = Task::new("id".into(), "title".into(), "first".into(), "main".into());
        assert_eq!(task.agent_prompt(), "first");
        task.last_turn_mut().unwrap().output = "did it".into();
        task.push_turn("second".into());
        let prompt = task.agent_prompt();
        assert!(prompt.contains("User: first"));
        assert!(prompt.contains("Assistant: did it"));
        assert!(prompt.ends_with("Now: second"));
    }

    #[test]
    fn agent_output_is_searchable() {
        let mut task = Task::new("id".into(), "title".into(), "prompt".into(), "main".into());
        task.last_turn_mut().unwrap().output = "Rewrote the Tokenizer".into();
        assert!(task.matches_query("tokenizer"));
        assert!(!task.matches_query("tokenizers"));

        let mut project = Project::from_dir(std::env::temp_dir()).unwrap();
        project.tasks.push(task);
        assert!(project.matches_query("TOKENIZER"));
    }

    #[test]
    fn relative_time_labels() {
        let now = now_secs();
        assert_eq!(format_relative_time(now), "now");
        assert_eq!(format_relative_time(now - 120), "2m");
        assert_eq!(format_relative_time(now - 7200), "2h");
    }

    #[test]
    fn from_dir_uses_folder_name() {
        let dir = std::env::temp_dir().join(format!("sillage-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = Project::from_dir(dir.clone()).expect("folder should become a workspace");
        assert_eq!(
            project.name,
            dir.file_name().unwrap().to_string_lossy().as_ref()
        );
        assert!(project.matches_query(""));
        assert!(project.matches_query(&project.name.to_lowercase()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
