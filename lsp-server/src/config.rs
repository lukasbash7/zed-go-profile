use serde::Deserialize;

/// LSP initialization options passed from the Zed extension.
/// All fields are optional with sensible defaults.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Directories to search for profile files, relative to workspace root.
    pub profile_paths: Vec<String>,
    /// Glob pattern for profile files.
    pub profile_glob: String,
    /// Filtering thresholds.
    pub threshold: ThresholdConfig,
    /// Display options.
    pub display: DisplayConfig,
    /// Path mapping configuration.
    pub path_mapping: PathMappingConfig,
    /// Profile file poll interval in seconds.
    pub watch_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            profile_paths: vec![
                ".".to_string(),
                "./profiles".to_string(),
                "./pprof".to_string(),
            ],
            profile_glob: "*.{pprof,prof}".to_string(),
            threshold: ThresholdConfig::default(),
            display: DisplayConfig::default(),
            path_mapping: PathMappingConfig::default(),
            watch_interval_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThresholdConfig {
    /// Minimum percentage of total cost to display a hint (0.0 - 100.0).
    pub min_percent: f64,
    /// Minimum flat value to display a hint (unit-dependent, e.g. nanoseconds or bytes).
    /// If None, only percentage threshold applies.
    pub min_flat: Option<i64>,
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            min_percent: 0.1,
            min_flat: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DisplayConfig {
    /// Show flat cost in inlay hints.
    pub show_flat: bool,
    /// Show cumulative cost in inlay hints.
    pub show_cumulative: bool,
    /// Maximum number of code lenses per file.
    pub max_code_lenses: usize,
    /// Maximum number of hotspot functions tracked globally.
    pub max_hotspots: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            show_flat: true,
            show_cumulative: true,
            max_code_lenses: 10,
            max_hotspots: 50,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PathMappingConfig {
    /// Prefix to strip from profile file paths.
    pub trim_prefix: String,
    /// Source root to prepend after trimming.
    pub source_root: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.profile_paths, vec![".", "./profiles", "./pprof"]);
        assert_eq!(config.profile_glob, "*.{pprof,prof}");
        assert!((config.threshold.min_percent - 0.1).abs() < f64::EPSILON);
        assert!(config.threshold.min_flat.is_none());
        assert!(config.display.show_flat);
        assert!(config.display.show_cumulative);
        assert_eq!(config.display.max_code_lenses, 10);
        assert_eq!(config.display.max_hotspots, 50);
        assert_eq!(config.watch_interval_secs, 5);
    }

    #[test]
    fn test_deserialize_partial_json() {
        let json = r#"{ "profileGlob": "*.pprof", "watchIntervalSecs": 10 }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.profile_glob, "*.pprof");
        assert_eq!(config.watch_interval_secs, 10);
        // Defaults for unspecified fields:
        assert_eq!(config.profile_paths, vec![".", "./profiles", "./pprof"]);
        assert!(config.display.show_flat);
    }

    #[test]
    fn test_deserialize_full_json() {
        let json = r#"{
            "profilePaths": ["/tmp/profiles"],
            "profileGlob": "*.prof",
            "threshold": { "minPercent": 1.0, "minFlat": 1000000 },
            "display": {
                "showFlat": false,
                "showCumulative": true,
                "maxCodeLenses": 5,
                "maxHotspots": 20
            },
            "pathMapping": {
                "trimPrefix": "/home/ci/go/src/",
                "sourceRoot": ""
            },
            "watchIntervalSecs": 2
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.profile_paths, vec!["/tmp/profiles"]);
        assert!(!config.display.show_flat);
        assert_eq!(config.display.max_hotspots, 20);
        assert_eq!(config.threshold.min_flat, Some(1000000));
        assert_eq!(config.path_mapping.trim_prefix, "/home/ci/go/src/");
    }

    #[test]
    fn test_deserialize_empty_json() {
        let json = "{}";
        let config: Config = serde_json::from_str(json).unwrap();
        // All defaults should apply.
        assert_eq!(config.profile_paths.len(), 3);
        assert_eq!(config.watch_interval_secs, 5);
    }
}
