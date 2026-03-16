# Go Profile LSP Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust LSP server that parses Go pprof files and surfaces per-line cost annotations (inlay hints) and function-level hotspot summaries (code lenses) in Zed editor.

**Architecture:** Two-component monorepo — a thin Zed WASM extension (~50 lines) that downloads and launches a standalone Rust LSP server binary. The LSP server parses gzip-compressed protobuf pprof files, aggregates per-line costs, and serves inlay hints and code lenses over stdin/stdout JSON-RPC. All analysis logic lives in the LSP server.

**Tech Stack:** Rust, tower-lsp (LSP protocol), prost/prost-build (protobuf), flate2 (gzip), tokio (async runtime), serde/serde_json (config), glob (file discovery), tracing (logging), zed_extension_api (WASM extension)

**Spec:** `docs/superpowers/specs/2026-03-16-go-profile-zed-extension-design.md`

---

## File Structure

```
zed-go-profile/
  extension/                    # Thin Zed WASM extension
    extension.toml              # Extension metadata, language server registration
    Cargo.toml                  # WASM crate dependencies (zed_extension_api)
    src/lib.rs                  # Extension trait impl, binary download/launch
    languages/go/config.toml    # Go language binding for the LSP

  lsp-server/                   # Standalone Rust LSP server binary
    Cargo.toml                  # Binary crate dependencies
    build.rs                    # prost-build: compile profile.proto at build time
    proto/
      profile.proto             # Google pprof protobuf schema (vendored)
    src/
      main.rs                   # Entrypoint: tokio runtime, tower-lsp Server setup
      server.rs                 # Backend struct, LanguageServer trait impl, lifecycle
      config.rs                 # InitializationOptions deserialization, defaults
      profile.rs                # Pprof parsing: gzip detect, decompress, protobuf decode
      analysis.rs               # Cost aggregation: walk samples, build LineCost/HotspotFunction
      format.rs                 # Value formatting: nanoseconds, bytes, counts, percentages
      paths.rs                  # File path resolution: exact, trim, suffix, source root matching
      hints.rs                  # Inlay hint generation from line_costs
      lenses.rs                 # Code lens generation from hotspots
      watch.rs                  # Profile file watcher: poll, detect changes, trigger refresh
```

**Design rationale:**
- `server.rs` is separate from `main.rs` to keep the Backend/LanguageServer impl testable without transport concerns.
- `config.rs` is its own file because the InitializationOptions struct has many fields with defaults — keeps server.rs focused on protocol.
- `format.rs` is separate from `analysis.rs` because formatting is pure/stateless and highly testable in isolation.
- `profile.rs` owns protobuf decoding; `analysis.rs` owns the cost aggregation logic that consumes decoded profiles. This separation allows testing parsing and analysis independently.
- `watch.rs` encapsulates the polling/change-detection loop, keeping it out of the server lifecycle code.

---

## Chunk 1: Project Scaffolding + Profile Parsing

### Task 1: LSP Server Project Scaffolding

**Files:**
- Create: `lsp-server/Cargo.toml`
- Create: `lsp-server/build.rs`
- Create: `lsp-server/proto/profile.proto`
- Create: `lsp-server/src/main.rs` (minimal placeholder)

- [ ] **Step 1: Create `lsp-server/Cargo.toml`**

```toml
[package]
name = "go-profile-lsp"
version = "0.0.1"
edition = "2021"

[dependencies]
tower-lsp = "0.20"
tokio = { version = "1", features = ["full"] }
prost = "0.13"
flate2 = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
glob = "0.3"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
tempfile = "3"

[build-dependencies]
prost-build = "0.13"
```

Note: Use prost 0.13 (not 0.14) because tower-lsp 0.20 pins to lsp-types 0.94 which is compatible. The prost 0.13 API is identical for our use case (`Message::decode`, `encode_to_vec`). If 0.13 causes issues, bump to 0.14.

- [ ] **Step 2: Vendor `profile.proto`**

Create `lsp-server/proto/profile.proto`. This is the Google pprof protobuf schema from https://github.com/google/pprof/blob/main/proto/profile.proto. Vendor the full file:

