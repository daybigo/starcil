use starcil_worktree::{
    CreateOptions, SystemCommandRunner, WorktreeManager, WorktreeSelector,
};
use std::{fs, path::Path, process::Command};

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git must be installed for the real repository integration test");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn create_list_status_and_remove_in_real_repository() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Starcil Test"]);
    git(
        &repository,
        &["config", "user.email", "starcil-test@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "-m", "initial"]);

    let worktrees_directory = temp.path().join("worktrees");
    let worktree_path = worktrees_directory.join("feature-d1");
    let manager = WorktreeManager::new(
        SystemCommandRunner,
        &repository,
        &worktrees_directory,
    );

    let created = manager
        .create(CreateOptions {
            branch: Some("feature/d1".to_owned()),
            base: Some("HEAD".to_owned()),
            path: Some(worktree_path.clone()),
            label: None,
        })
        .unwrap();
    assert_eq!(created.branch.as_deref(), Some("feature/d1"));
    assert!(worktree_path.is_dir());

    let listed = manager.list().unwrap();
    assert!(listed
        .iter()
        .any(|worktree| worktree.branch.as_deref() == Some("feature/d1")));
    assert_eq!(
        manager
            .current_branch(Some(&worktree_path))
            .unwrap()
            .as_deref(),
        Some("feature/d1")
    );

    fs::write(worktree_path.join("untracked.txt"), "dirty\n").unwrap();
    let status = manager.status(Some(&worktree_path)).unwrap();
    assert_eq!(status.untracked, 1);
    assert!(status.is_dirty());

    let removed = manager
        .remove(WorktreeSelector::Path(worktree_path.clone()), true)
        .unwrap();
    assert_eq!(removed.branch.as_deref(), Some("feature/d1"));
    assert!(!worktree_path.exists());
    assert!(!manager
        .list()
        .unwrap()
        .iter()
        .any(|worktree| worktree.branch.as_deref() == Some("feature/d1")));
}
