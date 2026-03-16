use crate::analysis::ProfileData;
use crate::config::Config;
use crate::format::{format_percent, format_value};
use tower_lsp::lsp_types::*;

/// Generate code lenses for a specific file.
///
/// `file_key` is the workspace-relative path as stored in `ProfileData`.
/// Returns code lenses for hotspot functions that appear in this file.
pub fn generate_code_lenses(data: &ProfileData, config: &Config, file_key: &str) -> Vec<CodeLens> {
    let mut lenses: Vec<CodeLens> = data
        .hotspots
        .iter()
        .filter(|h| h.filename == file_key)
        .take(config.display.max_code_lenses)
        .map(|hotspot| {
            let title = format_lens_title(hotspot, data);

            // If start_line is 0 (missing), use first profiled line for this function.
            let line = if hotspot.start_line > 0 {
                hotspot.start_line - 1 // Convert to 0-indexed
            } else {
                // Fallback: find the first profiled line in the file for this function.
                // This is approximate — we use the first line with cost data.
                find_first_profiled_line(data, file_key).unwrap_or(0)
            };

            CodeLens {
                range: Range {
                    start: Position {
                        line: line as u32,
                        character: 0,
                    },
                    end: Position {
                        line: line as u32,
                        character: 0,
                    },
                },
                command: Some(Command {
                    title,
                    command: String::new(), // Display-only, no action.
                    arguments: None,
                }),
                data: None,
            }
        })
        .collect();

    // Sort by line number for consistent ordering in the editor.
    lenses.sort_by_key(|l| l.range.start.line);

    lenses
}

/// Format the code lens title.
/// Example: "# #1 hotspot — flat: 2.1s (34%) | cum: 4.3s (70%)"
fn format_lens_title(hotspot: &crate::analysis::HotspotFunction, data: &ProfileData) -> String {
    let flat_str = format_value(hotspot.flat, data.value_unit);
    let flat_pct = format_percent(hotspot.flat, data.total_value);
    let cum_str = format_value(hotspot.cumulative, data.value_unit);
    let cum_pct = format_percent(hotspot.cumulative, data.total_value);

    format!(
        "# #{} hotspot — flat: {} {} | cum: {} {}",
        hotspot.rank, flat_str, flat_pct, cum_str, cum_pct
    )
}

/// Find the first (lowest) profiled line number for a file.
/// Returns 0-indexed line number, or None if no costs exist.
fn find_first_profiled_line(data: &ProfileData, file_key: &str) -> Option<u64> {
    data.line_costs
        .get(file_key)
        .and_then(|costs| costs.keys().next().copied())
        .map(|line_no| line_no.saturating_sub(1)) // Convert to 0-indexed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{HotspotFunction, LineCost, ProfileData, ProfileType};
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
            25,
            LineCost {
                flat: 100_000_000,
                cumulative: 700_000_000,
            },
        );

        let mut line_costs = HashMap::new();
        line_costs.insert("handler.go".to_string(), file_costs);

        ProfileData {
            line_costs,
            hotspots: vec![
                HotspotFunction {
                    rank: 1,
                    name: "handleRequest".to_string(),
                    filename: "handler.go".to_string(),
                    start_line: 20,
                    flat: 100_000_000,
                    cumulative: 700_000_000,
                },
                HotspotFunction {
                    rank: 2,
                    name: "processData".to_string(),
                    filename: "handler.go".to_string(),
                    start_line: 5,
                    flat: 50_000_000,
                    cumulative: 340_000_000,
                },
                HotspotFunction {
                    rank: 3,
                    name: "otherFunc".to_string(),
                    filename: "other.go".to_string(),
                    start_line: 1,
                    flat: 10_000_000,
                    cumulative: 50_000_000,
                },
            ],
            profile_type: ProfileType::Cpu,
            sample_type_label: "cpu".to_string(),
            value_unit: ValueUnit::Nanoseconds,
            total_value: 1_000_000_000,
            duration: None,
        }
    }

    fn make_test_config() -> Config {
        Config {
            display: DisplayConfig {
                max_code_lenses: 10,
                max_hotspots: 50,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_generate_lenses_basic() {
        let data = make_test_data();
        let config = make_test_config();

        let lenses = generate_code_lenses(&data, &config, "handler.go");
        assert_eq!(lenses.len(), 2); // Only hotspots in handler.go

        // Sorted by line: processData (start_line=5, 0-indexed=4) first
        assert_eq!(lenses[0].range.start.line, 4);
        assert_eq!(lenses[1].range.start.line, 19); // start_line=20, 0-indexed=19

        // Verify title format
        let title = &lenses[1].command.as_ref().unwrap().title;
        assert!(title.contains("#1 hotspot"), "title was: {title}");
        assert!(title.contains("cum: 700ms"), "title was: {title}");
    }

    #[test]
    fn test_generate_lenses_max_limit() {
        let data = make_test_data();
        let mut config = make_test_config();
        config.display.max_code_lenses = 1;

        let lenses = generate_code_lenses(&data, &config, "handler.go");
        assert_eq!(lenses.len(), 1); // Capped at 1
    }

    #[test]
    fn test_generate_lenses_no_file() {
        let data = make_test_data();
        let config = make_test_config();

        let lenses = generate_code_lenses(&data, &config, "nonexistent.go");
        assert!(lenses.is_empty());
    }

    #[test]
    fn test_generate_lenses_other_file() {
        let data = make_test_data();
        let config = make_test_config();

        let lenses = generate_code_lenses(&data, &config, "other.go");
        assert_eq!(lenses.len(), 1);
        let title = &lenses[0].command.as_ref().unwrap().title;
        assert!(title.contains("#3 hotspot"), "title was: {title}");
    }

    #[test]
    fn test_lens_with_zero_start_line() {
        let mut data = make_test_data();
        // Set start_line to 0 (missing).
        data.hotspots[0].start_line = 0;

        let config = make_test_config();
        let lenses = generate_code_lenses(&data, &config, "handler.go");

        // Should fall back to first profiled line (line_no=10, 0-indexed=9).
        let handle_lens = lenses
            .iter()
            .find(|l| l.command.as_ref().unwrap().title.contains("#1 hotspot"))
            .unwrap();
        assert_eq!(handle_lens.range.start.line, 9);
    }

    #[test]
    fn test_lens_command_is_empty() {
        let data = make_test_data();
        let config = make_test_config();

        let lenses = generate_code_lenses(&data, &config, "handler.go");
        for lens in &lenses {
            assert_eq!(lens.command.as_ref().unwrap().command, "");
        }
    }

    #[test]
    fn test_format_lens_title() {
        let data = make_test_data();
        let title = format_lens_title(&data.hotspots[0], &data);
        assert_eq!(
            title,
            "# #1 hotspot — flat: 100ms (10.0%) | cum: 700ms (70.0%)"
        );
    }
}
