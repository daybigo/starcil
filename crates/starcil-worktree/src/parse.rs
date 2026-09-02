use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked_reason: Option<String>,
    pub prunable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RepoStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub detached: bool,
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

impl RepoStatus {
    pub const fn dirty_count(&self) -> u32 {
        self.staged + self.unstaged + self.untracked + self.conflicted
    }

    pub const fn is_dirty(&self) -> bool {
        self.dirty_count() > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("worktree porcelain block is missing its `worktree` path")]
    MissingWorktreePath,
    #[error("invalid branch divergence header `{0}`")]
    InvalidBranchDivergence(String),
}

#[derive(Default)]
struct WorktreeBuilder {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<String>,
    detached: bool,
    bare: bool,
    locked_reason: Option<String>,
    prunable_reason: Option<String>,
}

impl WorktreeBuilder {
    fn has_content(&self) -> bool {
        self.path.is_some()
            || self.head.is_some()
            || self.branch.is_some()
            || self.detached
            || self.bare
            || self.locked_reason.is_some()
            || self.prunable_reason.is_some()
    }

    fn finish(self) -> Result<WorktreeInfo, ParseError> {
        Ok(WorktreeInfo {
            path: self.path.ok_or(ParseError::MissingWorktreePath)?,
            head: self.head,
            branch: self.branch,
            detached: self.detached,
            bare: self.bare,
            locked_reason: self.locked_reason,
            prunable_reason: self.prunable_reason,
        })
    }
}

pub fn parse_worktree_porcelain(output: &str) -> Result<Vec<WorktreeInfo>, ParseError> {
    let mut worktrees = Vec::new();
    let mut current = WorktreeBuilder::default();

    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            if current.has_content() {
                worktrees.push(std::mem::take(&mut current).finish()?);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if current.has_content() {
                worktrees.push(std::mem::take(&mut current).finish()?);
            }
            current.path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current.head = Some(head.to_owned());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current.branch = Some(short_branch(branch).to_owned());
        } else if line == "detached" {
            current.detached = true;
        } else if line == "bare" {
            current.bare = true;
        } else if let Some(reason) = line.strip_prefix("locked") {
            current.locked_reason = Some(reason.trim_start().to_owned());
        } else if let Some(reason) = line.strip_prefix("prunable") {
            current.prunable_reason = Some(reason.trim_start().to_owned());
        }
    }

    if current.has_content() {
        worktrees.push(current.finish()?);
    }
    Ok(worktrees)
}

pub fn parse_status_porcelain_v2(output: &str) -> Result<RepoStatus, ParseError> {
    let mut status = RepoStatus::default();
    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        if let Some(branch) = line.strip_prefix("# branch.head ") {
            if branch == "(detached)" {
                status.detached = true;
                status.branch = None;
            } else {
                status.branch = Some(branch.to_owned());
            }
        } else if let Some(upstream) = line.strip_prefix("# branch.upstream ") {
            status.upstream = Some(upstream.to_owned());
        } else if let Some(divergence) = line.strip_prefix("# branch.ab ") {
            let mut fields = divergence.split_whitespace();
            let ahead = fields
                .next()
                .and_then(|field| field.strip_prefix('+'))
                .and_then(|value| value.parse::<u32>().ok());
            let behind = fields
                .next()
                .and_then(|field| field.strip_prefix('-'))
                .and_then(|value| value.parse::<u32>().ok());
            if fields.next().is_some() || ahead.is_none() || behind.is_none() {
                return Err(ParseError::InvalidBranchDivergence(divergence.to_owned()));
            }
            status.ahead = ahead.unwrap_or_default();
            status.behind = behind.unwrap_or_default();
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            count_xy(line, &mut status);
        } else if line.starts_with("u ") {
            status.conflicted = status.conflicted.saturating_add(1);
        } else if line.starts_with("? ") {
            status.untracked = status.untracked.saturating_add(1);
        }
    }
    Ok(status)
}

fn count_xy(line: &str, status: &mut RepoStatus) {
    let bytes = line.as_bytes();
    if bytes.len() < 4 {
        return;
    }
    if bytes[2] != b'.' {
        status.staged = status.staged.saturating_add(1);
    }
    if bytes[3] != b'.' {
        status.unstaged = status.unstaged.saturating_add(1);
    }
}

fn short_branch(branch: &str) -> &str {
    branch.strip_prefix("refs/heads/").unwrap_or(branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_worktrees_detached_and_future_keys() {
        let output = concat!(
            "worktree C:/repo with spaces\n",
            "HEAD 1111111111111111111111111111111111111111\n",
            "branch refs/heads/main\n",
            "future-key ignored\n\n",
            "worktree C:/repo-detached\n",
            "HEAD 2222222222222222222222222222222222222222\n",
            "detached\n",
            "locked maintenance\n\n",
        );
        let parsed = parse_worktree_porcelain(output).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, PathBuf::from("C:/repo with spaces"));
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert!(parsed[1].detached);
        assert_eq!(parsed[1].locked_reason.as_deref(), Some("maintenance"));
    }

    #[test]
    fn parses_branch_headers_and_dirty_counts() {
        let output = concat!(
            "# branch.oid abcdef\n",
            "# branch.head feature/worktrees\n",
            "# branch.upstream origin/feature/worktrees\n",
            "# branch.ab +3 -2\n",
            "1 M. N... 100644 100644 100644 abc abc staged.txt\n",
            "1 .M N... 100644 100644 100644 abc abc unstaged.txt\n",
            "2 MM N... 100644 100644 100644 abc abc R100 renamed.txt\trenamed-old.txt\n",
            "u UU N... 100644 100644 100644 100644 abc abc abc conflict.txt\n",
            "? untracked.txt\n",
            "! ignored.txt\n",
        );
        let parsed = parse_status_porcelain_v2(output).unwrap();
        assert_eq!(parsed.branch.as_deref(), Some("feature/worktrees"));
        assert_eq!(parsed.upstream.as_deref(), Some("origin/feature/worktrees"));
        assert_eq!((parsed.ahead, parsed.behind), (3, 2));
        assert_eq!((parsed.staged, parsed.unstaged), (2, 2));
        assert_eq!((parsed.untracked, parsed.conflicted), (1, 1));
        assert!(parsed.is_dirty());
    }

    #[test]
    fn parses_detached_status() {
        let parsed = parse_status_porcelain_v2("# branch.head (detached)\n").unwrap();
        assert!(parsed.detached);
        assert_eq!(parsed.branch, None);
    }
}