```protobuf
// Copyright 2016 Google Inc. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

syntax = "proto3";

package perftools.profiles;

option java_package = "com.google.perftools.profiles";
option java_outer_classname = "ProfileProto";

message Profile {
  // A description of the string table, samples, mappings and profile metadata.
  repeated ValueType sample_type = 1;

  repeated Sample sample = 2;

  repeated Mapping mapping = 3;

  repeated Location location = 4;

  repeated Function function = 5;

  // A common table for strings referenced by various messages.
  // string_table[0] must always be "".
  repeated string string_table = 6;

  // frames with Function.function_name fully matching the following
  // regexp will be dropped from the samples, along with their successors.
  int64 drop_frames = 7;   // Index into string table.

  // frames with Function.function_name fully matching the following
  // regexp will be kept, even if it matches drop_frames.
  int64 keep_frames = 8;   // Index into string table.

  // Time of collection (UTC) represented as nanoseconds past the epoch.
  int64 time_nanos = 9;

  // Duration of the profile, if a duration makes sense.
  int64 duration_nanos = 10;

  // The kind of events between sampled occurrences.
  // e.g [ "cpu","cycles" ] or [ "heap","bytes" ]
  ValueType period_type = 11;

  // The number of events between sampled occurrences.
  int64 period = 12;

  // Freeform text associated to the profile.
  repeated int64 comment = 13; // Indices into string table.

  // Index into the string table of the type of the preferred sample
  // value. If unset, clients should default to the last sample value.
  int64 default_sample_type = 14;
}

// ValueType describes the semantics and measurement units of a value.
message ValueType {
  int64 type = 1; // Index into string table.
  int64 unit = 2; // Index into string table.
}

// Each Sample records values encountered in some program
// context. The program context is typically a stack trace, perhaps
// augmented with auxiliary information like the thread-id, some
// indicator of a skill's higher level request, etc.
message Sample {
  // The ids recorded here correspond to a Profile.location.id.
  // The leaf is at location_id[0].
  repeated uint64 location_id = 1;

  // The type and unit of each value is defined by the corresponding
  // entry in Profile.sample_type. All samples must have the same
  // number of values, the same as the length of Profile.sample_type.
  // When aggregating multiple samples into a single sample, the
  // result has a list of values that is the element-wise sum of the
  // lists of the originals.
  repeated int64 value = 2;

  // label includes additional context for this sample. It can include
  // things like a thread id, allocation size, etc
  repeated Label label = 3;
}

// Provides additional context for a sample,
// for instance the number of bytes allocated for objects at a callsite.
message Label {
  int64 key = 1; // Index into string table

  // At most one of the following must be present
  int64 str = 2; // Index into string table
  int64 num = 3; // Integer value for this label
  // can be positive or negative

  // Specifies the units of num.
  // Use arbitrary string (for example, "requests") as a custom count unit.
  // If no unit is specified, consumer may apply heuristic to deduce the unit.
  // Consumers may also interpret units like "bytes" and "kilobytes" as memory
  // units and units like "seconds" and "nanoseconds" as time units,
  // andடிapply appropriate conversions.
  // Consumers that are not consumers of the proto may simply display the raw
  // number with the unit label.
  int64 num_unit = 4; // Index into string table
}

// Describes the mapping of a binary/library/framework into address space.
message Mapping {
  // Unique nonzero id for the mapping
  uint64 id = 1;

  // Address at which the binary (or DLL) is loaded into memory.
  uint64 memory_start = 2;

  // The limit of the address range occupied by this mapping.
  uint64 memory_limit = 3;

  // Offset in the binary that corresponds to the first mapped address.
  uint64 file_offset = 4;

  // The object this entry is loaded from.  This can be a filename on
  // disk for the main binary and shared libraries, or virtual
  // abstractions like "[vdso]".
  int64 filename = 5;  // Index into string table

  // A string that uniquely identifies a particular program version
  // with high probability. E.g., for binaries generated by GNU tools,
  // it could be the contents of the .note.gnu.build-id field.
  int64 build_id = 6;  // Index into string table

  // The following fields indicate the resolution of symbolic info.
  bool has_functions = 7;
  bool has_filenames = 8;
  bool has_line_numbers = 9;
  bool has_inline_frames = 10;
}

// Describes function and line number information for a Location.
message Location {
  // Unique nonzero id for the location.  A profile could use
  // instruction addresses or any integer sequence as ids.
  uint64 id = 1;

  // The id of the corresponding profile.Mapping for this location.
  // It can be unset if the mapping is unknown or not applicable for
  // this profile type.
  uint64 mapping_id = 2;

  // The instruction address for this location, if available.  It
  // should be within [Mapping.memory_start...Mapping.memory_limit]
  // for the corresponding mapping. A non-leaf address may be in the
  // middle of a call instruction. It is up to display tools to find
  // the beginning of the instruction if necessary.
  uint64 address = 3;

  // Multiple line indicates this location has inlined functions,
  // where the last entry represents the caller into which the
  // preceding entries were inlined.
  //
  // E.g., if memcpy() is inlined into printf:
  //    line[0].function_name == "memcpy"
  //    line[1].function_name == "printf"
  repeated Line line = 4;

  // Provides an indication that multiple symbols map to this location's
  // address, for example due to identical code folding by the linker. In that
  // case the line information above represents one of the multiple
  // symbols. This field must be recomputed when the symbolization state of the
  // profile changes.
  bool is_folded = 5;
}

message Line {
  // The id of the corresponding profile.Function for this line.
  uint64 function_id = 1;

  // Line number in source code.
  int64 line = 2;
}

message Function {
  // Unique nonzero id for the function.
  uint64 id = 1;

  // Name of the function, in human-readable form if available.
  int64 name = 2;         // Index into string table

  // Name of the function, as identified by the system.
  // For instance, it can be a C++ mangled name.
  int64 system_name = 3;  // Index into string table

  // Source file containing the function.
  int64 filename = 4;     // Index into string table

  // Line number in source file.
  int64 start_line = 5;
}
```

- [ ] **Step 3: Create `lsp-server/build.rs`**

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    prost_build::compile_protos(&["proto/profile.proto"], &["proto/"])?;
    Ok(())
}
```

- [ ] **Step 4: Create minimal `lsp-server/src/main.rs`**

```rust
fn main() {
    println!("go-profile-lsp placeholder");
}
```

- [ ] **Step 5: Verify project compiles**

Run: `cargo build` (in `lsp-server/`)
Expected: Successful compilation. The prost-build step generates `perftools.profiles.rs` in `OUT_DIR`. Requires `protoc` on PATH — if missing, install with `brew install protobuf` (macOS) or `apt install protobuf-compiler` (Linux).

- [ ] **Step 6: Commit**

```bash
git add lsp-server/
git commit -m "scaffold: lsp-server project with prost-build and vendored profile.proto"
```

---

### Task 2: Profile Parsing (`profile.rs`)

**Files:**
- Create: `lsp-server/src/profile.rs`

**Dependencies:** Task 1 (proto compilation)

This module handles: gzip detection, decompression, protobuf decoding, and string table resolution. It exposes a high-level `parse_profile(bytes) -> Result<Profile>` function that returns the raw prost-generated `Profile` struct.

- [ ] **Step 1: Write failing test for gzip detection**

Create `lsp-server/src/profile.rs`:

```rust
use flate2::read::GzDecoder;
use prost::Message;
use std::io::Read;
use std::path::Path;

/// Include prost-generated protobuf types.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/perftools.profiles.rs"));
}

/// Errors that can occur during profile parsing.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("Empty profile data")]
    Empty,
}

