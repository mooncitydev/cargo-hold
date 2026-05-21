//! Stow command implementation.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::discovery::discover_tracked_files;
use crate::error::{HoldError, Result};
use crate::hashing::{get_file_mtime_nanos, get_file_size, hash_file};
use crate::logging::Logger;
use crate::metadata::{load_metadata, save_metadata};
use crate::state::{FileState, StateMetadata};

/// Executes the stow command.
///
/// Scans all Git-tracked files, hashes them, and persists the state.
pub fn stow(metadata_path: &Path, verbose: u8, quiet: bool, working_dir: &Path) -> Result<()> {
    let log = Logger::new(verbose, quiet);
    log.verbose(1, "Stowing files in cargo hold...");

    let discovered = discover_tracked_files(working_dir)?;
    let total_processable = discovered.processable_count();
    let repo_root = discovered.repo_root;
    let tracked_files = discovered.files;
    let symlink_count = discovered.symlink_count;
    let inaccessible_files = discovered.inaccessible_files;

    log.verbose(1, format!("Found {} tracked files", tracked_files.len()));

    if !log.quiet() && symlink_count > 0 {
        eprintln!(
            "Note: Skipped {} symbolic link{} (not stored in metadata)",
            symlink_count,
            if symlink_count == 1 { "" } else { "s" }
        );
    }

    let mut errors = inaccessible_files.len();
    if errors > 0 && !log.quiet() {
        eprintln!("Warning: Failed to access {errors} tracked file(s)");
        for path in &inaccessible_files {
            log.verbose(
                1,
                format!("  Could not access tracked file: {}", path.display()),
            );
        }
    }

    let file_states: Vec<Result<FileState>> = tracked_files
        .par_iter()
        .map(|path| build_file_state(&repo_root, path))
        .collect();

    let mut new_metadata = StateMetadata::new();
    for result in file_states {
        match result {
            Ok(state) => {
                if let Err(e) = new_metadata.upsert(state) {
                    errors += 1;
                    if !log.quiet() {
                        eprintln!("Warning: Failed to add file to metadata: {e:?}");
                    }
                }
            }
            Err(e) => {
                errors += 1;
                if !log.quiet() {
                    eprintln!("Warning: Failed to analyze file: {e:?}");
                }
            }
        }
    }

    if errors > 0 {
        if !log.quiet() {
            eprintln!("Warning: Failed to analyze {errors} file(s)");
            if log.level() == 0 {
                eprintln!("Run with -v for more details");
            }
        }
        return Err(HoldError::PartialFileProcessing {
            failed: errors,
            total: total_processable,
        });
    }

    let existing_metadata = match load_metadata(metadata_path) {
        Ok(metadata) => Some(metadata),
        Err(HoldError::DeserializationError { .. }) => None,
        Err(err) => return Err(err),
    };

    if let Some(existing) = existing_metadata.as_ref() {
        new_metadata.gc_metrics = existing.gc_metrics.clone();
    }

    new_metadata.last_gc_mtime_nanos = existing_metadata
        .as_ref()
        .and_then(|existing| existing.last_gc_mtime_nanos);

    save_metadata(&new_metadata, metadata_path)?;

    if !log.quiet() {
        eprintln!("File scan complete:");
        eprintln!("  Files tracked: {}", tracked_files.len());
        eprintln!("  Metadata entries: {}", new_metadata.len());
        if errors > 0 {
            eprintln!("  Files skipped: {errors} (errors)");
        }
        eprintln!("  Metadata saved to: {}", metadata_path.display());

        if let Ok(metadata) = std::fs::metadata(metadata_path) {
            eprintln!("  Metadata size: {} KB", metadata.len() / 1024);
        }
    }

    Ok(())
}

fn build_file_state(repo_root: &Path, path: &PathBuf) -> Result<FileState> {
    let full_path = repo_root.join(path);
    let size = get_file_size(&full_path)?;
    let hash = hash_file(&full_path)?;
    let mtime_nanos = get_file_mtime_nanos(&full_path)?;

    Ok(FileState {
        path: path.clone(),
        size,
        hash,
        mtime_nanos,
    })
}
