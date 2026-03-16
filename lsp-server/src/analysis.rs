use crate::format::ValueUnit;
use crate::profile::{proto, resolve_string};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// The type of profile (detected from sample_type labels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileType {
    Cpu,
    Heap,
    Allocs,
    Block,
    #[allow(dead_code)]
    Mutex,
    Goroutine,
    Unknown,
}

/// Aggregated profiling data ready for LSP consumption.
#[derive(Debug, Clone)]
pub struct ProfileData {
    /// Per-file, per-line costs. Outer key: resolved file path.
    pub line_costs: HashMap<String, BTreeMap<u64, LineCost>>,
    /// Top functions ranked by cumulative cost.
    pub hotspots: Vec<HotspotFunction>,
    /// What kind of profile.
    #[allow(dead_code)]
    pub profile_type: ProfileType,
    /// Human-readable description of the value (e.g., "cpu").
    pub sample_type_label: String,
    /// Unit for formatting.
    pub value_unit: ValueUnit,
    /// Total value across all samples (for percentage calculation).
    pub total_value: i64,
    /// Profile collection duration, if available.
    #[allow(dead_code)]
    pub duration: Option<Duration>,
}

/// Per-line cost aggregation.
#[derive(Debug, Clone, Default)]
pub struct LineCost {
    /// Cost directly at this line (line was leaf of call stack).
    pub flat: i64,
    /// Cost at this line including all callees.
    pub cumulative: i64,
}

/// A function ranked as a hotspot.
#[derive(Debug, Clone)]
pub struct HotspotFunction {
    pub rank: usize,
    #[allow(dead_code)]
    pub name: String,
    pub filename: String,
    pub start_line: u64,
    pub flat: i64,
    pub cumulative: i64,
}

/// Internal struct for accumulating per-function costs during analysis.
#[derive(Debug, Default)]
struct FunctionCost {
    name: String,
    filename: String,
    start_line: u64,
    flat: i64,
    cumulative: i64,
}