/// Check if data starts with gzip magic bytes (0x1f 0x8b).
fn is_gzip(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

/// Parse a pprof profile from raw bytes.
/// Handles both gzip-compressed and raw protobuf formats.
pub fn parse_profile(data: &[u8]) -> Result<proto::Profile, ParseError> {
    if data.is_empty() {
        return Err(ParseError::Empty);
    }

    let bytes = if is_gzip(data) {
        let mut decoder = GzDecoder::new(data);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        decompressed
    } else {
        data.to_vec()
    };

    let profile = proto::Profile::decode(bytes.as_slice())?;
    Ok(profile)
}

/// Parse a pprof profile from a file path.
pub fn parse_profile_file(path: &Path) -> Result<proto::Profile, ParseError> {
    let data = std::fs::read(path)?;
    parse_profile(&data)
}

/// Resolve a string from the profile's string table by index.
/// Returns empty string if index is out of bounds or negative.
pub fn resolve_string(profile: &proto::Profile, index: i64) -> &str {
    if index < 0 {
        return "";
    }
    profile
        .string_table
        .get(index as usize)
        .map(|s| s.as_str())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_gzip_true() {
        assert!(is_gzip(&[0x1f, 0x8b, 0x08, 0x00]));
    }

    #[test]
    fn test_is_gzip_false() {
        assert!(!is_gzip(&[0x0a, 0x0b, 0x0c]));
    }

    #[test]
    fn test_is_gzip_too_short() {
        assert!(!is_gzip(&[0x1f]));
        assert!(!is_gzip(&[]));
    }

    #[test]
    fn test_parse_empty_returns_error() {
        assert!(matches!(parse_profile(&[]), Err(ParseError::Empty)));
    }

    #[test]
    fn test_parse_raw_protobuf() {
        // Encode a minimal profile and parse it back.
        let profile = proto::Profile {
            string_table: vec!["".to_string(), "cpu".to_string(), "nanoseconds".to_string()],
            sample_type: vec![proto::ValueType { r#type: 1, unit: 2 }],
            ..Default::default()
        };
        let encoded = profile.encode_to_vec();
        let parsed = parse_profile(&encoded).unwrap();
        assert_eq!(parsed.string_table.len(), 3);
        assert_eq!(parsed.string_table[1], "cpu");
        assert_eq!(parsed.sample_type.len(), 1);
    }

    #[test]
    fn test_parse_gzipped_protobuf() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let profile = proto::Profile {
            string_table: vec!["".to_string(), "heap".to_string(), "bytes".to_string()],
            sample_type: vec![proto::ValueType { r#type: 1, unit: 2 }],
            ..Default::default()
        };
        let encoded = profile.encode_to_vec();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&encoded).unwrap();
        let gzipped = encoder.finish().unwrap();

        let parsed = parse_profile(&gzipped).unwrap();
        assert_eq!(parsed.string_table[1], "heap");
    }

    #[test]
    fn test_resolve_string() {
        let profile = proto::Profile {
            string_table: vec![
                "".to_string(),
                "main.go".to_string(),
                "runtime.mallocgc".to_string(),
            ],
            ..Default::default()
        };
        assert_eq!(resolve_string(&profile, 0), "");
        assert_eq!(resolve_string(&profile, 1), "main.go");
        assert_eq!(resolve_string(&profile, 2), "runtime.mallocgc");
        assert_eq!(resolve_string(&profile, 99), ""); // out of bounds
        assert_eq!(resolve_string(&profile, -1), ""); // negative
    }
}
```

Note: `thiserror` is already in `Cargo.toml` dependencies.

- [ ] **Step 2: Register module in `main.rs`**

Add `mod profile;` to `lsp-server/src/main.rs`:

```rust
mod profile;

fn main() {
    println!("go-profile-lsp placeholder");
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib profile` (in `lsp-server/`)
Expected: All 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/
git commit -m "feat: profile parsing with gzip detection and protobuf decoding"
```

---

### Task 3: Configuration (`config.rs`)

**Files:**
- Create: `lsp-server/src/config.rs`

**Dependencies:** None (pure data types)

This module defines the InitializationOptions struct with all configuration fields and their defaults, matching the spec's JSON schema.

- [ ] **Step 1: Create `lsp-server/src/config.rs`**

```rust
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PathMappingConfig {
    /// Prefix to strip from profile file paths.
    pub trim_prefix: String,
    /// Source root to prepend after trimming.
    pub source_root: String,
}

impl Default for PathMappingConfig {
    fn default() -> Self {
        Self {
            trim_prefix: String::new(),
            source_root: String::new(),
        }
    }
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
```

- [ ] **Step 2: Register module in `main.rs`**

Update `lsp-server/src/main.rs`:

```rust
mod config;
mod profile;

fn main() {
    println!("go-profile-lsp placeholder");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib config` (in `lsp-server/`)
Expected: All 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/src/config.rs lsp-server/src/main.rs
git commit -m "feat: configuration module with serde deserialization and defaults"
```

---

### Task 4: Value Formatting (`format.rs`)

**Files:**
- Create: `lsp-server/src/format.rs`

**Dependencies:** None (pure functions)

Pure formatting functions for nanoseconds, bytes, counts, and percentages. Follows the spec's formatting rules exactly.

- [ ] **Step 1: Create `lsp-server/src/format.rs` with tests**

```rust
/// Format a nanosecond value into a human-readable duration string.
/// Uses the largest unit where value >= 1: s, ms, us, ns.
pub fn format_nanoseconds(ns: i64) -> String {
    let abs = ns.unsigned_abs();
    let sign = if ns < 0 { "-" } else { "" };

    if abs >= 1_000_000_000 {
        format!("{sign}{:.1}s", abs as f64 / 1_000_000_000.0)
    } else if abs >= 1_000_000 {
        format!("{sign}{:.0}ms", abs as f64 / 1_000_000.0)
    } else if abs >= 1_000 {
        format!("{sign}{:.0}us", abs as f64 / 1_000.0)
    } else {
        format!("{sign}{abs}ns")
    }
}

/// Format a byte value into a human-readable size string.
/// Uses the largest unit where value >= 1: GB, MB, KB, B.
pub fn format_bytes(bytes: i64) -> String {
    let abs = bytes.unsigned_abs();
    let sign = if bytes < 0 { "-" } else { "" };

    if abs >= 1_000_000_000 {
        format!("{sign}{:.1}GB", abs as f64 / 1_000_000_000.0)
    } else if abs >= 1_000_000 {
        format!("{sign}{:.0}MB", abs as f64 / 1_000_000.0)
    } else if abs >= 1_000 {
        format!("{sign}{:.0}KB", abs as f64 / 1_000.0)
    } else {
        format!("{sign}{abs}B")
    }
}

/// Format a count value with comma-separated thousands.
pub fn format_count(count: i64) -> String {
    if count == 0 {
        return "0".to_string();
    }

    let sign = if count < 0 { "-" } else { "" };
    let abs = count.unsigned_abs();
    let s = abs.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);

    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }

    format!("{sign}{result}")
}

/// Format a percentage value: "(34.2%)" relative to total.
/// Returns empty string if total is zero.
pub fn format_percent(value: i64, total: i64) -> String {
    if total == 0 {
        return String::new();
    }
    let pct = (value as f64 / total as f64) * 100.0;
    format!("({:.1}%)", pct)
}

/// The unit type for a profile's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueUnit {
    Nanoseconds,
    Bytes,
    Count,
}

/// Format a value according to its unit.
pub fn format_value(value: i64, unit: ValueUnit) -> String {
    match unit {
        ValueUnit::Nanoseconds => format_nanoseconds(value),
        ValueUnit::Bytes => format_bytes(value),
        ValueUnit::Count => format_count(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Nanoseconds
    #[test]
    fn test_format_ns_seconds() {
        assert_eq!(format_nanoseconds(1_200_000_000), "1.2s");
        assert_eq!(format_nanoseconds(5_500_000_000), "5.5s");
    }

    #[test]
    fn test_format_ns_milliseconds() {
        assert_eq!(format_nanoseconds(340_000_000), "340ms");
        assert_eq!(format_nanoseconds(1_500_000), "2ms");
    }

    #[test]
    fn test_format_ns_microseconds() {
        assert_eq!(format_nanoseconds(15_000), "15us");
        assert_eq!(format_nanoseconds(1_500), "2us");
    }

    #[test]
    fn test_format_ns_nanoseconds() {
        assert_eq!(format_nanoseconds(800), "800ns");
        assert_eq!(format_nanoseconds(0), "0ns");
    }

    // Bytes
    #[test]
    fn test_format_bytes_gb() {
        assert_eq!(format_bytes(1_500_000_000), "1.5GB");
    }

    #[test]
    fn test_format_bytes_mb() {
        assert_eq!(format_bytes(230_000_000), "230MB");
    }

    #[test]
    fn test_format_bytes_kb() {
        assert_eq!(format_bytes(12_000), "12KB");
    }

    #[test]
    fn test_format_bytes_b() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(0), "0B");
    }

    // Counts
    #[test]
    fn test_format_count_with_commas() {
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(0), "0");
    }

    #[test]
    fn test_format_count_negative() {
        assert_eq!(format_count(-1_234), "-1,234");
    }

    // Percentages
    #[test]
    fn test_format_percent() {
        assert_eq!(format_percent(342, 1000), "(34.2%)");
        assert_eq!(format_percent(1000, 1000), "(100.0%)");
        assert_eq!(format_percent(0, 1000), "(0.0%)");
    }

    #[test]
    fn test_format_percent_zero_total() {
        assert_eq!(format_percent(100, 0), "");
    }

    // format_value dispatch
    #[test]
    fn test_format_value() {
        assert_eq!(format_value(1_200_000_000, ValueUnit::Nanoseconds), "1.2s");
        assert_eq!(format_value(1_500_000_000, ValueUnit::Bytes), "1.5GB");
        assert_eq!(format_value(1_234_567, ValueUnit::Count), "1,234,567");
    }
}
```

- [ ] **Step 2: Register module in `main.rs`**

Update `lsp-server/src/main.rs`:

```rust
mod config;
mod format;
mod profile;

