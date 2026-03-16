use crate::analysis::{self, ProfileData};
use crate::config::Config;
use crate::hints;
use crate::lenses;
use crate::profile;
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
                let data = analysis::analyze_profile(&raw_profile, max_hotspots);
                tracing::info!(
                    "loaded profile from {:?}: {} files, {} hotspots, total_value={}",
                    profile_path,
                    data.line_costs.len(),
                    data.hotspots.len(),
                    data.total_value,
                );

                let mut state = self.state.write().await;
                state.profile_data = Some(data);
                // Note: profile data keys are raw protobuf paths at this point.
                // Path resolution is added in Task 11.
            }
            Err(e) => {
                tracing::error!("failed to parse profile {:?}: {e}", profile_path);
            }
        }
    }

    /// Notify the client to refresh inlay hints and code lenses.
    pub async fn request_refresh(&self) {
        let state = self.state.read().await;

        if state.client_supports_inlay_refresh {
            if let Err(e) = self.client.inlay_hint_refresh().await {
                tracing::warn!("inlay hint refresh failed: {e}");
            }
        }

        if state.client_supports_codelens_refresh {
            if let Err(e) = self.client.code_lens_refresh().await {
                tracing::warn!("code lens refresh failed: {e}");
            }
        }
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
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

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

        // Start the profile file watcher as a background task.
        let state = self.state.clone();
        let client = self.client.clone();

        let watch_state = self.state.clone();
        tokio::spawn(async move {
            let interval = {
                let s = watch_state.read().await;
                Duration::from_secs(s.config.watch_interval_secs)
            };

            let mut watcher = crate::watch::FileWatcher::new();

            loop {
                tokio::time::sleep(interval).await;

                let files = {
                    let s = watch_state.read().await;
                    match (&s.workspace_root, &s.config) {
                        (Some(root), config) => discover_profile_files(root, config),
                        _ => continue,
                    }
                };

                if watcher.check_for_changes(&files) {
                    tracing::info!("profile file changes detected, reloading");

                    // Re-create a temporary Backend-like context to reload.
                    let backend = Backend {
                        client: client.clone(),
                        state: state.clone(),
                    };
                    backend.load_profiles().await;
                    backend.request_refresh().await;
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

        let config = Config::default();
        let files = discover_profile_files(root, &config);

        assert_eq!(files.len(), 1);
    }
}
