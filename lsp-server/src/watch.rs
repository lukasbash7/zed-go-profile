use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// How many poll cycles between full re-discovery of profile files.
/// Between re-discoveries, only the known files are stat-checked (cheap).
pub const REDISCOVER_EVERY_N_CYCLES: u32 = 6;

/// Tracks file modification times to detect changes.
pub struct FileWatcher {
    /// Last known mtime for each tracked file.
    mtimes: HashMap<PathBuf, SystemTime>,
    /// Counter for re-discovery cycles.
    cycle: u32,
}

impl FileWatcher {
    pub fn new() -> Self {
        Self {
            mtimes: HashMap::new(),
            cycle: 0,
        }
    }

    /// Seed the watcher with the initial set of files discovered at startup.
    /// This records their mtimes without reporting changes, so the first
    /// poll cycle won't trigger a spurious reload.
    pub fn seed(&mut self, files: &[PathBuf]) {
        self.mtimes.clear();
        for file in files {
            let mtime = file
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            self.mtimes.insert(file.clone(), mtime);
        }
    }

    /// Check only the already-known files for mtime changes.
    /// This is very cheap: just one stat() per known profile file.
    /// Returns true if any known file was modified or deleted.
    pub fn check_known_files(&mut self) -> bool {
        let mut changed = false;

        // Check each known file's current mtime.
        self.mtimes.retain(|file, old_mtime| {
            match file.metadata().ok().and_then(|m| m.modified().ok()) {
                Some(mtime) => {
                    if *old_mtime != mtime {
                        changed = true;
                        *old_mtime = mtime;
                    }
                    true // keep in map
                }
                None => {
                    // File was deleted.
                    changed = true;
                    false // remove from map
                }
            }
        });

        changed
    }

    /// Returns true if it's time for a full re-discovery of profile files.
    /// Call this once per poll cycle to decide whether to run the expensive glob.
    pub fn should_rediscover(&mut self) -> bool {
        self.cycle += 1;
        if self.cycle >= REDISCOVER_EVERY_N_CYCLES {
            self.cycle = 0;
            true
        } else {
            false
        }
    }

    /// Check a list of files for changes since last check.
    /// Returns true if any file was added, removed, or modified.
    /// This is used after a full re-discovery to detect new/removed files.
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
    use std::time::Duration;

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
        watcher.check_for_changes(std::slice::from_ref(&file));
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
        watcher.check_for_changes(std::slice::from_ref(&file1));

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
        watcher.check_for_changes(std::slice::from_ref(&file));

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

    #[test]
    fn test_seed_prevents_spurious_first_change() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.seed(std::slice::from_ref(&file));

        // After seeding, check_known_files should report no changes.
        assert!(!watcher.check_known_files());

        // And check_for_changes with same files should also report no changes.
        assert!(!watcher.check_for_changes(std::slice::from_ref(&file)));
    }

    #[test]
    fn test_check_known_files_detects_modification() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data1").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.seed(std::slice::from_ref(&file));

        // Modify the file.
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&file, b"data2_longer").unwrap();

        assert!(watcher.check_known_files());

        // Second check after update: no more changes.
        assert!(!watcher.check_known_files());
    }

    #[test]
    fn test_check_known_files_detects_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.seed(std::slice::from_ref(&file));

        // Delete the file.
        fs::remove_file(&file).unwrap();
        assert!(watcher.check_known_files());
    }

    #[test]
    fn test_should_rediscover_cycle() {
        let mut watcher = FileWatcher::new();

        // Should not rediscover on the first N-1 cycles.
        for _ in 0..REDISCOVER_EVERY_N_CYCLES - 1 {
            assert!(!watcher.should_rediscover());
        }

        // Should rediscover on the Nth cycle.
        assert!(watcher.should_rediscover());

        // Counter resets — next N-1 cycles are false again.
        assert!(!watcher.should_rediscover());
    }
}