fn main() {
    println!("go-profile-lsp placeholder");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib format` (in `lsp-server/`)
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/src/format.rs lsp-server/src/main.rs
git commit -m "feat: value formatting for nanoseconds, bytes, counts, and percentages"
```

---

### Task 5: Cost Aggregation & Analysis (`analysis.rs`)

**Files:**
- Create: `lsp-server/src/analysis.rs`

**Dependencies:** Task 2 (`profile.rs`), Task 4 (`format.rs`)

This is the core analysis module. It walks profile samples, accumulates per-line costs, builds the hotspot function list, and detects profile type from sample_type labels.

- [ ] **Step 1: Create `lsp-server/src/analysis.rs` with core types and analysis logic**

```rust
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
    pub profile_type: ProfileType,
    /// Human-readable description of the value (e.g., "cpu").
    pub sample_type_label: String,
    /// Unit for formatting.
    pub value_unit: ValueUnit,
    /// Total value across all samples (for percentage calculation).
    pub total_value: i64,
    /// Profile collection duration, if available.
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
        return (0, ProfileType::Unknown, "unknown".to_string(), ValueUnit::Count);
    }

    // Check if "contentions" is present (indicates block/mutex profile).
    let has_contentions = sample_types.iter().any(|st| {
        resolve_string(profile, st.r#type) == "contentions"
    });

    // Try matching rules in order.
    for (i, st) in sample_types.iter().enumerate() {
        let type_name = resolve_string(profile, st.r#type);
        let unit_name = resolve_string(profile, st.unit);

        match (type_name, unit_name) {
            ("cpu", "nanoseconds") => {
                return (i, ProfileType::Cpu, "cpu".to_string(), ValueUnit::Nanoseconds);
            }
            ("inuse_space", "bytes") => {
                return (i, ProfileType::Heap, "inuse_space".to_string(), ValueUnit::Bytes);
            }
            ("alloc_space", "bytes") => {
                return (i, ProfileType::Allocs, "alloc_space".to_string(), ValueUnit::Bytes);
            }
            ("delay", "nanoseconds") if has_contentions => {
                // Could be block or mutex — we don't distinguish for now.
                return (i, ProfileType::Block, "delay".to_string(), ValueUnit::Nanoseconds);
            }
            ("goroutine", "count") => {
                return (i, ProfileType::Goroutine, "goroutine".to_string(), ValueUnit::Count);
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
    let location_map: HashMap<u64, &proto::Location> = profile
        .location
        .iter()
        .map(|loc| (loc.id, loc))
        .collect();

    // Build function lookup: id -> &Function.
    let function_map: HashMap<u64, &proto::Function> = profile
        .function
        .iter()
        .map(|f| (f.id, f))
        .collect();

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

                let line_no = line.line as u64;
                if line_no == 0 {
                    continue;
                }

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
                name: 4,       // "main"
                system_name: 4,
                filename: 3,   // "main.go"
                start_line: 10,
            },
            proto::Function {
                id: 2,
                name: 6,       // "handleRequest"
                system_name: 6,
                filename: 5,   // "handler.go"
                start_line: 25,
            },
        ];

        let locations = vec![
            proto::Location {
                id: 1,
                mapping_id: 0,
                address: 0,
                line: vec![proto::Line { function_id: 1, line: 15 }], // main.go:15
                is_folded: false,
            },
            proto::Location {
                id: 2,
                mapping_id: 0,
                address: 0,
                line: vec![proto::Line { function_id: 2, line: 30 }], // handler.go:30
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
        profile.string_table.push("bytes".to_string());       // index 8
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
        assert_eq!(cost_15.flat, 50_000_000);         // only sample 2 (leaf)

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
            "".to_string(),       // 0
            "cpu".to_string(),    // 1
            "nanoseconds".to_string(), // 2
            "util.go".to_string(), // 3
            "innerFunc".to_string(), // 4
            "outerFunc".to_string(), // 5
        ];

        let functions = vec![
            proto::Function { id: 1, name: 4, system_name: 4, filename: 3, start_line: 10 },
            proto::Function { id: 2, name: 5, system_name: 5, filename: 3, start_line: 20 },
        ];

        // One location with two lines (innerFunc inlined into outerFunc).
        let locations = vec![proto::Location {
            id: 1,
            mapping_id: 0,
            address: 0,
            line: vec![
                proto::Line { function_id: 1, line: 12 }, // line[0] = innerFunc (inlined)
                proto::Line { function_id: 2, line: 22 }, // line[1] = outerFunc (caller)
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
```

- [ ] **Step 2: Register `analysis` module in `main.rs`**

Update `lsp-server/src/main.rs` (adds `mod analysis;` to the existing module declarations):

```rust
mod analysis;
mod config;
mod format;
mod profile;

fn main() {
    println!("go-profile-lsp placeholder");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test` (in `lsp-server/`)
Expected: All tests across all modules pass.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/src/
git commit -m "feat: cost aggregation and analysis with profile type detection and hotspot ranking"
```

---

## Chunk 2: Path Resolution + LSP Server Core

### Task 6: File Path Resolution (`paths.rs`)

**Files:**
- Create: `lsp-server/src/paths.rs`

**Dependencies:** Task 3 (`config.rs` for `PathMappingConfig`)

Resolution strategy from spec (tried in order):
1. Exact match — profile path exists in workspace.
2. Configured trim — strip `trimPrefix`, check remainder.
3. Suffix match — workspace file is a suffix of profile path.
4. Configured source root — prepend `sourceRoot` to trimmed path.

- [ ] **Step 1: Create `lsp-server/src/paths.rs`**

```rust
use crate::config::PathMappingConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolves profile file paths to workspace-relative paths.
pub struct PathResolver {
    workspace_root: PathBuf,
    config: PathMappingConfig,
    /// Cache: profile path -> resolved workspace-relative path (or None if unresolvable).
    cache: HashMap<String, Option<String>>,
    /// Cached list of Go files in workspace (relative paths).
    go_files: Vec<String>,
}

impl PathResolver {
    pub fn new(workspace_root: PathBuf, config: PathMappingConfig) -> Self {
        let go_files = scan_go_files(&workspace_root);
        Self {
            workspace_root,
            config,
            cache: HashMap::new(),
            go_files,
        }
    }

    /// Clear cache and rescan workspace files.
    /// Call this when profile data is reloaded.
    pub fn invalidate(&mut self) {
        self.cache.clear();
        self.go_files = scan_go_files(&self.workspace_root);
    }

    /// Resolve a profile path to a workspace-relative path.
    /// Returns None if the file cannot be found in the workspace.
    pub fn resolve(&mut self, profile_path: &str) -> Option<String> {
        if let Some(cached) = self.cache.get(profile_path) {
            return cached.clone();
        }

        let result = self.resolve_uncached(profile_path);
        self.cache.insert(profile_path.to_string(), result.clone());
        result
    }

    fn resolve_uncached(&self, profile_path: &str) -> Option<String> {
        // Strategy 1: Exact match.
        if self.workspace_root.join(profile_path).exists() {
            return Some(profile_path.to_string());
        }

        // Strategy 2: Configured trim prefix.
        if !self.config.trim_prefix.is_empty() {
            if let Some(trimmed) = profile_path.strip_prefix(&self.config.trim_prefix) {
                let trimmed = trimmed.trim_start_matches('/');
                if self.workspace_root.join(trimmed).exists() {
                    return Some(trimmed.to_string());
                }

                // Strategy 4: Source root prepend (after trimming).
                if !self.config.source_root.is_empty() {
                    let with_root = Path::new(&self.config.source_root).join(trimmed);
                    if let Some(s) = with_root.to_str() {
                        if self.workspace_root.join(s).exists() {
                            return Some(s.to_string());
                        }
                    }
                }
            }
        }

        // Strategy 3: Suffix match against workspace Go files.
        for go_file in &self.go_files {
            if profile_path.ends_with(go_file.as_str())
                || profile_path.ends_with(&format!("/{go_file}"))
            {
                return Some(go_file.clone());
            }
        }

        None
    }
}

/// Scan workspace for .go files and return relative paths.
fn scan_go_files(workspace_root: &Path) -> Vec<String> {
    let pattern = workspace_root.join("**/*.go");
    let pattern_str = pattern.to_string_lossy();

    let mut files = Vec::new();
    if let Ok(paths) = glob::glob(&pattern_str) {
        for entry in paths.flatten() {
            if let Ok(relative) = entry.strip_prefix(workspace_root) {
                if let Some(s) = relative.to_str() {
                    files.push(s.to_string());
                }
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temp workspace with some Go files for testing.
    fn setup_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create directory structure.
        fs::create_dir_all(root.join("pkg/handler")).unwrap();
        fs::create_dir_all(root.join("cmd/server")).unwrap();

        // Create Go files.
        fs::write(root.join("main.go"), "package main").unwrap();
        fs::write(root.join("pkg/handler/handler.go"), "package handler").unwrap();
        fs::write(root.join("cmd/server/server.go"), "package main").unwrap();

        dir
    }

    #[test]
    fn test_exact_match() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        assert_eq!(resolver.resolve("main.go"), Some("main.go".to_string()));
        assert_eq!(
            resolver.resolve("pkg/handler/handler.go"),
            Some("pkg/handler/handler.go".to_string())
        );
    }

    #[test]
    fn test_trim_prefix() {
        let dir = setup_workspace();
        let config = PathMappingConfig {
            trim_prefix: "/home/ci/go/src/github.com/user/project/".to_string(),
            source_root: String::new(),
        };
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        assert_eq!(
            resolver.resolve("/home/ci/go/src/github.com/user/project/pkg/handler/handler.go"),
            Some("pkg/handler/handler.go".to_string())
        );
    }

    #[test]
    fn test_suffix_match() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        // Profile path has extra prefix, but ends with workspace-relative path.
        assert_eq!(
            resolver.resolve("github.com/user/project/pkg/handler/handler.go"),
            Some("pkg/handler/handler.go".to_string())
        );
    }

    #[test]
    fn test_no_match() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        assert_eq!(resolver.resolve("nonexistent.go"), None);
    }

    #[test]
    fn test_caching() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        // First resolve — populates cache.
        let result1 = resolver.resolve("main.go");
        // Second resolve — hits cache.
        let result2 = resolver.resolve("main.go");
        assert_eq!(result1, result2);
        assert_eq!(result1, Some("main.go".to_string()));
    }

    #[test]
    fn test_invalidate() {
        let dir = setup_workspace();
        let config = PathMappingConfig::default();
        let mut resolver = PathResolver::new(dir.path().to_path_buf(), config);

        // Populate cache.
        resolver.resolve("main.go");
        assert!(!resolver.cache.is_empty());

        // Invalidate.
        resolver.invalidate();
        assert!(resolver.cache.is_empty());
    }
}
```

Note: `tempfile` is already in `Cargo.toml` dev-dependencies; `glob` is already in dependencies.

- [ ] **Step 2: Register module**

Add `mod paths;` to `lsp-server/src/main.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test --lib paths` (in `lsp-server/`)
Expected: All 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/
git commit -m "feat: file path resolution with exact, trim, suffix, and source root strategies"
```

