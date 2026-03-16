use crate::analysis::ProfileData;
use crate::config::{Config, DiagnosticsSeverity};
use crate::format::{format_percent, format_value};
use tower_lsp::lsp_types::*;

/// Generate diagnostics for a single file's profiled lines.
///
/// Only lines whose cumulative cost exceeds `min_diagnostic_percent` of the
/// total profile value are included. Returns an empty vec when the file has
/// no qualifying lines.
pub fn generate_diagnostics(
    data: &ProfileData,
    config: &Config,
    file_key: &str,
) -> Vec<Diagnostic> {
    let Some(line_costs) = data.line_costs.get(file_key) else {
        return Vec::new();
    };

    let threshold = config.diagnostics.min_percent;
    let total = data.total_value;

    line_costs
        .iter()
        .filter(|(_, cost)| {
            if total == 0 {
                return false;
            }
            let pct = (cost.cumulative as f64 / total as f64) * 100.0;
            pct >= threshold
        })
        .filter_map(|(&line_no, cost)| {
            let line = line_no.saturating_sub(1) as u32; // Convert to 0-indexed

            let mut parts = Vec::new();
            if config.display.show_flat && cost.flat != 0 {
                let flat_str = format_value(cost.flat, data.value_unit);
                let flat_pct = format_percent(cost.flat, data.total_value);
                parts.push(format!("flat: {flat_str} {flat_pct}"));
            }
            if config.display.show_cumulative {
                let cum_str = format_value(cost.cumulative, data.value_unit);
                let cum_pct = format_percent(cost.cumulative, data.total_value);
                parts.push(format!("cum: {cum_str} {cum_pct}"));
            }

            let message = format!("[profile] {}", parts.join(" | "));

            let severity = match config.diagnostics.severity {
                DiagnosticsSeverity::Warning => DiagnosticSeverity::WARNING,
                DiagnosticsSeverity::Info => DiagnosticSeverity::INFORMATION,
                DiagnosticsSeverity::Off => return None, // shouldn't reach here
            };

            Some(Diagnostic {
                range: Range {
                    start: Position { line, character: 0 },
                    end: Position { line, character: 0 },
                },
                severity: Some(severity),
                source: Some("go-profile".to_string()),
                message,
                ..Default::default()
            })
        })
        .collect()
}

