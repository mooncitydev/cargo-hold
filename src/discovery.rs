use std::path::{Path, PathBuf};

use git2::{Index, Repository};

use crate::error::HoldError;

/// Files discovered from the Git index.
pub(crate) struct TrackedFiles {
    /// Repository root path.
    pub(crate) repo_root: PathBuf,
    /// Tracked regular files that can be processed.
    pub(crate) files: Vec<PathBuf>,
    /// Number of tracked symbolic links skipped intentionally.
    pub(crate) symlink_count: usize,
    /// Tracked paths that could not be accessed on disk.
    pub(crate) inaccessible_files: Vec<PathBuf>,
}

impl TrackedFiles {
    /// Number of tracked paths that should have been processable.
    pub(crate) fn processable_count(&self) -> usize {
        self.files.len() + self.inaccessible_files.len()
    }
}

/// Discovers all tracked files in the Git repository.
///
/// This function uses the Git index to find all files that are tracked by Git,
/// automatically respecting `.gitignore` rules. The returned paths are relative
/// to the repository root. Symbolic links tracked by Git are included in the
/// results but can be filtered by the caller if needed.
///
/// # Arguments
///
/// * `repo_path` - A path within the Git repository (will search upward for the
///   repo root)
///
/// # Returns
///
/// A tuple containing:
/// - The repository root path (absolute)
/// - A vector of file paths relative to the repository root
/// - A count of skipped symbolic links
///
/// # Errors
///
/// Returns an error if:
/// - No Git repository is found at or above the given path
/// - The Git index cannot be accessed
/// - Any file path contains invalid UTF-8
pub(crate) fn discover_tracked_files(repo_path: &Path) -> Result<TrackedFiles, HoldError> {
    // Open the repository, searching upward from the given path
    let repo = Repository::discover(repo_path)
        .map_err(|_| HoldError::RepoNotFound(repo_path.to_path_buf()))?;

    // Get the repository root
    let repo_root = repo
        .workdir()
        .ok_or_else(|| HoldError::RepoNotFound(repo_path.to_path_buf()))?
        .to_path_buf();

    // Access the Git index
    let index = repo.index().map_err(HoldError::IndexError)?;

    // Collect all tracked file paths, filtering out symlinks
    let (tracked_files, symlink_count, inaccessible_files) =
        collect_index_paths(&index, &repo_root)?;

    Ok(TrackedFiles {
        repo_root,
        files: tracked_files,
        symlink_count,
        inaccessible_files,
    })
}

/// Extract all file paths from the Git index, filtering out symlinks
fn collect_index_paths(
    index: &Index,
    repo_root: &Path,
) -> Result<(Vec<PathBuf>, usize, Vec<PathBuf>), HoldError> {
    let mut paths = Vec::new();
    let mut symlink_count = 0;
    let mut inaccessible_paths = Vec::new();

    for entry in index.iter() {
        // Skip submodules (mode 160000) - they appear as directories in the filesystem
        // but are special entries in git that we can't set timestamps on
        if entry.mode == 0o160000 {
            continue;
        }

        // Get the path from the index entry - it's already relative to repo root
        let path = entry.path;

        // Convert path bytes to string and then to PathBuf
        let path_str = std::str::from_utf8(&path).map_err(|e| HoldError::InvalidPath {
            message: format!("Invalid UTF-8 in path: {e}"),
        })?;

        let path_buf = PathBuf::from(path_str);

        // Check if the file is a symlink in the actual filesystem
        let full_path = repo_root.join(&path_buf);
        match std::fs::symlink_metadata(&full_path) {
            Ok(metadata) => {
                if metadata.is_symlink() {
                    symlink_count += 1;
                    continue; // Skip symlinks
                }
            }
            Err(_) => {
                inaccessible_paths.push(path_buf);
                continue; // Report files we can't access to the caller.
            }
        }

        paths.push(path_buf);
    }

    Ok((paths, symlink_count, inaccessible_paths))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn setup_test_repo() -> (TempDir, Repository) {
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        // Add to index
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("test.txt")).unwrap();
        index.write().unwrap();

        (temp_dir, repo)
    }

    #[test]
    fn test_discover_tracked_files() {
        let (temp_dir, _repo) = setup_test_repo();

        let discovered = discover_tracked_files(temp_dir.path()).unwrap();
        // On macOS, /var is a symlink to /private/var, so we need to canonicalize paths
        assert_eq!(
            discovered.repo_root.canonicalize().unwrap(),
            temp_dir.path().canonicalize().unwrap()
        );
        assert_eq!(discovered.files.len(), 1);
        assert!(discovered.files[0].ends_with("test.txt"));
        assert_eq!(discovered.symlink_count, 0);
        assert!(discovered.inaccessible_files.is_empty());
        assert_eq!(discovered.processable_count(), 1);
    }

    #[test]
    fn test_discover_tracked_files_reports_missing_tracked_files() {
        let (temp_dir, _repo) = setup_test_repo();
        fs::remove_file(temp_dir.path().join("test.txt")).unwrap();

        let discovered = discover_tracked_files(temp_dir.path()).unwrap();

        assert!(discovered.files.is_empty());
        assert_eq!(
            discovered.inaccessible_files,
            vec![PathBuf::from("test.txt")]
        );
        assert_eq!(discovered.processable_count(), 1);
    }

    #[test]
    fn test_repo_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let result = discover_tracked_files(temp_dir.path());
        assert!(matches!(result, Err(HoldError::RepoNotFound { .. })));
    }
}