---

### Task 7: Inlay Hint Generation (`hints.rs`)

**Files:**
- Create: `lsp-server/src/hints.rs`

**Dependencies:** Task 4 (`format.rs`), Task 5 (`analysis.rs`), Task 3 (`config.rs`)

This module converts `ProfileData` line costs into `lsp_types::InlayHint` structs for a given file and range.

- [ ] **Step 1: Create `lsp-server/src/hints.rs`**

```rust
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
                character: u32::MAX,         // End of line
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
        file_costs.insert(10, LineCost { flat: 50_000_000, cumulative: 340_000_000 });
        file_costs.insert(15, LineCost { flat: 15_000_000, cumulative: 200_000_000 });
        file_costs.insert(20, LineCost { flat: 100_000, cumulative: 500_000 }); // Below threshold

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
            start: Position { line: 0, character: 0 },
            end: Position { line: 30, character: 0 },
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
            start: Position { line: 0, character: 0 },
            end: Position { line: 12, character: 0 },
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
            start: Position { line: 0, character: 0 },
            end: Position { line: 100, character: 0 },
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
            start: Position { line: 0, character: 0 },
            end: Position { line: 30, character: 0 },
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
            start: Position { line: 0, character: 0 },
            end: Position { line: 30, character: 0 },
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
            start: Position { line: 0, character: 0 },
            end: Position { line: 30, character: 0 },
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
```

