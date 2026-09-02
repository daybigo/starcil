use crate::{
    parse_status_porcelain_v2, parse_worktree_porcelain, CommandInvocation, CommandOutput,
    CommandRunner, ParseError, RepoStatus, WorktreeInfo,
};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CreateOptions {
    pub branch: Option<String>,
    pub base: Option<String>,
    pub path: Option<PathBuf>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeSelector {
    Path(PathBuf),
    Branch(String),
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("could not run `{command}`: {source}")]
    Runner {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("`{command}` failed with exit code {exit_code:?}: {stderr}")]
    CommandFailed {
        command: String,
        exit_code: Option<i32>,
        stderr: String,
    },
    #[error("`{command}` returned non-UTF-8 output: {source}")]
    NonUtf8 {
        command: String,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("worktree not found: {0}")]
    NotFound(String),
    #[error("invalid worktree option: {0}")]
    InvalidOption(String),
}

pub struct WorktreeManager<R> {
    runner: R,
    repository: PathBuf,
    worktrees_directory: PathBuf,
}

impl<R: CommandRunner> WorktreeManager<R> {
    pub fn new(
        runner: R,
        repository: impl Into<PathBuf>,
        worktrees_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            runner,
            repository: repository.into(),
            worktrees_directory: worktrees_directory.into(),
        }
    }

    pub fn repository(&self) -> &Path {
        &self.repository
    }

    pub fn worktrees_directory(&self) -> &Path {
        &self.worktrees_directory
    }

    pub fn list(&self) -> Result<Vec<WorktreeInfo>, WorktreeError> {
        let output = self.run_git(
            ["worktree", "list", "--porcelain"],
            Some(&self.repository),
        )?;
        parse_worktree_porcelain(&output).map_err(Into::into)
    }

    pub fn create(&self, options: CreateOptions) -> Result<WorktreeInfo, WorktreeError> {
        validate_optional_name("branch", options.branch.as_deref())?;
        validate_optional_name("base", options.base.as_deref())?;
        validate_optional_name("label", options.label.as_deref())?;

        let path = options.path.unwrap_or_else(|| {
            let candidate = options
                .label
                .as_deref()
                .or(options.branch.as_deref())
                .unwrap_or("worktree");
            self.worktrees_directory.join(sanitize_path_component(candidate))
        });

        let mut args = vec![OsString::from("worktree"), OsString::from("add")];
        if let Some(branch) = options.branch {
            args.push(OsString::from("-b"));
            args.push(OsString::from(branch));
        }
        args.push(path.as_os_str().to_owned());
        if let Some(base) = options.base {
            args.push(OsString::from(base));
        }
        self.run_git_os(args, Some(&self.repository))?;
        self.open(WorktreeSelector::Path(path))
    }

    pub fn open(&self, selector: WorktreeSelector) -> Result<WorktreeInfo, WorktreeError> {
        let worktrees = self.list()?;
        worktrees
            .into_iter()
            .find(|worktree| match &selector {
                WorktreeSelector::Path(path) => paths_equal(&worktree.path, path),
                WorktreeSelector::Branch(branch) => worktree
                    .branch
                    .as_deref()
                    .map(|current| current == short_branch(branch))
                    .unwrap_or(false),
            })
            .ok_or_else(|| WorktreeError::NotFound(selector_label(&selector)))
    }

    pub fn open_path(&self, path: impl Into<PathBuf>) -> Result<WorktreeInfo, WorktreeError> {
        self.open(WorktreeSelector::Path(path.into()))
    }

    pub fn open_branch(&self, branch: impl Into<String>) -> Result<WorktreeInfo, WorktreeError> {
        self.open(WorktreeSelector::Branch(branch.into()))
    }

    pub fn remove(
        &self,
        selector: WorktreeSelector,
        force: bool,
    ) -> Result<WorktreeInfo, WorktreeError> {
        let worktree = self.open(selector)?;
        let mut args = vec![OsString::from("worktree"), OsString::from("remove")];
        if force {
            args.push(OsString::from("--force"));
        }
        args.push(worktree.path.as_os_str().to_owned());
        self.run_git_os(args, Some(&self.repository))?;
        Ok(worktree)
    }

    pub fn status(&self, worktree: Option<&Path>) -> Result<RepoStatus, WorktreeError> {
        let cwd = worktree.unwrap_or(&self.repository);
        let output = self.run_git(
            ["status", "--porcelain=v2", "--branch"],
            Some(cwd),
        )?;
        parse_status_porcelain_v2(&output).map_err(Into::into)
    }

    pub fn current_branch(&self, worktree: Option<&Path>) -> Result<Option<String>, WorktreeError> {
        Ok(self.status(worktree)?.branch)
    }

    fn run_git<const N: usize>(
        &self,
        args: [&str; N],
        cwd: Option<&Path>,
    ) -> Result<String, WorktreeError> {
        self.run_git_os(args.into_iter().map(OsString::from).collect(), cwd)
    }

    fn run_git_os(
        &self,
        args: Vec<OsString>,
        cwd: Option<&Path>,
    ) -> Result<String, WorktreeError> {
        let invocation = CommandInvocation {
            program: OsString::from("git"),
            args,
            cwd: cwd.map(Path::to_owned),
        };
        let command = display_invocation(&invocation);
        let output = self
            .runner
            .run(&invocation)
            .map_err(|source| WorktreeError::Runner {
                command: command.clone(),
                source,
            })?;
        decode_output(command, output)
    }
}

