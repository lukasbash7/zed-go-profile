use zed_extension_api as zed;

struct GoProfileExtension {
    cached_binary_path: Option<String>,
}

impl GoProfileExtension {
    fn ensure_binary(&mut self, _worktree: &zed::Worktree) -> zed::Result<String> {
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
            "user/zed-go-profile",
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
        let settings = zed::settings::LspSettings::for_worktree("go-profile-lsp", worktree)?;
        Ok(settings.initialization_options)
    }
}

zed::register_extension!(GoProfileExtension);