- [ ] **Step 2: Register module**

Add `mod hints;` to `lsp-server/src/main.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test --lib hints` (in `lsp-server/`)
Expected: All 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/src/hints.rs lsp-server/src/main.rs
git commit -m "feat: inlay hint generation with threshold filtering and configurable display"
```

---

### Task 8: Code Lens Generation (`lenses.rs`)

**Files:**
- Create: `lsp-server/src/lenses.rs`

**Dependencies:** Task 5 (`analysis.rs`), Task 4 (`format.rs`), Task 3 (`config.rs`)

- [ ] **Step 1: Create `lsp-server/src/lenses.rs`**

```rust
use crate::analysis::ProfileData;
use crate::config::Config;
use crate::format::{format_percent, format_value};
use tower_lsp::lsp_types::*;

/// Generate code lenses for a specific file.
///
/// `file_key` is the workspace-relative path as stored in `ProfileData`.
/// Returns code lenses for hotspot functions that appear in this file.
pub fn generate_code_lenses(
    data: &ProfileData,
    config: &Config,
    file_key: &str,
) -> Vec<CodeLens> {
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
        file_costs.insert(10, LineCost { flat: 50_000_000, cumulative: 340_000_000 });
        file_costs.insert(25, LineCost { flat: 100_000_000, cumulative: 700_000_000 });

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
        let handle_lens = lenses.iter().find(|l| {
            l.command.as_ref().unwrap().title.contains("#1 hotspot")
        }).unwrap();
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
        assert_eq!(title, "# #1 hotspot — flat: 100ms (10.0%) | cum: 700ms (70.0%)");
    }
}
```

- [ ] **Step 2: Register module**

Add `mod lenses;` to `lsp-server/src/main.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test --lib lenses` (in `lsp-server/`)
Expected: All 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/src/lenses.rs lsp-server/src/main.rs
git commit -m "feat: code lens generation for hotspot function summaries"
```

