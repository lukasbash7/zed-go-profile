use crate::analysis::{self, ProfileData};
use crate::config::Config;
use crate::diagnostics;
use crate::hints;
use crate::lenses;
use crate::paths::PathResolver;
use crate::profile;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

/// Shared server state protected by an async RwLock.
pub struct ServerState {
    pub config: Config,
    pub profile_data: Option<ProfileData>,
    pub workspace_root: Option<PathBuf>,
    pub client_supports_inlay_refresh: bool,
    pub client_supports_codelens_refresh: bool,
}

impl ServerState {
    fn new() -> Self {
        Self {
            config: Config::default(),
            profile_data: None,
            workspace_root: None,
            client_supports_inlay_refresh: false,
            client_supports_codelens_refresh: false,
        }
    }
}

/// The LSP server backend.
pub struct Backend {
    pub client: Client,
    pub state: Arc<RwLock<ServerState>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(ServerState::new())),
        }
    }

    /// Load profile files from configured paths and rebuild analysis data.
    pub async fn load_profiles(&self) {
        let state = self.state.read().await;
        let Some(ref workspace_root) = state.workspace_root else {
            return;
        };
        let workspace_root = workspace_root.clone();
        let config = state.config.clone();
        drop(state);

        let profile_files = discover_profile_files(&workspace_root, &config);

        if profile_files.is_empty() {
            tracing::debug!("no profile files found");
            let mut state = self.state.write().await;
            state.profile_data = None;
            return;
        }

        // Parse all profile files and merge results.
        // For v1, if multiple profiles exist, use the most recently modified one.
        let newest = profile_files
            .iter()
            .filter_map(|p| {
                p.metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| (p, t))
            })
            .max_by_key(|(_, t)| *t)
            .map(|(p, _)| p);

        let Some(profile_path) = newest else {
            return;
        };

        match profile::parse_profile_file(profile_path) {
            Ok(raw_profile) => {
                let max_hotspots = config.display.max_hotspots;
                let mut data = analysis::analyze_profile(&raw_profile, max_hotspots);
                tracing::info!(
                    "loaded profile from {:?}: {} files, {} hotspots, total_value={}",
                    profile_path,
                    data.line_costs.len(),
                    data.hotspots.len(),
                    data.total_value,
                );

                // Resolve profile paths to workspace-relative paths.
                let mut resolver = PathResolver::new(
                    workspace_root.clone(),
                    config.path_mapping.clone(),
                );

                // Resolve line_costs keys.
                let resolved_line_costs: HashMap<String, BTreeMap<u64, analysis::LineCost>> = data
                    .line_costs
                    .drain()
                    .filter_map(|(profile_path, costs)| {
                        resolver
                            .resolve(&profile_path)
                            .map(|resolved| (resolved, costs))
                    })
                    .collect();
                data.line_costs = resolved_line_costs;

                // Resolve hotspot filenames.
                for hotspot in &mut data.hotspots {
                    if let Some(resolved) = resolver.resolve(&hotspot.filename) {
                        hotspot.filename = resolved;
                    }
                    // If unresolvable, leave original — the lens just won't match any open file.
                }

                let mut state = self.state.write().await;
                state.profile_data = Some(data);
            }
            Err(e) => {
                tracing::error!("failed to parse profile {:?}: {e}", profile_path);
            }
        }
    }

    /// Notify the client to refresh inlay hints and code lenses.
    pub async fn request_refresh(&self) {
        let (inlay, codelens) = {
            let state = self.state.read().await;
            (state.client_supports_inlay_refresh, state.client_supports_codelens_refresh)
        };

        if inlay {
            if let Err(e) = self.client.inlay_hint_refresh().await {
                tracing::warn!("inlay hint refresh failed: {e}");
            }
        }

        if codelens {
            if let Err(e) = self.client.code_lens_refresh().await {
                tracing::warn!("code lens refresh failed: {e}");
            }
        }
    }

    /// Publish diagnostics for all files that have lines above the threshold.
    /// Sends `textDocument/publishDiagnostics` for each qualifying file, and
    /// clears diagnostics for files that previously had them but no longer do.
    pub async fn publish_diagnostics(&self) {
        let state = self.state.read().await;
        if state.config.diagnostics.severity == crate::config::DiagnosticsSeverity::Off {
            return;
        }
        let Some(ref data) = state.profile_data else {
            return;
        };
        let Some(ref workspace_root) = state.workspace_root else {
            return;
        };

        let config = state.config.clone();
        let workspace_root = workspace_root.clone();
        let file_keys = diagnostics::files_with_diagnostics(data, &config);

        let mut notifications = Vec::new();
        for file_key in &file_keys {
            let diags = diagnostics::generate_diagnostics(data, &config, file_key);
            let path = workspace_root.join(file_key);
            if let Ok(uri) = Url::from_file_path(&path) {
                notifications.push((uri, diags));
            }
        }
        drop(state);

        for (uri, diags) in notifications {
            self.client
                .publish_diagnostics(uri, diags, None)
                .await;
        }

        tracing::info!(
            "published diagnostics for {} files",
            file_keys.len()
        );
    }

    /// Resolve a document URI to a workspace-relative file key.
    /// Returns None if the URI can't be resolved or doesn't match workspace.
    fn uri_to_file_key(uri: &Url, workspace_root: &Path) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        let relative = path.strip_prefix(workspace_root).ok()?;
        relative.to_str().map(|s| s.to_string())
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Extract workspace root.
        let workspace_root = params
            .root_uri
            .as_ref()
            .and_then(|uri| uri.to_file_path().ok())
            .or_else(|| {
                #[allow(deprecated)]
                params.root_path.as_ref().map(PathBuf::from)
            });

        // Parse initialization options.
        let config: Config = params
            .initialization_options
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        tracing::info!(
            "config: profile_paths={:?}, profile_glob={:?}, workspace_root={:?}",
            config.profile_paths,
            config.profile_glob,
            workspace_root
        );

        // Check client capabilities for refresh support.
        let client_supports_inlay_refresh = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.inlay_hint.as_ref())
            .and_then(|ih| ih.refresh_support)
            .unwrap_or(false);

        let client_supports_codelens_refresh = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.code_lens.as_ref())
            .and_then(|cl| cl.refresh_support)
            .unwrap_or(false);

        if !client_supports_inlay_refresh {
            tracing::warn!("client does not support workspace/inlayHint/refresh");
        }
        if !client_supports_codelens_refresh {
            tracing::warn!("client does not support workspace/codeLens/refresh");
        }

        // Update state.
        {
            let mut state = self.state.write().await;
            state.config = config;
            state.workspace_root = workspace_root;
            state.client_supports_inlay_refresh = client_supports_inlay_refresh;
            state.client_supports_codelens_refresh = client_supports_codelens_refresh;
        }

        // Load profiles in background.
        self.load_profiles().await;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                    InlayHintOptions {
                        resolve_provider: Some(false),
                        work_done_progress_options: WorkDoneProgressOptions {
                            work_done_progress: None,
                        },
                    },
                ))),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::NONE),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn initialized(&self, _params: InitializedParams) {
        tracing::info!("go-profile-lsp initialized");

        // Profiles were loaded during initialize(). Now that the handshake is
        // complete, ask the client to re-request inlay hints / code lenses so
        // that any early (empty) responses are replaced with real data.
        self.request_refresh().await;

        // Push diagnostics for all qualifying files now that profile is loaded.
        self.publish_diagnostics().await;

        // Start the profile file watcher as a background task.
        let state = self.state.clone();
        let client = self.client.clone();

        let watch_state = self.state.clone();
        tokio::spawn(async move {
            let (interval, initial_files) = {
                let s = watch_state.read().await;
                let interval = Duration::from_secs(s.config.watch_interval_secs);
                let files = match &s.workspace_root {
                    Some(root) => discover_profile_files(root, &s.config),
                    None => Vec::new(),
                };
                (interval, files)
            };

            let mut watcher = crate::watch::FileWatcher::new();
            // Seed with files discovered at startup to avoid spurious first reload.
            watcher.seed(&initial_files);

            loop {
                tokio::time::sleep(interval).await;

                let changed = if watcher.should_rediscover() {
                    // Expensive: full glob re-discovery to find new/removed files.
                    let files = {
                        let s = watch_state.read().await;
                        match (&s.workspace_root, &s.config) {
                            (Some(root), config) => discover_profile_files(root, config),
                            _ => continue,
                        }
                    };
                    watcher.check_for_changes(&files)
                } else {
                    // Cheap: only stat the already-known files.
                    watcher.check_known_files()
                };

                if changed {
                    tracing::info!("profile file changes detected, reloading");

                    // Re-create a temporary Backend-like context to reload.
                    let backend = Backend {
                        client: client.clone(),
                        state: state.clone(),
                    };
                    backend.load_profiles().await;
                    backend.request_refresh().await;
                    backend.publish_diagnostics().await;
                }
            }
        });
    }

    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
        let state = self.state.read().await;

        let Some(ref data) = state.profile_data else {
            return Ok(None);
        };

        let Some(ref workspace_root) = state.workspace_root else {
            return Ok(None);
        };

        let Some(file_key) = Self::uri_to_file_key(&params.text_document.uri, workspace_root) else {
            return Ok(None);
        };

        // After Task 11, profile data keys are workspace-relative paths,
        // so direct lookup works.
        if !data.line_costs.contains_key(&file_key) {
            return Ok(None);
        }

        let hints = hints::generate_inlay_hints(data, &state.config, &file_key, &params.range);

        if hints.is_empty() {
            Ok(None)
        } else {
            Ok(Some(hints))
        }
    }

    async fn code_lens(
        &self,
        params: CodeLensParams,
    ) -> Result<Option<Vec<CodeLens>>> {
        let state = self.state.read().await;

        let Some(ref data) = state.profile_data else {
            return Ok(None);
        };

        let Some(ref workspace_root) = state.workspace_root else {
            return Ok(None);
        };

        let Some(file_key) = Self::uri_to_file_key(&params.text_document.uri, workspace_root) else {
            return Ok(None);
        };

        let code_lenses = lenses::generate_code_lenses(data, &state.config, &file_key);

        if code_lenses.is_empty() {
            Ok(None)
        } else {
            Ok(Some(code_lenses))
        }
    }
}

