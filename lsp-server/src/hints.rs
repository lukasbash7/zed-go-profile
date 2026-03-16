use crate::analysis::ProfileData;
use crate::config::Config;
use crate::format::{format_percent, format_value};
use tower_lsp::lsp_types::*;

/// Generate inlay hints for a specific file within the given line range.
///
/// `file_key` is the workspace-relative path as stored in `ProfileData.line_costs`.
/// `range` is the visible range requested by the editor (0-indexed lines).
pub fn generate_inlay_hints(
    data: &ProfileData,
    config: &Config,
    file_key: &str,
    range: &Range,
) -> Vec<InlayHint> {
    let Some(file_costs) = data.line_costs.get(file_key) else {
        return Vec::new();
    };

    let start_line = range.start.line as u64 + 1; // LSP lines are 0-indexed, costs are 1-indexed
    let end_line = range.end.line as u64 + 1;

    let mut hints = Vec::new();

    for (&line_no, cost) in file_costs.range(start_line..=end_line) {
        if !passes_threshold(cost.flat, cost.cumulative, data.total_value, config) {
            continue;
        }

        let label = format_hint_label(
            cost.flat,
            cost.cumulative,
            data.total_value,
            data.value_unit,
            config.display.show_flat,
            config.display.show_cumulative,
        );

        if label.is_empty() {
            continue;
        }

        let tooltip = format_hint_tooltip(
            cost.flat,
            cost.cumulative,
            data.total_value,
            data.value_unit,
            &data.sample_type_label,
        );

        hints.push(InlayHint {
            position: Position {
                line: (line_no - 1) as u32, // Convert back to 0-indexed
                character: u32::MAX,        // End of line
            },
            label: InlayHintLabel::String(label),
            kind: None,
            text_edits: None,
            tooltip: Some(InlayHintTooltip::String(tooltip)),
            padding_left: Some(true),
            padding_right: None,
            data: None,
        });
    }

    hints
}

/// Check if a line cost passes the configured threshold.
fn passes_threshold(flat: i64, cumulative: i64, total: i64, config: &Config) -> bool {
    if total == 0 {
        return false;
    }

    // Check min_percent threshold on cumulative.
    let cum_pct = (cumulative as f64 / total as f64) * 100.0;
    if cum_pct >= config.threshold.min_percent {
        return true;
    }

    // Check min_flat threshold.
    if let Some(min_flat) = config.threshold.min_flat {
        if flat >= min_flat {
            return true;
        }
    }

    false
}

/// Format the inlay hint label text.
/// Example: "  flat: 15ms | cum: 340ms (55.1%)"
fn format_hint_label(
    flat: i64,
    cumulative: i64,
    total: i64,
    unit: crate::format::ValueUnit,
    show_flat: bool,
    show_cumulative: bool,
) -> String {
    let mut parts = Vec::new();

    if show_flat {
        parts.push(format!("flat: {}", format_value(flat, unit)));
    }

    if show_cumulative {
        let pct = format_percent(cumulative, total);
        let formatted = format_value(cumulative, unit);
        if pct.is_empty() {
            parts.push(format!("cum: {formatted}"));
        } else {
            parts.push(format!("cum: {formatted} {pct}"));
        }
    }

    if parts.is_empty() {
        return String::new();
    }

    format!("  {}", parts.join(" | "))
}