---

### Task 9: LSP Server Core (`server.rs` + `main.rs`)

**Files:**
- Create: `lsp-server/src/server.rs`
- Modify: `lsp-server/src/main.rs`

**Dependencies:** All previous tasks (this wires everything together)

This is the core LSP server implementation. The Backend struct holds the shared state (config, profile data) behind an `RwLock`, and the `LanguageServer` trait impl dispatches to the `hints.rs` and `lenses.rs` modules. Path resolution is NOT done here — it is added later in Task 11.

- [ ] **Step 1: Create `lsp-server/src/server.rs`**

```rust
use crate::analysis::{self, ProfileData};
use crate::config::Config;
use crate::hints;
use crate::lenses;
use crate::profile;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

/// Discover profile files in the workspace based on configuration.
fn discover_profile_files(workspace_root: &Path, config: &Config) -> Vec<PathBuf> {
    let mut files = Vec::new();

    for search_path in &config.profile_paths {
        let dir = workspace_root.join(search_path);
        if !dir.is_dir() {
            continue;
        }

        let pattern = dir.join(&config.profile_glob);
        let pattern_str = pattern.to_string_lossy();

        if let Ok(paths) = glob::glob(&pattern_str) {
            for entry in paths.flatten() {
                if entry.is_file() {
                    files.push(entry);
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
```

- [ ] **Step 2: Update `lsp-server/src/main.rs` with full entrypoint**

```rust
mod analysis;
mod config;
mod format;
mod hints;
mod lenses;
mod paths;
mod profile;
mod server;

use server::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    // Initialize tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("go_profile_lsp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
```

- [ ] **Step 3: Run all tests and verify compilation**

Run: `cargo test` (in `lsp-server/`)
Expected: All tests pass. Binary compiles.

Run: `cargo build` (in `lsp-server/`)
Expected: Successful compilation producing `target/debug/go-profile-lsp`.

- [ ] **Step 4: Commit**

```bash
git add lsp-server/src/
git commit -m "feat: LSP server core with LanguageServer trait impl, profile loading, and file discovery"
```

---

## Chunk 3: Profile Watching + Extension + Integration Testing

### Task 10: Profile File Watcher (`watch.rs`)

**Files:**
- Create: `lsp-server/src/watch.rs`
- Modify: `lsp-server/src/server.rs` (start watcher after initialized)

**Dependencies:** Task 9 (server.rs Backend)

The watcher polls profile directories at a configurable interval, detects mtime changes, and triggers profile reload + client refresh.

- [ ] **Step 1: Create `lsp-server/src/watch.rs`**

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Tracks file modification times to detect changes.
pub struct FileWatcher {
    /// Last known mtime for each tracked file.
    mtimes: HashMap<PathBuf, SystemTime>,
}

impl FileWatcher {
    pub fn new() -> Self {
        Self {
            mtimes: HashMap::new(),
        }
    }

    /// Check a list of files for changes since last check.
    /// Returns true if any file was added, removed, or modified.
    pub fn check_for_changes(&mut self, current_files: &[PathBuf]) -> bool {
        let mut changed = false;

        let mut new_mtimes = HashMap::new();

        for file in current_files {
            let mtime = file
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            if let Some(old_mtime) = self.mtimes.get(file) {
                if *old_mtime != mtime {
                    changed = true;
                }
            } else {
                // New file appeared.
                changed = true;
            }

            new_mtimes.insert(file.clone(), mtime);
        }

        // Check if any files were removed.
        if self.mtimes.len() != new_mtimes.len() {
            changed = true;
        }

        self.mtimes = new_mtimes;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_initial_check_detects_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        // First check: all files are new.
        assert!(watcher.check_for_changes(&[file]));
    }

