use crate::config::PathMappingConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolves profile file paths to workspace-relative paths.
pub struct PathResolver {
    workspace_root: PathBuf,
    config: PathMappingConfig,
    /// Cache: profile path -> resolved workspace-relative path (or None if unresolvable).
    cache: HashMap<String, Option<String>>,
    /// Cached list of Go files in workspace (relative paths).
    go_files: Vec<String>,
}

impl PathResolver {
    pub fn new(workspace_root: PathBuf, config: PathMappingConfig) -> Self {
        let go_files = scan_go_files(&workspace_root);
        Self {
            workspace_root,
            config,
            cache: HashMap::new(),
            go_files,
        }
    }

    /// Clear cache and rescan workspace files.
    /// Call this when profile data is reloaded.
    #[allow(dead_code)]
    pub fn invalidate(&mut self) {
        self.cache.clear();
        self.go_files = scan_go_files(&self.workspace_root);
    }

    /// Resolve a profile path to a workspace-relative path.
    /// Returns None if the file cannot be found in the workspace.
    pub fn resolve(&mut self, profile_path: &str) -> Option<String> {
        if let Some(cached) = self.cache.get(profile_path) {
            return cached.clone();
        }

        let result = self.resolve_uncached(profile_path);
        self.cache.insert(profile_path.to_string(), result.clone());
        result
    }

    fn resolve_uncached(&self, profile_path: &str) -> Option<String> {
        // Strategy 1: Exact match.
        if self.workspace_root.join(profile_path).exists() {
            return Some(profile_path.to_string());
        }

        // Strategy 2: Configured trim prefix.
        if !self.config.trim_prefix.is_empty() {
            if let Some(trimmed) = profile_path.strip_prefix(&self.config.trim_prefix) {
                let trimmed = trimmed.trim_start_matches('/');
                if self.workspace_root.join(trimmed).exists() {
                    return Some(trimmed.to_string());
                }

                // Strategy 4: Source root prepend (after trimming).
                if !self.config.source_root.is_empty() {
                    let with_root = Path::new(&self.config.source_root).join(trimmed);
                    if let Some(s) = with_root.to_str() {
                        if self.workspace_root.join(s).exists() {
                            return Some(s.to_string());
                        }
                    }
                }
            }
        }

        // Strategy 3: Suffix match against workspace Go files.
        // Use path-component-level matching to avoid false positives
        // (e.g. "notmain.go" should not match "main.go").
        let profile = Path::new(profile_path);
        for go_file in &self.go_files {
            let go = Path::new(go_file.as_str());
            if profile.ends_with(go) {
                return Some(go_file.clone());
            }
        }

        None
    }
}

/// Scan workspace for .go files and return relative paths.
fn scan_go_files(workspace_root: &Path) -> Vec<String> {
    let pattern = workspace_root.join("**/*.go");
    let pattern_str = pattern.to_string_lossy();

    let mut files = Vec::new();
    if let Ok(paths) = glob::glob(&pattern_str) {
        for entry in paths.flatten() {
            if let Ok(relative) = entry.strip_prefix(workspace_root) {
                if let Some(s) = relative.to_str() {
                    files.push(s.to_string());
                }
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temp workspace with some Go files for testing.
    fn setup_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create directory structure.
        fs::create_dir_all(root.join("pkg/handler")).unwrap();
        fs::create_dir_all(root.join("cmd/server")).unwrap();

        // Create Go files.
        fs::write(root.join("main.go"), "package main").unwrap();
        fs::write(root.join("pkg/handler/handler.go"), "package handler").unwrap();
        fs::write(root.join("cmd/server/server.go"), "package main").unwrap();

        dir
    }

    #[test]
    fn test_exact_match() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        assert_eq!(resolver.resolve("main.go"), Some("main.go".to_string()));
        assert_eq!(
            resolver.resolve("pkg/handler/handler.go"),
            Some("pkg/handler/handler.go".to_string())
        );
    }

    #[test]
    fn test_trim_prefix() {
        let dir = setup_workspace();
        let config = PathMappingConfig {
            trim_prefix: "/home/ci/go/src/github.com/user/project/".to_string(),
            source_root: String::new(),
        };
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        assert_eq!(
            resolver.resolve("/home/ci/go/src/github.com/user/project/pkg/handler/handler.go"),
            Some("pkg/handler/handler.go".to_string())
        );
    }

    #[test]
    fn test_suffix_match() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        // Profile path has extra prefix, but ends with workspace-relative path.
        assert_eq!(
            resolver.resolve("github.com/user/project/pkg/handler/handler.go"),
            Some("pkg/handler/handler.go".to_string())
        );
    }

    #[test]
    fn test_no_match() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        assert_eq!(resolver.resolve("nonexistent.go"), None);
    }

    #[test]
    fn test_caching() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        // First resolve — populates cache.
        let result1 = resolver.resolve("main.go");
        // Second resolve — hits cache.
        let result2 = resolver.resolve("main.go");
        assert_eq!(result1, result2);
        assert_eq!(result1, Some("main.go".to_string()));
    }

    #[test]
    fn test_invalidate() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        // Populate cache.
        resolver.resolve("main.go");
        assert!(!resolver.cache.is_empty());

        // Invalidate.
        resolver.invalidate();
        assert!(resolver.cache.is_empty());
    }
}
