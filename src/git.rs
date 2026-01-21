use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};
use git2::{Delta, DiffOptions, Repository};

#[derive(Debug, Clone, PartialEq)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl fmt::Display for ChangeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChangeType::Added => write!(f, "A"),
            ChangeType::Modified => write!(f, "M"),
            ChangeType::Deleted => write!(f, "D"),
            ChangeType::Renamed => write!(f, "R"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub change_type: ChangeType,
}

impl ChangedFile {
    pub fn new(path: String, change_type: ChangeType) -> Self {
        Self { path, change_type }
    }
}

pub fn get_changed_files(repo_path: &Path, base_ref: &str) -> Result<Vec<ChangedFile>> {
    let repo = Repository::discover(repo_path).with_context(|| {
        format!(
            "Failed to find git repository at or above '{}'",
            repo_path.display()
        )
    })?;

    let repo_root = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("Repository has no working directory (bare repository)"))?;

    // Resolve the base reference to a commit
    let base_obj = repo.revparse_single(base_ref).with_context(|| {
        format!(
            "Could not resolve git reference '{}'. Ensure it exists.",
            base_ref
        )
    })?;

    let base_commit = base_obj
        .peel_to_commit()
        .with_context(|| format!("Reference '{}' does not point to a commit", base_ref))?;

    let head_ref = repo
        .head()
        .with_context(|| "Failed to get HEAD reference")?;
    let head_commit = head_ref
        .peel_to_commit()
        .with_context(|| "HEAD does not point to a commit")?;

    let head_tree = head_commit
        .tree()
        .with_context(|| "Failed to get tree from HEAD commit")?;

    // Find the merge-base (common ancestor) between HEAD and base
    // This ensures we only get files changed in the current branch,
    // regardless of whether the local base branch is up-to-date
    let merge_base_oid = repo
        .merge_base(head_commit.id(), base_commit.id())
        .with_context(|| {
            format!(
                "Could not find merge-base between HEAD and '{}'. Ensure the branches share common history.",
                base_ref
            )
        })?;

    let merge_base_commit = repo
        .find_commit(merge_base_oid)
        .with_context(|| "Failed to find merge-base commit")?;

    let merge_base_tree = merge_base_commit
        .tree()
        .with_context(|| "Failed to get tree from merge-base commit")?;

    let mut diff_opts = DiffOptions::new();
    diff_opts.include_untracked(false);

    let diff = repo
        .diff_tree_to_tree(Some(&merge_base_tree), Some(&head_tree), Some(&mut diff_opts))
        .with_context(|| "Failed to compute diff between merge-base and HEAD")?;

    let mut changed_files = Vec::new();

    diff.foreach(
        &mut |delta, _| {
            let change_type = match delta.status() {
                Delta::Added => ChangeType::Added,
                Delta::Deleted => ChangeType::Deleted,
                Delta::Modified => ChangeType::Modified,
                Delta::Renamed => ChangeType::Renamed,
                Delta::Copied => ChangeType::Added,
                _ => return true, // Skip other types
            };

            let file_path = if delta.status() == Delta::Deleted {
                delta.old_file().path()
            } else {
                delta.new_file().path()
            };

            if let Some(path) = file_path {
                let absolute_path = repo_root.join(path);
                let path_str = absolute_path.to_string_lossy().to_string();
                changed_files.push(ChangedFile::new(path_str, change_type));
            }

            true
        },
        None,
        None,
        None,
    )
    .with_context(|| "Failed to iterate over diff")?;

    Ok(changed_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use std::fs;
    use tempfile::tempdir;

    fn create_commit(
        repo: &Repository,
        message: &str,
        parent: Option<&git2::Commit>,
    ) -> git2::Oid {
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        let mut index = repo.index().unwrap();
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let parents: Vec<&git2::Commit> = parent.into_iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    #[test]
    fn test_change_type_display() {
        assert_eq!(format!("{}", ChangeType::Added), "A");
        assert_eq!(format!("{}", ChangeType::Modified), "M");
        assert_eq!(format!("{}", ChangeType::Deleted), "D");
        assert_eq!(format!("{}", ChangeType::Renamed), "R");
    }

    #[test]
    fn test_changed_file_new() {
        let cf = ChangedFile::new("/path/to/file.ts".to_string(), ChangeType::Modified);
        assert_eq!(cf.path, "/path/to/file.ts");
        assert_eq!(cf.change_type, ChangeType::Modified);
    }

    #[test]
    fn test_get_changed_files_uses_merge_base() {
        // This test verifies that get_changed_files uses merge-base
        // and only returns files changed in the current branch,
        // not files changed in main after the branch diverged.
        //
        // Setup:
        //   main:    A --- B --- C (main moves forward)
        //             \
        //   feature:   D --- E (HEAD)
        //
        // Expected: Only files from D and E should be returned,
        // not files from B and C.

        let temp = tempdir().unwrap();
        let repo = Repository::init(temp.path()).unwrap();

        // Configure repo to avoid warnings
        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config().unwrap().set_str("user.email", "test@test.com").unwrap();

        // Commit A: Initial commit on main
        fs::write(temp.path().join("base.txt"), "base content").unwrap();
        let commit_a_oid = create_commit(&repo, "Initial commit A", None);
        let commit_a = repo.find_commit(commit_a_oid).unwrap();

        // Create main branch pointing to A
        repo.branch("main", &commit_a, false).unwrap();

        // Create feature branch from A
        repo.branch("feature", &commit_a, false).unwrap();

        // Switch to feature branch
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

        // Commit D: Add feature file on feature branch
        fs::write(temp.path().join("feature.txt"), "feature content").unwrap();
        let commit_d_oid = create_commit(&repo, "Feature commit D", Some(&commit_a));
        let commit_d = repo.find_commit(commit_d_oid).unwrap();

        // Commit E: Modify feature file on feature branch
        fs::write(temp.path().join("feature.txt"), "modified feature content").unwrap();
        fs::write(temp.path().join("feature2.txt"), "another feature file").unwrap();
        create_commit(&repo, "Feature commit E", Some(&commit_d));

        // Now switch to main and add commits B and C
        repo.set_head("refs/heads/main").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

        // Commit B: Add main-only file
        fs::write(temp.path().join("main_only.txt"), "main only content").unwrap();
        let commit_b_oid = create_commit(&repo, "Main commit B", Some(&commit_a));
        let commit_b = repo.find_commit(commit_b_oid).unwrap();

        // Commit C: Another main-only change
        fs::write(temp.path().join("main_only2.txt"), "another main file").unwrap();
        create_commit(&repo, "Main commit C", Some(&commit_b));

        // Switch back to feature branch
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

        // Get changed files comparing feature branch to main
        let changed = get_changed_files(temp.path(), "main").unwrap();

        // Extract just the filenames for easier assertion
        let changed_names: Vec<&str> = changed
            .iter()
            .map(|cf| cf.path.rsplit('/').next().unwrap())
            .collect();

        // Should contain feature branch files
        assert!(changed_names.contains(&"feature.txt"), "Should contain feature.txt");
        assert!(changed_names.contains(&"feature2.txt"), "Should contain feature2.txt");

        // Should NOT contain main-only files (this is the key assertion)
        assert!(!changed_names.contains(&"main_only.txt"), "Should NOT contain main_only.txt");
        assert!(!changed_names.contains(&"main_only2.txt"), "Should NOT contain main_only2.txt");

        // Should have exactly 2 changed files
        assert_eq!(changed.len(), 2, "Should have exactly 2 changed files");
    }

    #[test]
    fn test_get_changed_files_linear_history() {
        // Test with linear history (no divergence)
        // main: A --- B --- C (HEAD)
        // Comparing to A should show files from B and C

        let temp = tempdir().unwrap();
        let repo = Repository::init(temp.path()).unwrap();

        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config().unwrap().set_str("user.email", "test@test.com").unwrap();

        // Commit A
        fs::write(temp.path().join("file_a.txt"), "content a").unwrap();
        let commit_a_oid = create_commit(&repo, "Commit A", None);
        let commit_a = repo.find_commit(commit_a_oid).unwrap();

        // Create a tag at commit A to use as base reference
        repo.tag_lightweight("v1.0", commit_a.as_object(), false).unwrap();

        // Commit B
        fs::write(temp.path().join("file_b.txt"), "content b").unwrap();
        let commit_b_oid = create_commit(&repo, "Commit B", Some(&commit_a));
        let commit_b = repo.find_commit(commit_b_oid).unwrap();

        // Commit C
        fs::write(temp.path().join("file_c.txt"), "content c").unwrap();
        create_commit(&repo, "Commit C", Some(&commit_b));

        let changed = get_changed_files(temp.path(), "v1.0").unwrap();

        let changed_names: Vec<&str> = changed
            .iter()
            .map(|cf| cf.path.rsplit('/').next().unwrap())
            .collect();

        assert!(changed_names.contains(&"file_b.txt"));
        assert!(changed_names.contains(&"file_c.txt"));
        assert!(!changed_names.contains(&"file_a.txt"));
        assert_eq!(changed.len(), 2);
    }

    #[test]
    fn test_get_changed_files_detects_change_types() {
        let temp = tempdir().unwrap();
        let repo = Repository::init(temp.path()).unwrap();

        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config().unwrap().set_str("user.email", "test@test.com").unwrap();

        // Initial commit with a file
        fs::write(temp.path().join("existing.txt"), "original").unwrap();
        fs::write(temp.path().join("to_delete.txt"), "will be deleted").unwrap();
        let commit_a_oid = create_commit(&repo, "Initial", None);
        let commit_a = repo.find_commit(commit_a_oid).unwrap();

        repo.tag_lightweight("base", commit_a.as_object(), false).unwrap();

        // Second commit: modify, delete, and add
        fs::write(temp.path().join("existing.txt"), "modified").unwrap();
        fs::remove_file(temp.path().join("to_delete.txt")).unwrap();
        fs::write(temp.path().join("new_file.txt"), "new content").unwrap();

        // Need to explicitly remove deleted file from index
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new("to_delete.txt")).unwrap();
        index.add_path(Path::new("existing.txt")).unwrap();
        index.add_path(Path::new("new_file.txt")).unwrap();
        index.write().unwrap();

        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "Changes", &tree, &[&commit_a]).unwrap();

        let changed = get_changed_files(temp.path(), "base").unwrap();

        let find_change = |name: &str| -> Option<&ChangedFile> {
            changed.iter().find(|cf| cf.path.ends_with(name))
        };

        assert_eq!(find_change("existing.txt").unwrap().change_type, ChangeType::Modified);
        assert_eq!(find_change("to_delete.txt").unwrap().change_type, ChangeType::Deleted);
        assert_eq!(find_change("new_file.txt").unwrap().change_type, ChangeType::Added);
        assert_eq!(changed.len(), 3);
    }
}
