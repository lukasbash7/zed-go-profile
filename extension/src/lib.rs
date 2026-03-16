use zed_extension_api as zed;

struct GoProfileExtension {
    cached_binary_path: Option<String>,
}

impl GoProfileExtension {
    fn local_binary_path(worktree: &zed::Worktree) -> Option<String> {
        let settings = zed::settings::LspSettings::for_worktree("go-profile-lsp", worktree).ok()?;
        let binary = settings
            .settings
            .as_ref()?
            .get("binary")?
            .get("path")?
            .as_str()?
            .to_string();
        Some(binary)
    }

    fn ensure_binary(&mut self, worktree: &zed::Worktree) -> zed::Result<String> {
        // Check for user-configured local binary path first (for development).
        // Configure via Zed settings:
        //   "lsp": { "go-profile-lsp": { "settings": { "binary": { "path": "/absolute/path/to/go-profile-lsp" } } } }
        if let Some(local_path) = Self::local_binary_path(worktree) {
            return Ok(local_path);
        }

        if let Some(ref path) = self.cached_binary_path {
            if std::fs::metadata(path).is_ok() {
                return Ok(path.clone());
            }
        }

        let (os, arch) = zed::current_platform();

        let os_str = match os {
            zed::Os::Mac => "apple-darwin",
            zed::Os::Linux => "unknown-linux-gnu",
            zed::Os::Windows => "pc-windows-msvc",
        };

        let arch_str = match arch {
            zed::Architecture::Aarch64 => "aarch64",
            zed::Architecture::X8664 => "x86_64",
            _ => return Err("unsupported architecture".into()),
        };

        let ext = match os {
            zed::Os::Windows => ".exe",
            _ => "",
        };

        let asset_name = format!("go-profile-lsp-{arch_str}-{os_str}{ext}");

        let release = zed::latest_github_release(
            "lukasjorg/zed-go-profile",
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| format!("no asset found matching {asset_name}"))?;

        let binary_path = format!("go-profile-lsp{ext}");

        zed::download_file(
            &asset.download_url,
            &binary_path,
            zed::DownloadedFileType::Uncompressed,
        )?;

        zed::make_file_executable(&binary_path)?;

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

impl zed::Extension for GoProfileExtension {
    fn new() -> Self {
        GoProfileExtension {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let binary_path = self.ensure_binary(worktree)?;
        Ok(zed::Command {
            command: binary_path,
            args: vec!["--stdio".to_string()],
            env: Default::default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<zed::serde_json::Value>> {
        let lsp_settings = zed::settings::LspSettings::for_worktree("go-profile-lsp", worktree)?;

        // Prefer explicit initialization_options if set. Otherwise, forward
        // the "settings" object (minus the "binary" key which is extension-only)
        // so users can put all LSP config under "settings" naturally.
        if lsp_settings.initialization_options.is_some() {
            return Ok(lsp_settings.initialization_options);
        }

        if let Some(mut settings) = lsp_settings.settings {
            // Remove extension-only keys before forwarding to the LSP server.
            if let Some(obj) = settings.as_object_mut() {
                obj.remove("binary");
            }
            Ok(Some(settings))
        } else {
            Ok(None)
        }
    }
}

zed::register_extension!(GoProfileExtension);