/// Expand simple brace patterns like `*.{pprof,prof}` into multiple patterns.
/// The `glob` crate doesn't support `{a,b}` syntax, so we expand it manually.
/// Only handles a single brace group. Returns the original pattern if no braces found.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close) = pattern[open..].find('}').map(|i| open + i) else {
        return vec![pattern.to_string()];
    };

    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let alternatives = &pattern[open + 1..close];

    alternatives
        .split(',')
        .map(|alt| format!("{prefix}{alt}{suffix}"))
        .collect()
}

/// Discover profile files in the workspace based on configuration.
fn discover_profile_files(workspace_root: &Path, config: &Config) -> Vec<PathBuf> {
    let mut files = Vec::new();

    for search_path in &config.profile_paths {
        let dir = workspace_root.join(search_path);
        if !dir.is_dir() {
            continue;
        }

        let base_pattern = dir.join(&config.profile_glob);
        let base_pattern_str = base_pattern.to_string_lossy();
        let patterns = expand_braces(&base_pattern_str);

        for pattern_str in &patterns {
            if let Ok(paths) = glob::glob(pattern_str) {
                for entry in paths.flatten() {
                    if entry.is_file() {
                        files.push(entry);
                    }
                }
            }
        }
    }

    // Deduplicate (same file could be found via multiple search paths).
    files.sort();
    files.dedup();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uri_to_file_key() {
        let workspace_root = PathBuf::from("/workspace/project");
        let uri = Url::from_file_path("/workspace/project/pkg/handler.go").unwrap();

        let key = Backend::uri_to_file_key(&uri, &workspace_root);
        assert_eq!(key, Some("pkg/handler.go".to_string()));
    }

    #[test]
    fn test_uri_to_file_key_outside_workspace() {
        let workspace_root = PathBuf::from("/workspace/project");
        let uri = Url::from_file_path("/other/path/file.go").unwrap();

        let key = Backend::uri_to_file_key(&uri, &workspace_root);
        assert_eq!(key, None);
    }

    #[test]
    fn test_discover_profile_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create profile files.
        std::fs::write(root.join("cpu.pprof"), b"data").unwrap();
        std::fs::write(root.join("heap.prof"), b"data").unwrap();
        std::fs::write(root.join("other.txt"), b"data").unwrap();

        let config = Config::default();
        let files = discover_profile_files(root, &config);

        assert_eq!(files.len(), 2);
        let names: Vec<&str> = files.iter().filter_map(|f| f.file_name()?.to_str()).collect();
        assert!(names.contains(&"cpu.pprof"));
        assert!(names.contains(&"heap.prof"));
    }

    #[test]
    fn test_discover_profile_files_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a "profiles" subdirectory with a profile.
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::write(root.join("profiles/cpu.pprof"), b"data").unwrap();

        // Default config only searches ".", so subdirectory profiles are not found.
        let config = Config::default();
        let files = discover_profile_files(root, &config);
        assert_eq!(files.len(), 0);

        // User adds "profiles" to profilePaths to find them.
        let config = Config {
            profile_paths: vec![".".to_string(), "./profiles".to_string()],
            ..Config::default()
        };
        let files = discover_profile_files(root, &config);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_discover_profile_files_deep_nesting_requires_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a deeply nested profile file.
        std::fs::create_dir_all(root.join("cmd/server")).unwrap();
        std::fs::write(root.join("cmd/server/cpu.pprof"), b"data").unwrap();
        // Also one at root level.
        std::fs::write(root.join("heap.pprof"), b"data").unwrap();

        // Default config (non-recursive) should only find root-level file.
        let config = Config::default();
        let files = discover_profile_files(root, &config);
        assert_eq!(files.len(), 1);

        // With explicit profilePaths, deep files are found.
        let config = Config {
            profile_paths: vec![".".to_string(), "./cmd/server".to_string()],
            ..Config::default()
        };
        let files = discover_profile_files(root, &config);
        assert_eq!(files.len(), 2);
        let names: Vec<&str> = files.iter().filter_map(|f| f.file_name()?.to_str()).collect();
        assert!(names.contains(&"cpu.pprof"));
        assert!(names.contains(&"heap.pprof"));
    }

    #[test]
    fn test_discover_profile_files_recursive_glob_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a deeply nested profile file.
        std::fs::create_dir_all(root.join("cmd/server")).unwrap();
        std::fs::write(root.join("cmd/server/cpu.pprof"), b"data").unwrap();
        std::fs::write(root.join("heap.pprof"), b"data").unwrap();

        // User can opt in to recursive glob.
        let config = Config {
            profile_glob: "**/*.{pprof,prof}".to_string(),
            ..Config::default()
        };
        let files = discover_profile_files(root, &config);
        assert_eq!(files.len(), 2);
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::profile::proto;
    use prost::Message;

    /// Build a realistic test profile with multiple files and functions.
    /// Uses absolute paths (like real pprof profiles do) to exercise path resolution.
    fn make_realistic_profile() -> Vec<u8> {
        let string_table = vec![
            "".to_string(),            // 0
            "cpu".to_string(),         // 1
            "nanoseconds".to_string(), // 2
            "/home/ci/go/src/myproject/main.go".to_string(),     // 3
            "main".to_string(),        // 4
            "/home/ci/go/src/myproject/pkg/handler/handler.go".to_string(), // 5
            "HandleRequest".to_string(), // 6
            "/home/ci/go/src/myproject/pkg/db/query.go".to_string(), // 7
            "ExecuteQuery".to_string(),  // 8
        ];

        let functions = vec![
            proto::Function { id: 1, name: 4, system_name: 4, filename: 3, start_line: 10 },
            proto::Function { id: 2, name: 6, system_name: 6, filename: 5, start_line: 25 },
            proto::Function { id: 3, name: 8, system_name: 8, filename: 7, start_line: 15 },
        ];

        let locations = vec![
            proto::Location { id: 1, line: vec![proto::Line { function_id: 1, line: 15 }], ..Default::default() },
            proto::Location { id: 2, line: vec![proto::Line { function_id: 2, line: 30 }], ..Default::default() },
            proto::Location { id: 3, line: vec![proto::Line { function_id: 2, line: 35 }], ..Default::default() },
            proto::Location { id: 4, line: vec![proto::Line { function_id: 3, line: 20 }], ..Default::default() },
        ];

        let samples = vec![
            // Hot path: query -> handler -> main
            proto::Sample {
                location_id: vec![4, 2, 1],
                value: vec![500_000_000], // 500ms
                label: vec![],
            },
            // Handler doing its own work
            proto::Sample {
                location_id: vec![3, 1],
                value: vec![200_000_000], // 200ms
                label: vec![],
            },
            // Direct main work
            proto::Sample {
                location_id: vec![1],
                value: vec![100_000_000], // 100ms
                label: vec![],
            },
        ];

        let profile = proto::Profile {
            sample_type: vec![proto::ValueType { r#type: 1, unit: 2 }],
            sample: samples,
            location: locations,
            function: functions,
            string_table,
            duration_nanos: 10_000_000_000,
            ..Default::default()
        };

        profile.encode_to_vec()
    }

    #[test]
    fn test_full_pipeline() {
        // 1. Create workspace with Go files.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pkg/handler")).unwrap();
        std::fs::create_dir_all(root.join("pkg/db")).unwrap();
        std::fs::write(root.join("main.go"), "package main\n\nfunc main() {\n}\n").unwrap();
        std::fs::write(root.join("pkg/handler/handler.go"), "package handler\n").unwrap();
        std::fs::write(root.join("pkg/db/query.go"), "package db\n").unwrap();

        // 2. Write profile file.
        let profile_bytes = make_realistic_profile();
        std::fs::write(root.join("cpu.pprof"), &profile_bytes).unwrap();

        // 3. Parse.
        let raw = crate::profile::parse_profile(&profile_bytes).unwrap();

        // 4. Analyze.
        let mut data = crate::analysis::analyze_profile(&raw, 50);

        // Verify totals.
        assert_eq!(data.total_value, 800_000_000); // 500 + 200 + 100

        // 5. Resolve paths using trim_prefix (simulating CI build paths).
        let path_config = crate::config::PathMappingConfig {
            trim_prefix: "/home/ci/go/src/myproject/".to_string(),
            ..Default::default()
        };
        let mut resolver = crate::paths::PathResolver::new(root.to_path_buf(), path_config);

        // Resolve line_costs keys.
        let resolved: std::collections::HashMap<String, std::collections::BTreeMap<u64, crate::analysis::LineCost>> = data
            .line_costs
            .drain()
            .filter_map(|(path, costs)| {
                resolver.resolve(&path).map(|r| (r, costs))
            })
            .collect();
        data.line_costs = resolved;

        // Resolve hotspot filenames.
        for h in &mut data.hotspots {
            if let Some(r) = resolver.resolve(&h.filename) {
                h.filename = r;
            }
        }

        // 6. Verify resolved data.
        assert!(data.line_costs.contains_key("main.go"), "keys: {:?}", data.line_costs.keys().collect::<Vec<_>>());
        assert!(data.line_costs.contains_key("pkg/handler/handler.go"));
        assert!(data.line_costs.contains_key("pkg/db/query.go"));

        // 7. Generate hints.
        let cfg = crate::config::Config::default();
        let range = tower_lsp::lsp_types::Range {
            start: tower_lsp::lsp_types::Position { line: 0, character: 0 },
            end: tower_lsp::lsp_types::Position { line: 100, character: 0 },
        };

        let hints = crate::hints::generate_inlay_hints(&data, &cfg, "main.go", &range);
        assert!(!hints.is_empty(), "expected hints for main.go");

        let handler_hints = crate::hints::generate_inlay_hints(&data, &cfg, "pkg/handler/handler.go", &range);
        assert!(!handler_hints.is_empty(), "expected hints for handler.go");

        // 8. Generate code lenses.
        let lenses = crate::lenses::generate_code_lenses(&data, &cfg, "pkg/handler/handler.go");
        // HandleRequest should be a hotspot.
        assert!(!lenses.is_empty(), "expected code lenses for handler.go");

        let lens_title = &lenses[0].command.as_ref().unwrap().title;
        assert!(lens_title.contains("hotspot"), "lens title: {lens_title}");
    }
}
