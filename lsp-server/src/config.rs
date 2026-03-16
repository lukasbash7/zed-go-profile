use serde::Deserialize;
use std::fmt;

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
    /// Diagnostics settings.
    pub diagnostics: DiagnosticsConfig,
    /// Profile file poll interval in seconds.
    pub watch_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            profile_paths: vec![".".to_string()],
            profile_glob: "*.{pprof,prof}".to_string(),
            threshold: ThresholdConfig::default(),
            display: DisplayConfig::default(),
            path_mapping: PathMappingConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            watch_interval_secs: 30,
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
    /// Style for severity indicators: "emoji" (colored circles) or "ascii".
    pub hint_style: HintStyle,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            show_flat: true,
            show_cumulative: true,
            max_code_lenses: 10,
            max_hotspots: 50,
            hint_style: HintStyle::Emoji,
        }
    }
}

/// Style for severity indicator prefixes on inlay hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HintStyle {
    /// Colored circle emoji: 🟢 🟡 🟠 🔴
    Emoji,
    /// ASCII block characters: ░ ▒ ▓ █
    Ascii,
}

impl fmt::Display for HintStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HintStyle::Emoji => write!(f, "emoji"),
            HintStyle::Ascii => write!(f, "ascii"),
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DiagnosticsConfig {
    /// Severity level for profile diagnostics: "warning", "info", or "off".
    pub severity: DiagnosticsSeverity,
    /// Minimum cumulative percentage to publish a diagnostic for a line.
    /// Lines below this threshold are omitted from the diagnostics panel.
    pub min_percent: f64,
}

/// Controls the severity level of published diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticsSeverity {
    /// Publish as warnings (visible in Diagnostics panel).
    Warning,
    /// Publish as info (visible on hover / inline only).
    Info,
    /// Do not publish diagnostics.
    Off,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            severity: DiagnosticsSeverity::Off,
            min_percent: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.profile_paths, vec!["."]);
        assert_eq!(config.profile_glob, "*.{pprof,prof}");
        assert!((config.threshold.min_percent - 0.1).abs() < f64::EPSILON);
        assert!(config.threshold.min_flat.is_none());
        assert!(config.display.show_flat);
        assert!(config.display.show_cumulative);
        assert_eq!(config.display.max_code_lenses, 10);
        assert_eq!(config.display.max_hotspots, 50);
        assert_eq!(config.display.hint_style, HintStyle::Emoji);
        assert_eq!(config.watch_interval_secs, 30);
    }

    #[test]
    fn test_deserialize_partial_json() {
        let json = r#"{ "profileGlob": "*.pprof", "watchIntervalSecs": 10 }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.profile_glob, "*.pprof");
        assert_eq!(config.watch_interval_secs, 10);
        assert_eq!(config.profile_paths, vec!["."]);
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
        assert_eq!(config.profile_paths.len(), 1);
        assert_eq!(config.watch_interval_secs, 30);
        assert_eq!(config.display.hint_style, HintStyle::Emoji);
    }

    #[test]
    fn test_deserialize_hint_style_ascii() {
        let json = r#"{ "display": { "hintStyle": "ascii" } }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.display.hint_style, HintStyle::Ascii);
    }

    #[test]
    fn test_deserialize_hint_style_emoji() {
        let json = r#"{ "display": { "hintStyle": "emoji" } }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.display.hint_style, HintStyle::Emoji);
    }
}