    #[test]
    fn test_no_change_on_second_check() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file.clone()]);
        // Second check: no changes.
        assert!(!watcher.check_for_changes(&[file]));
    }

    #[test]
    fn test_detects_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("cpu.pprof");
        let file2 = dir.path().join("heap.pprof");
        fs::write(&file1, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file1.clone()]);

        // Add a new file.
        fs::write(&file2, b"data").unwrap();
        assert!(watcher.check_for_changes(&[file1, file2]));
    }

    #[test]
    fn test_detects_removed_file() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("cpu.pprof");
        let file2 = dir.path().join("heap.pprof");
        fs::write(&file1, b"data").unwrap();
        fs::write(&file2, b"data").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file1.clone(), file2.clone()]);

        // Remove file2.
        fs::remove_file(&file2).unwrap();
        assert!(watcher.check_for_changes(&[file1]));
    }

    #[test]
    fn test_detects_modified_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cpu.pprof");
        fs::write(&file, b"data1").unwrap();

        let mut watcher = FileWatcher::new();
        watcher.check_for_changes(&[file.clone()]);

        // Modify the file (need to ensure mtime changes).
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&file, b"data2_longer").unwrap();

        assert!(watcher.check_for_changes(&[file]));
    }

    #[test]
    fn test_empty_files_list() {
        let mut watcher = FileWatcher::new();
        // First check with empty list: no change (nothing to detect).
        assert!(!watcher.check_for_changes(&[]));
    }
}
```

- [ ] **Step 2: Integrate watcher into server lifecycle**

Add to `lsp-server/src/server.rs` — update the `initialized` method and add `mod watch;` to `main.rs`:

In `server.rs`, update the `initialized` method:

```rust
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
```

Note: Add `use std::time::Duration;` to the imports in `server.rs`.

- [ ] **Step 3: Register module**

Add `mod watch;` to `lsp-server/src/main.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test` (in `lsp-server/`)
Expected: All tests pass (including new watch tests).

Run: `cargo build` (in `lsp-server/`)
Expected: Successful compilation.

- [ ] **Step 5: Commit**

```bash
git add lsp-server/
git commit -m "feat: profile file watcher with mtime-based change detection"
```

---

### Task 11: Path Resolution Integration

**Files:**
- Modify: `lsp-server/src/server.rs`
- Modify: `lsp-server/src/analysis.rs`

**Dependencies:** Task 6 (`paths.rs`), Task 9 (`server.rs`)

Currently the server stores profile paths as-is from the protobuf (e.g., `/home/ci/go/src/...`). We need to resolve them to workspace-relative paths at profile load time so that inlay hint and code lens lookups match the URIs sent by the editor.

Note: Task 9 intentionally omits a `path_resolver` field from `ServerState` — resolution happens inline in `load_profiles` and doesn't need to persist. The `PathResolver` is created fresh each time profiles are loaded.

- [ ] **Step 1: Add path resolution to `load_profiles` in `server.rs`**

After `analyze_profile` returns `ProfileData`, iterate through `line_costs` and `hotspots`, resolving each filename through the `PathResolver`. Replace the original keys with resolved workspace-relative paths. Keys that can't be resolved are dropped.

Modify `load_profiles` in `server.rs` — after the `analyze_profile` call, add:

```rust
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
```

Also add `use std::collections::BTreeMap;` and `use crate::paths::PathResolver;` to imports in `server.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test` (in `lsp-server/`)
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add lsp-server/src/server.rs
git commit -m "feat: resolve profile paths to workspace-relative paths at load time"
```

---

### Task 12: Zed Extension (WASM Layer)

**Files:**
- Create: `extension/extension.toml`
- Create: `extension/Cargo.toml`
- Create: `extension/src/lib.rs`
- Create: `extension/languages/go/config.toml`

**Dependencies:** None (independent of LSP server code, follows spec exactly)

The extension is ~50 lines of boilerplate. It downloads the LSP binary and passes configuration. Since this compiles to WASM and depends on `zed_extension_api`, it cannot be tested with standard `cargo test` (WASM target required). We verify it compiles only.

- [ ] **Step 1: Create `extension/extension.toml`**

```toml
id = "go-profile"
name = "Go Profile"
version = "0.0.1"
schema_version = 1
authors = ["Author"]
description = "Inline Go pprof profiling annotations via LSP inlay hints and code lenses"
repository = "https://github.com/user/zed-go-profile"

[language_servers.go-profile-lsp]
name = "Go Profile LSP"
languages = ["Go"]
```

- [ ] **Step 2: Create `extension/Cargo.toml`**

```toml
[package]
name = "go-profile-extension"
version = "0.0.1"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
zed_extension_api = "0.7.0"
```

- [ ] **Step 3: Create `extension/src/lib.rs`**

```rust
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
        let settings =
            zed::settings::LspSettings::for_worktree("go-profile-lsp", worktree)?;
        Ok(settings.initialization_options)
    }
}

zed::register_extension!(GoProfileExtension);
```

- [ ] **Step 4: Create `extension/languages/go/config.toml`**

```toml
name = "Go"
path_suffixes = ["go"]
```

- [ ] **Step 5: Verify LSP server compiles**

We cannot compile the extension without the WASM target and `zed_extension_api` WASM setup. Instead, verify the LSP server still compiles cleanly:

Run: `cargo build` (in `lsp-server/`)
Expected: Successful compilation.

Note: To compile the extension, you need `rustup target add wasm32-wasi` and the Zed extension build toolchain. This is a distribution concern, not a development concern for the LSP server itself.

- [ ] **Step 6: Commit**

```bash
git add extension/
git commit -m "feat: Zed WASM extension with binary download and configuration passthrough"
```

---

### Task 13: Integration Test with Real Profile Data

**Files:**
- Modify: `lsp-server/src/server.rs` (add `#[cfg(test)] mod integration_tests`)

**Dependencies:** All previous tasks

This test creates a synthetic pprof file, sets up a workspace directory, and verifies the full pipeline: parse -> analyze -> resolve paths -> generate hints + lenses.

- [ ] **Step 1: Create test fixture generator and integration test**

Add to the bottom of `lsp-server/src/server.rs`:

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;
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
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test integration_tests` (in `lsp-server/`)
Expected: Test passes, confirming the full parse -> analyze -> resolve -> hint/lens pipeline.

- [ ] **Step 3: Commit**

```bash
git add lsp-server/src/server.rs
git commit -m "test: end-to-end integration test for full profile-to-LSP pipeline"
```

---

### Task 14: Workspace `Cargo.toml` and Final Polish

**Files:**
- Create: `Cargo.toml` (workspace root, optional)
- Verify all modules are registered

**Dependencies:** All previous tasks

- [ ] **Step 1: Create root `Cargo.toml` workspace file (optional)**

```toml
[workspace]
members = ["lsp-server"]
# Note: extension/ is not included because it targets wasm32-wasi
# and has different compilation requirements.
```

- [ ] **Step 2: Run full test suite from workspace root**

Run: `cargo test` (from workspace root)
Expected: All tests pass.

Run: `cargo clippy` (from workspace root)
Expected: No warnings (or only minor ones to address).

- [ ] **Step 3: Fix any clippy warnings**

Address any clippy suggestions. Common ones might include:
- Unnecessary clones
- Missing `#[must_use]` attributes
- Simplifiable match patterns

- [ ] **Step 4: Final commit**

```bash
git add .
git commit -m "chore: workspace setup and clippy fixes"
```
