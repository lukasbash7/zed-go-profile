use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Tracks file modification times to detect changes.
pub struct FileWatcher {
    /// Last known mtime for each tracked file.
    mtimes: HashMap<PathBuf, SystemTime>,
}

impl FileWatcher {
    pub fn new() -> Self {
        Self {
            mtimes: HashMap::new(),
        }
    }

    /// Check a list of files for changes since last check.
    /// Returns true if any file was added, removed, or modified.
    pub fn check_for_changes(&mut self, current_files: &[PathBuf]) -> bool {
        let mut changed = false;

        let mut new_mtimes = HashMap::new();

        for file in current_files {
            let mtime = file
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            if let Some(old_mtime) = self.mtimes.get(file) {
                if *old_mtime != mtime {
                    changed = true;
                }
            } else {
                // New file appeared.
                changed = true;
            }

            new_mtimes.insert(file.clone(), mtime);
        }

        // Check if any files were removed.
        if self.mtimes.len() != new_mtimes.len() {
            changed = true;
        }

        self.mtimes = new_mtimes;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_initial_check_detects_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        // First check: all files are new.
        assert!(watcher.check_for_changes(&[file]));
    }

    #[test]
    fn test_no_change_on_second_check() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file.clone()]);
        // Second check: no changes.
        assert!(!watcher.check_for_changes(&[file]));
    }

    #[test]
    fn test_detects_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("cpu.pprof");
        let file2 = dir.path().join("heap.pprof");
        fs::write(&file1, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file1.clone()]);

        // Add a new file.
        fs::write(&file2, b"data").unwrap();
        assert!(watcher.check_for_changes(&[file1, file2]));
    }

    #[test]
    fn test_detects_removed_file() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("cpu.pprof");
        let file2 = dir.path().join("heap.pprof");
        fs::write(&file1, b"data").unwrap();
        fs::write(&file2, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file1.clone(), file2.clone()]);

        // Remove file2.
        fs::remove_file(&file2).unwrap();
        assert!(watcher.check_for_changes(&[file1]));
    }

    #[test]
    fn test_detects_modified_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data1").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file.clone()]);

        // Modify the file (need to ensure mtime changes).
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&file, b"data2_longer").unwrap();

        assert!(watcher.check_for_changes(&[file]));
    }

    #[test]
    fn test_empty_files_list() {
        let mut watcher = FileWatcher::new();
        // First check with empty list: no change (nothing to detect).
        assert!(!watcher.check_for_changes(&[]));
    }
}