/// Select which value column to use based on sample_type labels.
/// Returns (index, profile_type, label, unit).
fn select_sample_type(profile: &proto::Profile) -> (usize, ProfileType, String, ValueUnit) {
    let sample_types = &profile.sample_type;

    if sample_types.is_empty() {
        return (
            0,
            ProfileType::Unknown,
            "unknown".to_string(),
            ValueUnit::Count,
        );
    }

    // Check if "contentions" is present (indicates block/mutex profile).
    let has_contentions = sample_types
        .iter()
        .any(|st| resolve_string(profile, st.r#type) == "contentions");

    // Try matching rules in order.
    for (i, st) in sample_types.iter().enumerate() {
        let type_name = resolve_string(profile, st.r#type);
        let unit_name = resolve_string(profile, st.unit);

        match (type_name, unit_name) {
            ("cpu", "nanoseconds") => {
                return (
                    i,
                    ProfileType::Cpu,
                    "cpu".to_string(),
                    ValueUnit::Nanoseconds,
                );
            }
            ("inuse_space", "bytes") => {
                return (
                    i,
                    ProfileType::Heap,
                    "inuse_space".to_string(),
                    ValueUnit::Bytes,
                );
            }
            ("alloc_space", "bytes") => {
                return (
                    i,
                    ProfileType::Allocs,
                    "alloc_space".to_string(),
                    ValueUnit::Bytes,
                );
            }
            ("delay", "nanoseconds") if has_contentions => {
                // Could be block or mutex — we don't distinguish for now.
                return (
                    i,
                    ProfileType::Block,
                    "delay".to_string(),
                    ValueUnit::Nanoseconds,
                );
            }
            ("goroutine", "count") => {
                return (
                    i,
                    ProfileType::Goroutine,
                    "goroutine".to_string(),
                    ValueUnit::Count,
                );
            }
            _ => {}
        }
    }

    // Fallback: last entry (or only entry).
    let idx = sample_types.len() - 1;
    let st = &sample_types[idx];
    let unit_name = resolve_string(profile, st.unit);
    let type_name = resolve_string(profile, st.r#type);

    let unit = match unit_name {
        "nanoseconds" => ValueUnit::Nanoseconds,
        "bytes" => ValueUnit::Bytes,
        _ => ValueUnit::Count,
    };

    (idx, ProfileType::Unknown, type_name.to_string(), unit)
}

/// Analyze a parsed profile and produce aggregated ProfileData.
///
/// `max_hotspots` controls how many top functions to include in the hotspot list.
pub fn analyze_profile(profile: &proto::Profile, max_hotspots: usize) -> ProfileData {
    let (value_index, profile_type, sample_type_label, value_unit) = select_sample_type(profile);

    // Build location lookup: id -> &Location.
    let location_map: HashMap<u64, &proto::Location> =
        profile.location.iter().map(|loc| (loc.id, loc)).collect();

    // Build function lookup: id -> &Function.
    let function_map: HashMap<u64, &proto::Function> =
        profile.function.iter().map(|f| (f.id, f)).collect();

    let mut line_costs: HashMap<String, BTreeMap<u64, LineCost>> = HashMap::new();
    let mut func_costs: HashMap<u64, FunctionCost> = HashMap::new();
    let mut total_value: i64 = 0;

    for sample in &profile.sample {
        let value = sample.value.get(value_index).copied().unwrap_or(0);
        total_value += value;

        for (loc_idx, &location_id) in sample.location_id.iter().enumerate() {
            let Some(location) = location_map.get(&location_id) else {
                continue;
            };

            for (line_idx, line) in location.line.iter().enumerate() {
                let Some(func) = function_map.get(&line.function_id) else {
                    continue;
                };

                let filename = resolve_string(profile, func.filename).to_string();
                if filename.is_empty() {
                    continue;
                }

                if line.line <= 0 {
                    continue;
                }
                let line_no = line.line as u64;

                // Accumulate line costs.
                let file_costs = line_costs.entry(filename.clone()).or_default();
                let cost = file_costs.entry(line_no).or_default();
                cost.cumulative += value;

                // Flat cost: only the innermost line at the leaf location.
                if loc_idx == 0 && line_idx == 0 {
                    cost.flat += value;
                }

                // Accumulate function costs.
                let fc = func_costs.entry(func.id).or_default();
                if fc.name.is_empty() {
                    fc.name = resolve_string(profile, func.name).to_string();
                    fc.filename = filename;
                    fc.start_line = func.start_line as u64;
                }
                fc.cumulative += value;
                if loc_idx == 0 && line_idx == 0 {
                    fc.flat += value;
                }
            }
        }
    }

    // Build hotspots: sort by cumulative cost descending, take top N.
    let mut func_list: Vec<FunctionCost> = func_costs.into_values().collect();
    func_list.sort_by(|a, b| b.cumulative.cmp(&a.cumulative));
    func_list.truncate(max_hotspots);

    let hotspots: Vec<HotspotFunction> = func_list
        .into_iter()
        .enumerate()
        .map(|(i, fc)| HotspotFunction {
            rank: i + 1,
            name: fc.name,
            filename: fc.filename,
            start_line: fc.start_line,
            flat: fc.flat,
            cumulative: fc.cumulative,
        })
        .collect();

    let duration = if profile.duration_nanos > 0 {
        Some(Duration::from_nanos(profile.duration_nanos as u64))
    } else {
        None
    };

    ProfileData {
        line_costs,
        hotspots,
        profile_type,
        sample_type_label,
        value_unit,
        total_value,
        duration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a minimal profile for testing.
    fn make_test_profile() -> proto::Profile {
        // String table: 0="" 1="cpu" 2="nanoseconds" 3="main.go" 4="main" 5="handler.go" 6="handleRequest"
        let string_table = vec![
            "".to_string(),
            "cpu".to_string(),
            "nanoseconds".to_string(),
            "main.go".to_string(),
            "main".to_string(),
            "handler.go".to_string(),
            "handleRequest".to_string(),
        ];

        let sample_type = vec![proto::ValueType { r#type: 1, unit: 2 }]; // cpu/nanoseconds

        let functions = vec![
            proto::Function {
                id: 1,
                name: 4, // "main"
                system_name: 4,
                filename: 3, // "main.go"
                start_line: 10,
            },
            proto::Function {
                id: 2,
                name: 6, // "handleRequest"
                system_name: 6,
                filename: 5, // "handler.go"
                start_line: 25,
            },
        ];

        let locations = vec![
            proto::Location {
                id: 1,
                mapping_id: 0,
                address: 0,
                line: vec![proto::Line {
                    function_id: 1,
                    line: 15,
                }], // main.go:15
                is_folded: false,
            },
            proto::Location {
                id: 2,
                mapping_id: 0,
                address: 0,
                line: vec![proto::Line {
                    function_id: 2,
                    line: 30,
                }], // handler.go:30
                is_folded: false,
            },
        ];

        // Sample 1: handler.go:30 -> main.go:15 (leaf is handler.go:30)
        // value = 100_000_000 (100ms)
        let samples = vec![
            proto::Sample {
                location_id: vec![2, 1], // leaf first: location 2 (handler), then location 1 (main)
                value: vec![100_000_000],
                label: vec![],
            },
            // Sample 2: main.go:15 is leaf
            // value = 50_000_000 (50ms)
            proto::Sample {
                location_id: vec![1],
                value: vec![50_000_000],
                label: vec![],
            },
        ];

        proto::Profile {
            sample_type,
            sample: samples,
            mapping: vec![],
            location: locations,
            function: functions,
            string_table,
            duration_nanos: 5_000_000_000, // 5 seconds
            ..Default::default()
        }
    }

    #[test]
    fn test_select_sample_type_cpu() {
        let profile = make_test_profile();
        let (idx, ptype, label, unit) = select_sample_type(&profile);
        assert_eq!(idx, 0);
        assert_eq!(ptype, ProfileType::Cpu);
        assert_eq!(label, "cpu");
        assert_eq!(unit, ValueUnit::Nanoseconds);
    }

    #[test]
    fn test_select_sample_type_heap() {
        let mut profile = make_test_profile();
        // Change to heap profile: string table needs inuse_space and bytes
        profile.string_table.push("inuse_space".to_string()); // index 7
        profile.string_table.push("bytes".to_string()); // index 8
        profile.sample_type = vec![proto::ValueType { r#type: 7, unit: 8 }];

        let (_, ptype, _, unit) = select_sample_type(&profile);
        assert_eq!(ptype, ProfileType::Heap);
        assert_eq!(unit, ValueUnit::Bytes);
    }

    #[test]
    fn test_analyze_total_value() {
        let profile = make_test_profile();
        let data = analyze_profile(&profile, 50);
        // 100ms + 50ms = 150ms
        assert_eq!(data.total_value, 150_000_000);
        assert_eq!(data.profile_type, ProfileType::Cpu);
        assert_eq!(data.value_unit, ValueUnit::Nanoseconds);
    }

    #[test]
    fn test_analyze_line_costs() {
        let profile = make_test_profile();
        let data = analyze_profile(&profile, 50);

        // main.go:15 — appears in both samples:
        //   Sample 1: cumulative (not leaf in this sample, loc_idx=1) += 100M
        //   Sample 2: cumulative (leaf, loc_idx=0) += 50M, flat += 50M
        let main_costs = data.line_costs.get("main.go").unwrap();
        let cost_15 = main_costs.get(&15).unwrap();
        assert_eq!(cost_15.cumulative, 150_000_000); // 100M + 50M
        assert_eq!(cost_15.flat, 50_000_000); // only sample 2 (leaf)

        // handler.go:30 — appears in sample 1 as leaf:
        //   cumulative += 100M, flat += 100M
        let handler_costs = data.line_costs.get("handler.go").unwrap();
        let cost_30 = handler_costs.get(&30).unwrap();
        assert_eq!(cost_30.cumulative, 100_000_000);
        assert_eq!(cost_30.flat, 100_000_000);
    }

    #[test]
    fn test_analyze_hotspots() {
        let profile = make_test_profile();
        let data = analyze_profile(&profile, 50);

        assert_eq!(data.hotspots.len(), 2);
        // Ranked by cumulative: main (150M) > handleRequest (100M)
        assert_eq!(data.hotspots[0].name, "main");
        assert_eq!(data.hotspots[0].cumulative, 150_000_000);
        assert_eq!(data.hotspots[0].rank, 1);
        assert_eq!(data.hotspots[0].start_line, 10);

        assert_eq!(data.hotspots[1].name, "handleRequest");
        assert_eq!(data.hotspots[1].cumulative, 100_000_000);
        assert_eq!(data.hotspots[1].rank, 2);
    }

    #[test]
    fn test_analyze_hotspots_truncation() {
        let profile = make_test_profile();
        let data = analyze_profile(&profile, 1);
        assert_eq!(data.hotspots.len(), 1);
        assert_eq!(data.hotspots[0].name, "main");
    }

    #[test]
    fn test_analyze_duration() {
        let profile = make_test_profile();
        let data = analyze_profile(&profile, 50);
        assert_eq!(data.duration, Some(Duration::from_secs(5)));
    }

    #[test]
    fn test_analyze_empty_profile() {
        let profile = proto::Profile::default();
        let data = analyze_profile(&profile, 50);
        assert!(data.line_costs.is_empty());
        assert!(data.hotspots.is_empty());
        assert_eq!(data.total_value, 0);
    }

    #[test]
    fn test_inlined_functions() {
        // Test that inlined functions (multiple lines per location) are handled correctly.
        let string_table = vec![
            "".to_string(),            // 0
            "cpu".to_string(),         // 1
            "nanoseconds".to_string(), // 2
            "util.go".to_string(),     // 3
            "innerFunc".to_string(),   // 4
            "outerFunc".to_string(),   // 5
        ];

        let functions = vec![
            proto::Function {
                id: 1,
                name: 4,
                system_name: 4,
                filename: 3,
                start_line: 10,
            },
            proto::Function {
                id: 2,
                name: 5,
                system_name: 5,
                filename: 3,
                start_line: 20,
            },
        ];

        // One location with two lines (innerFunc inlined into outerFunc).
        let locations = vec![proto::Location {
            id: 1,
            mapping_id: 0,
            address: 0,
            line: vec![
                proto::Line {
                    function_id: 1,
                    line: 12,
                }, // line[0] = innerFunc (inlined)
                proto::Line {
                    function_id: 2,
                    line: 22,
                }, // line[1] = outerFunc (caller)
            ],
            is_folded: false,
        }];

        let samples = vec![proto::Sample {
            location_id: vec![1], // This is the leaf location
            value: vec![10_000_000],
            label: vec![],
        }];

        let profile = proto::Profile {
            sample_type: vec![proto::ValueType { r#type: 1, unit: 2 }],
            sample: samples,
            location: locations,
            function: functions,
            string_table,
            ..Default::default()
        };

        let data = analyze_profile(&profile, 50);
        let costs = data.line_costs.get("util.go").unwrap();

        // line 12 (innerFunc, line_idx=0, loc_idx=0): flat=10M, cum=10M
        let cost_12 = costs.get(&12).unwrap();
        assert_eq!(cost_12.flat, 10_000_000);
        assert_eq!(cost_12.cumulative, 10_000_000);

        // line 22 (outerFunc, line_idx=1, loc_idx=0): flat=0, cum=10M
        let cost_22 = costs.get(&22).unwrap();
        assert_eq!(cost_22.flat, 0);
        assert_eq!(cost_22.cumulative, 10_000_000);
    }
}
