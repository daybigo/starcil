//! Git worktree operations implemented through an injectable command runner.

mod manager;
mod parse;
mod runner;

pub use manager::{CreateOptions, WorktreeError, WorktreeManager, WorktreeSelector};
pub use parse::{parse_status_porcelain_v2, parse_worktree_porcelain, ParseError, RepoStatus, WorktreeInfo};
pub use runner::{CommandInvocation, CommandOutput, CommandRunner, SystemCommandRunner};