/// Return all file keys from the profile that have at least one line above the
/// diagnostic threshold. Used to know which files to publish diagnostics for.
pub fn files_with_diagnostics(data: &ProfileData, config: &Config) -> Vec<String> {
    let threshold = config.diagnostics.min_percent;
    let total = data.total_value;

    data.line_costs
        .iter()
        .filter(|(_, costs)| {
            costs.values().any(|cost| {
                if total == 0 {
                    return false;
                }
                let pct = (cost.cumulative as f64 / total as f64) * 100.0;
                pct >= threshold
            })
        })
        .map(|(file_key, _)| file_key.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{LineCost, ProfileData, ProfileType};
    use crate::config::{DiagnosticsConfig, DiagnosticsSeverity, DisplayConfig};
    use crate::format::ValueUnit;
    use std::collections::{BTreeMap, HashMap};

    fn make_test_data() -> ProfileData {
        let mut file_costs = BTreeMap::new();
        file_costs.insert(
            10,
            LineCost {
                flat: 50_000_000,
                cumulative: 100_000_000, // 10%
            },
        );
        file_costs.insert(
            20,
            LineCost {
                flat: 0,
                cumulative: 5_000_000, // 0.5%
            },
        );
        file_costs.insert(
            30,
            LineCost {
                flat: 200_000_000,
                cumulative: 500_000_000, // 50%
            },
        );

        let mut line_costs = HashMap::new();
        line_costs.insert("handler.go".to_string(), file_costs);

        // Add a file with only low-cost lines
        let mut low_costs = BTreeMap::new();
        low_costs.insert(
            5,
            LineCost {
                flat: 0,
                cumulative: 1_000_000, // 0.1%
            },
        );
        line_costs.insert("utils.go".to_string(), low_costs);

        ProfileData {
            line_costs,
            hotspots: vec![],
            profile_type: ProfileType::Cpu,
            sample_type_label: "cpu".to_string(),
            value_unit: ValueUnit::Nanoseconds,
            total_value: 1_000_000_000,
            duration: None,
        }
    }

    fn make_test_config() -> Config {
        Config {
            diagnostics: DiagnosticsConfig {
                severity: DiagnosticsSeverity::Warning,
                min_percent: 1.0,
            },
            display: DisplayConfig {
                show_flat: true,
                show_cumulative: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_generate_diagnostics_filters_by_threshold() {
        let data = make_test_data();
        let config = make_test_config();

        let diags = generate_diagnostics(&data, &config, "handler.go");
        // Line 10 (10%) and line 30 (50%) should pass 1% threshold.
        // Line 20 (0.5%) should be excluded.
        assert_eq!(diags.len(), 2);

        assert_eq!(diags[0].range.start.line, 9); // line 10, 0-indexed
        assert_eq!(diags[1].range.start.line, 29); // line 30, 0-indexed
    }

    #[test]
    fn test_diagnostics_are_hint_severity() {
        let data = make_test_data();
        let config = make_test_config();

        let diags = generate_diagnostics(&data, &config, "handler.go");
        for d in &diags {
            assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
            assert_eq!(d.source.as_deref(), Some("go-profile"));
        }
    }

    #[test]
    fn test_diagnostics_message_format() {
        let data = make_test_data();
        let config = make_test_config();

        let diags = generate_diagnostics(&data, &config, "handler.go");
        let big = &diags[1]; // line 30: flat=200ms, cum=500ms
        assert!(big.message.contains("[profile]"), "msg: {}", big.message);
        assert!(big.message.contains("cum: 500ms"), "msg: {}", big.message);
        assert!(big.message.contains("flat: 200ms"), "msg: {}", big.message);
    }

    #[test]
    fn test_diagnostics_hides_zero_flat() {
        let data = make_test_data();
        let mut config = make_test_config();
        config.diagnostics.min_percent = 5.0; // Only line 10 (10%) and 30 (50%)

        let diags = generate_diagnostics(&data, &config, "handler.go");
        let line10 = &diags[0]; // flat=50ms
        assert!(line10.message.contains("flat:"), "msg: {}", line10.message);
    }

    #[test]
    fn test_diagnostics_no_file() {
        let data = make_test_data();
        let config = make_test_config();

        let diags = generate_diagnostics(&data, &config, "nonexistent.go");
        assert!(diags.is_empty());
    }

    #[test]
    fn test_diagnostics_empty_when_below_threshold() {
        let data = make_test_data();
        let mut config = make_test_config();
        config.diagnostics.min_percent = 99.0; // Nothing passes

        let diags = generate_diagnostics(&data, &config, "handler.go");
        assert!(diags.is_empty());
    }

    #[test]
    fn test_files_with_diagnostics() {
        let data = make_test_data();
        let config = make_test_config(); // 1% threshold

        let files = files_with_diagnostics(&data, &config);
        // handler.go has lines at 10% and 50%, utils.go only at 0.1%
        assert!(files.contains(&"handler.go".to_string()));
        assert!(!files.contains(&"utils.go".to_string()));
    }

    #[test]
    fn test_files_with_diagnostics_lower_threshold() {
        let data = make_test_data();
        let mut config = make_test_config();
        config.diagnostics.min_percent = 0.05; // Very low

        let files = files_with_diagnostics(&data, &config);
        assert!(files.contains(&"handler.go".to_string()));
        assert!(files.contains(&"utils.go".to_string()));
    }

    #[test]
    fn test_diagnostics_zero_total() {
        let mut data = make_test_data();
        data.total_value = 0;
        let config = make_test_config();

        let diags = generate_diagnostics(&data, &config, "handler.go");
        assert!(diags.is_empty());
    }
}