fn decode_output(command: String, output: CommandOutput) -> Result<String, WorktreeError> {
    if output.exit_code != Some(0) {
        return Err(WorktreeError::CommandFailed {
            command,
            exit_code: output.exit_code,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|source| WorktreeError::NonUtf8 { command, source })
}

fn display_invocation(invocation: &CommandInvocation) -> String {
    std::iter::once(invocation.program.as_os_str())
        .chain(invocation.args.iter().map(OsString::as_os_str))
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_optional_name(field: &str, value: Option<&str>) -> Result<(), WorktreeError> {
    if value.map(str::trim).is_some_and(str::is_empty) {
        return Err(WorktreeError::InvalidOption(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn sanitize_path_component(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_separator = false;
    for character in value.trim().chars() {
        let allowed = character.is_alphanumeric() || matches!(character, '-' | '_' | '.');
        if allowed {
            sanitized.push(character);
            previous_separator = false;
        } else if !previous_separator {
            sanitized.push('-');
            previous_separator = true;
        }
    }
    let sanitized = sanitized.trim_matches(['-', '.']);
    if sanitized.is_empty() {
        "worktree".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn short_branch(branch: &str) -> &str {
    branch.strip_prefix("refs/heads/").unwrap_or(branch)
}

fn selector_label(selector: &WorktreeSelector) -> String {
    match selector {
        WorktreeSelector::Path(path) => path.display().to_string(),
        WorktreeSelector::Branch(branch) => format!("branch {branch}"),
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    // Git prints a path in its long form; the caller may hold the 8.3 short
    // form (`C:\Users\RUNNER~1\...` on GitHub's Windows runners) or a
    // symlinked one. When both exist on disk, the real paths settle it.
    if let (Ok(left_real), Ok(right_real)) =
        (std::fs::canonicalize(left), std::fs::canonicalize(right))
    {
        if left_real == right_real {
            return true;
        }
    }
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_owned()
    };
    let left = normalize(left);
    let right = normalize(right);
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};

    #[test]
    fn paths_equal_uses_the_real_path_when_both_exist() {
        let dir = std::env::temp_dir().join(format!("starcil-paths-equal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The canonical form differs textually (`\\?\` prefix on Windows,
        // resolved symlinks elsewhere) yet names the same directory.
        let canonical = std::fs::canonicalize(&dir).unwrap();
        assert!(paths_equal(&dir, &canonical));
        assert!(paths_equal(&canonical, &dir));
        assert!(!paths_equal(&dir, &dir.join("other")));
        std::fs::remove_dir_all(&dir).unwrap();
        // Missing paths still compare by spelling.
        assert!(paths_equal(Path::new("C:/a/b/"), Path::new("C:\\a\\b")));
    }

    #[derive(Default)]
    struct RecordedRunner {
        calls: RefCell<Vec<CommandInvocation>>,
        outputs: RefCell<VecDeque<CommandOutput>>,
    }

    impl RecordedRunner {
        fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                outputs: RefCell::new(outputs.into_iter().collect()),
            }
        }
    }

    impl CommandRunner for RecordedRunner {
        fn run(&self, invocation: &CommandInvocation) -> io::Result<CommandOutput> {
            self.calls.borrow_mut().push(invocation.clone());
            self.outputs
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no recorded output"))
        }
    }

    fn args(invocation: &CommandInvocation) -> Vec<String> {
        invocation
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn list_uses_porcelain_output_in_repository() {
        let runner = RecordedRunner::with_outputs([CommandOutput::success(
            b"worktree C:/repo\nHEAD abc\nbranch refs/heads/main\n\n".to_vec(),
        )]);
        let manager = WorktreeManager::new(&runner, "C:/repo", "C:/worktrees");
        let listed = manager.list().unwrap();
        assert_eq!(listed[0].branch.as_deref(), Some("main"));
        let calls = runner.calls.borrow();
        assert_eq!(args(&calls[0]), ["worktree", "list", "--porcelain"]);
        assert_eq!(calls[0].cwd.as_deref(), Some(Path::new("C:/repo")));
    }

    #[test]
    fn create_builds_branch_base_and_default_path_then_returns_info() {
        let created_path = PathBuf::from("C:/worktrees").join("Sidebar-UX");
        let list_output = format!(
            "worktree {}\nHEAD def\nbranch refs/heads/feature/sidebar\n\n",
            created_path.display()
        );
        let runner = RecordedRunner::with_outputs([
            CommandOutput::success(Vec::new()),
            CommandOutput::success(list_output.into_bytes()),
        ]);
        let manager = WorktreeManager::new(&runner, "C:/repo", "C:/worktrees");
        let created = manager
            .create(CreateOptions {
                branch: Some("feature/sidebar".to_owned()),
                base: Some("main".to_owned()),
                label: Some("Sidebar UX".to_owned()),
                path: None,
            })
            .unwrap();
        assert_eq!(created.path, created_path);
        let calls = runner.calls.borrow();
        assert_eq!(
            args(&calls[0]),
            vec![
                "worktree".to_owned(),
                "add".to_owned(),
                "-b".to_owned(),
                "feature/sidebar".to_owned(),
                created_path.to_string_lossy().into_owned(),
                "main".to_owned(),
            ]
        );
        assert_eq!(args(&calls[1]), ["worktree", "list", "--porcelain"]);
    }

    #[test]
    fn remove_resolves_branch_and_passes_force() {
        let runner = RecordedRunner::with_outputs([
            CommandOutput::success(
                b"worktree C:/worktrees/feature\nHEAD def\nbranch refs/heads/feature\n\n"
                    .to_vec(),
            ),
            CommandOutput::success(Vec::new()),
        ]);
        let manager = WorktreeManager::new(&runner, "C:/repo", "C:/worktrees");
        let removed = manager
            .remove(WorktreeSelector::Branch("feature".to_owned()), true)
            .unwrap();
        assert_eq!(removed.path, PathBuf::from("C:/worktrees/feature"));
        let calls = runner.calls.borrow();
        assert_eq!(
            args(&calls[1]),
            ["worktree", "remove", "--force", "C:/worktrees/feature"]
        );
    }

    #[test]
    fn status_and_current_branch_use_porcelain_v2() {
        let output = b"# branch.head main\n? new.txt\n".to_vec();
        let runner = RecordedRunner::with_outputs([
            CommandOutput::success(output.clone()),
            CommandOutput::success(output),
        ]);
        let manager = WorktreeManager::new(&runner, "C:/repo", "C:/worktrees");
        let status = manager.status(None).unwrap();
        assert_eq!(status.untracked, 1);
        assert_eq!(manager.current_branch(None).unwrap().as_deref(), Some("main"));
        let calls = runner.calls.borrow();
        assert_eq!(
            args(&calls[0]),
            ["status", "--porcelain=v2", "--branch"]
        );
    }

    #[test]
    fn command_failure_preserves_exit_code_and_stderr() {
        let runner = RecordedRunner::with_outputs([CommandOutput {
            exit_code: Some(128),
            stdout: Vec::new(),
            stderr: b"fatal: not a git repository\n".to_vec(),
        }]);
        let manager = WorktreeManager::new(&runner, "C:/missing", "C:/worktrees");
        let error = manager.list().unwrap_err();
        assert!(matches!(
            error,
            WorktreeError::CommandFailed {
                exit_code: Some(128),
                ..
            }
        ));
    }
}