/// Format a detailed tooltip string.
fn format_hint_tooltip(
    flat: i64,
    cumulative: i64,
    total: i64,
    unit: crate::format::ValueUnit,
    sample_type_label: &str,
) -> String {
    let flat_pct = format_percent(flat, total);
    let cum_pct = format_percent(cumulative, total);

    format!(
        "Profile type: {}\nFlat: {} {}\nCumulative: {} {}",
        sample_type_label,
        format_value(flat, unit),
        flat_pct,
        format_value(cumulative, unit),
        cum_pct,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{LineCost, ProfileType};
    use crate::config::{DisplayConfig, ThresholdConfig};
    use crate::format::ValueUnit;
    use std::collections::{BTreeMap, HashMap};

    fn make_test_data() -> ProfileData {
        let mut file_costs = BTreeMap::new();
        file_costs.insert(
            10,
            LineCost {
                flat: 50_000_000,
                cumulative: 340_000_000,
            },
        );
        file_costs.insert(
            15,
            LineCost {
                flat: 15_000_000,
                cumulative: 200_000_000,
            },
        );
        file_costs.insert(
            20,
            LineCost {
                flat: 100_000,
                cumulative: 500_000,
            },
        ); // Below threshold

        let mut line_costs = HashMap::new();
        line_costs.insert("main.go".to_string(), file_costs);

        ProfileData {
            line_costs,
            hotspots: vec![],
            profile_type: ProfileType::Cpu,
            sample_type_label: "cpu".to_string(),
            value_unit: ValueUnit::Nanoseconds,
            total_value: 1_000_000_000, // 1 second
            duration: None,
        }
    }

    fn make_test_config() -> Config {
        Config {
            threshold: ThresholdConfig {
                min_percent: 0.1,
                min_flat: None,
            },
            display: DisplayConfig {
                show_flat: true,
                show_cumulative: true,
                max_code_lenses: 10,
                max_hotspots: 50,
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_generate_hints_basic() {
        let data = make_test_data();
        let config = make_test_config();
        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 30,
                character: 0,
            },
        };

        let hints = generate_inlay_hints(&data, &config, "main.go", &range);
        // Line 10 and 15 should pass threshold. Line 20 is 0.05% — below 0.1%.
        assert_eq!(hints.len(), 2);

        // Line 10 (0-indexed: 9)
        assert_eq!(hints[0].position.line, 9);
        assert_eq!(hints[0].position.character, u32::MAX);

        // Verify label contains expected values
        if let InlayHintLabel::String(ref label) = hints[0].label {
            assert!(label.contains("flat: 50ms"), "label was: {label}");
            assert!(label.contains("cum: 340ms"), "label was: {label}");
            assert!(label.contains("(34.0%)"), "label was: {label}");
        } else {
            panic!("expected string label");
        }
    }

    #[test]
    fn test_generate_hints_range_filtering() {
        let data = make_test_data();
        let config = make_test_config();
        // Only request lines 0-12 (0-indexed), which covers line_no 1-13 (1-indexed).
        // Only line 10 should be included.
        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 12,
                character: 0,
            },
        };

        let hints = generate_inlay_hints(&data, &config, "main.go", &range);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].position.line, 9); // line_no 10 -> 0-indexed 9
    }

    #[test]
    fn test_generate_hints_no_file() {
        let data = make_test_data();
        let config = make_test_config();
        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 100,
                character: 0,
            },
        };

        let hints = generate_inlay_hints(&data, &config, "nonexistent.go", &range);
        assert!(hints.is_empty());
    }

    #[test]
    fn test_generate_hints_flat_only() {
        let data = make_test_data();
        let mut config = make_test_config();
        config.display.show_flat = true;
        config.display.show_cumulative = false;

        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 30,
                character: 0,
            },
        };

        let hints = generate_inlay_hints(&data, &config, "main.go", &range);
        if let InlayHintLabel::String(ref label) = hints[0].label {
            assert!(label.contains("flat:"), "label was: {label}");
            assert!(!label.contains("cum:"), "label was: {label}");
        }
    }

    #[test]
    fn test_generate_hints_cumulative_only() {
        let data = make_test_data();
        let mut config = make_test_config();
        config.display.show_flat = false;
        config.display.show_cumulative = true;

        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 30,
                character: 0,
            },
        };

        let hints = generate_inlay_hints(&data, &config, "main.go", &range);
        if let InlayHintLabel::String(ref label) = hints[0].label {
            assert!(!label.contains("flat:"), "label was: {label}");
            assert!(label.contains("cum:"), "label was: {label}");
        }
    }

    #[test]
    fn test_threshold_with_min_flat() {
        let data = make_test_data();
        let mut config = make_test_config();
        config.threshold.min_percent = 100.0; // Nothing passes percent threshold
        config.threshold.min_flat = Some(10_000_000); // 10ms flat threshold

        let range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 30,
                character: 0,
            },
        };

        let hints = generate_inlay_hints(&data, &config, "main.go", &range);
        // Line 10: flat=50M (passes), Line 15: flat=15M (passes), Line 20: flat=100K (fails)
        assert_eq!(hints.len(), 2);
    }

    #[test]
    fn test_format_hint_label_both() {
        let label = format_hint_label(
            15_000_000,    // 15ms
            340_000_000,   // 340ms
            1_000_000_000, // 1s total
            ValueUnit::Nanoseconds,
            true,
            true,
        );
        assert_eq!(label, "  flat: 15ms | cum: 340ms (34.0%)");
    }
}
