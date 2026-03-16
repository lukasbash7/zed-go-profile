# Go Profile Zed Extension — Design Specification

## Overview

A Zed editor extension that displays Go pprof profiling data inline alongside source code. The extension parses binary pprof files (gzip-compressed protobuf) and surfaces per-line cost information through LSP inlay hints and function-level hotspot summaries through LSP code lenses.

**Target users:** Go developers who profile their applications and want to see where time/memory is spent without leaving the editor.

**Core value proposition:** Open a `.pprof` file in your workspace, and every Go source file lights up with profiling costs — no context-switching to external tools.

## Constraints

### Zed Extension API Limitations

Zed extensions are Rust compiled to WASM. The API (v0.7.0) provides:
- LSP server management (launch, configure, pass init options)
- File downloads, process execution, HTTP requests
- Worktree file reading, settings access

The API does **not** provide:
- Custom side panels or UI
- Inline decorations or editor buffer manipulation
- Programmatic file navigation (go-to-file:line)
- Custom command palette entries

All visual features must flow through standard LSP protocol features that Zed supports: inlay hints, code lenses, diagnostics, and semantic tokens.

### Verified LSP Feature Support in Zed

| Feature | Supported | Used For |
|---|---|---|
| Inlay Hints | Yes | Per-line cost annotations |
| Code Lenses | Yes | Function-level hotspot summaries |
| Diagnostics | Yes (not used v1) | Reserved for future use |
| Semantic Tokens | Yes (not used v1) | Reserved for heat-map coloring |

## Architecture

### Two-Component Monorepo

```
zed-go-profile/
  extension/          # Thin Zed WASM extension (~50 lines Rust)
    extension.toml
    Cargo.toml
    src/lib.rs
    languages/go/config.toml
  lsp-server/         # Standalone Rust binary (LSP server)
    Cargo.toml
    src/
      main.rs         # LSP lifecycle, JSON-RPC transport
      profile.rs      # Pprof protobuf parsing
      analysis.rs     # Cost aggregation, hotspot ranking
      hints.rs        # Inlay hint generation
      lenses.rs       # Code lens generation
      paths.rs        # File path resolution/matching
```

### Component Responsibilities

**Extension (WASM):**
- Implements `zed_extension_api::Extension` trait
- Downloads the platform-specific LSP binary from GitHub Releases on first activation
- Returns the binary path from `language_server_command()`
- Passes user configuration via `language_server_initialization_options()`
- Total code: ~50 lines of Rust

**LSP Server (native binary):**
- Standalone Rust binary communicating via stdin/stdout JSON-RPC
- Parses pprof protobuf files directly (no Go dependency)
- Serves inlay hints and code lenses to Zed
- Watches profile files for changes, triggers refresh
- All analysis logic lives here

### Data Flow

```
.pprof file on disk
    |
    v
LSP Server starts (launched by Zed via extension)
    |
    v
Scan workspace for .pprof/.prof files
    |
    v
Parse: gzip decompress -> protobuf decode -> resolve string table
    |
    v
Analyze: walk samples -> accumulate per-line flat/cumulative costs
    |           |
    v           v
Build line_costs index    Build hotspots list (top-N functions)
    |                         |
    v                         v
textDocument/inlayHint    textDocument/codeLens
(per-line costs)          (function summaries)
    |                         |
    v                         v
Zed renders ghost text    Zed renders text above function defs
```

## Profile Parsing & Analysis

### Parse Pipeline

1. **Detect format:** Check first two bytes for gzip magic (`0x1f 0x8b`). If present, decompress with `flate2`. If not, attempt raw protobuf decode.
2. **Decode protobuf:** Use `prost`-generated structs from the pprof `profile.proto` schema.
3. **Resolve string table:** The protobuf stores all strings in a flat `string_table` array. Entity fields reference strings by index. After decode, resolve all string references to owned `String` values.
4. **Build cost index:** Walk every sample, accumulate costs keyed by `(filename, line_number)`.

### Core Data Structures

```rust
/// Aggregated profiling data ready for LSP consumption.
struct ProfileData {
    /// Per-file, per-line costs. Outer key: resolved file path.
    line_costs: HashMap<String, BTreeMap<u64, LineCost>>,
    /// Top functions ranked by cost.
    hotspots: Vec<HotspotFunction>,
    /// What kind of profile (cpu, heap, block, mutex, goroutine).
    profile_type: ProfileType,
    /// Human-readable description of the value being displayed (e.g., "cpu time").
    sample_type_label: String,
    /// Unit for formatting (nanoseconds, bytes, count).
    value_unit: ValueUnit,
    /// Total value across all samples (for percentage calculation).
    total_value: i64,
    /// Profile collection duration, if available.
    duration: Option<Duration>,
}

struct LineCost {
    /// Cost directly at this line (line was leaf of call stack).
    flat: i64,
    /// Cost at this line including all callees.
    cumulative: i64,
}

struct HotspotFunction {
    rank: usize,
    name: String,
    filename: String,
    start_line: u64,
    flat: i64,
    cumulative: i64,
}
```

### Cost Aggregation Algorithm

```
for each sample in profile.samples:
    value = sample.value[selected_index]
    for i, location in sample.locations:       // index 0 = leaf
        for j, line in location.lines:          // index 0 = innermost inlined
            file = resolve_path(line.function.filename)
            line_no = line.line

            costs[file][line_no].cumulative += value

            if i == 0 and j == 0:               // leaf location, innermost line
                costs[file][line_no].flat += value
```

**Inlined function handling:** A single `Location` can contain multiple `Line` entries when functions are inlined. `line[0]` is the innermost (inlined) function; `line[last]` is the outer caller. Each line gets cumulative credit. Only the innermost line at the leaf location gets flat credit.

### Value Column Selection

The server auto-selects which value column to display based on profile type:

| Profile Type | Selected Column | Unit |
|---|---|---|
| CPU | `cpu/nanoseconds` (index 1) | nanoseconds |
| Heap (default) | `inuse_space/bytes` (index 3) | bytes |
| Allocs | `alloc_space/bytes` (index 1) | bytes |
| Block | `delay/nanoseconds` (index 1) | nanoseconds |
| Mutex | `delay/nanoseconds` (index 1) | nanoseconds |
| Goroutine | `goroutine/count` (index 0) | count |

Profile type is detected from `sample_type` labels. Falls back to index 0 if unrecognized.

### Value Formatting

- **Nanoseconds:** `1.2s`, `340ms`, `15us`, `800ns` (use largest unit where value >= 1)
- **Bytes:** `1.5GB`, `230MB`, `12KB`, `512B`
- **Counts:** `1,234,567` (comma-separated thousands)
- **Percentages:** `(34.2%)` relative to `total_value`

### File Path Resolution

Profile file paths (from `Function.filename`) may not match workspace paths directly. Resolution strategy:

1. **Exact match:** Check if the profile path exists in the workspace as-is.
2. **Configured trim:** Apply `trimPrefix` from initialization options (e.g., strip `/home/ci/go/src/`).
3. **Suffix match:** If profile path ends with a workspace-relative path, use it. For example, profile path `github.com/user/project/pkg/handler.go` matches workspace file `pkg/handler.go`.
4. **Configured source root:** Prepend `sourceRoot` to the stripped path.

Cache resolved paths for the lifetime of the profile data to avoid repeated filesystem lookups.

## LSP Protocol & Features

### Inlay Hints (`textDocument/inlayHint`)

**Purpose:** Show per-line profiling cost as subtle ghost text at the end of each code line.

**Request/Response:**
- Zed sends `textDocument/inlayHint` with a document URI and visible range.
- Server looks up the file in `line_costs`, returns hints for lines within the range that exceed the threshold.

**Hint format:**

```
Position: end of line (column = max)
Kind: Other
Label: "  flat: 15ms | cum: 340ms"
Tooltip: "Function: runtime.mallocgc\nFlat: 15ms (2.4%)\nCumulative: 340ms (55.1%)\nSamples: 34\nProfile: cpu.prof"
```

- Leading two spaces provide visual separation from code.
- If `showFlat` is false (configurable), omit the flat portion: `  cum: 340ms`
- If `showCumulative` is false, omit the cumulative portion: `  flat: 15ms`

**Filtering/Thresholds:**
- Default: show hints for lines whose cumulative cost is in the top 80% of total cost, OR whose flat cost exceeds a minimum (1ms for CPU, 1KB for heap).
- Configurable via `threshold.minPercent` (default: 0.1%) and `threshold.minFlat`.
- This prevents visual noise from low-cost lines while ensuring all significant costs are visible.

### Code Lenses (`textDocument/codeLens`)

**Purpose:** Show function-level hotspot summaries above function definitions.

**Request/Response:**
- Zed sends `textDocument/codeLens` with a document URI.
- Server identifies functions in the file that appear in the top-N hotspots list.
- Returns a code lens positioned at the function's `start_line`.

**Lens format:**

```
Range: line = function start_line, character 0
Command: { title: "# #1 hotspot -- flat: 2.1s (34%) | cum: 4.3s (70%)", command: "" }
```

- `command` is empty string — display-only, no action on click.
- The `#` prefix and ranking number provide quick visual scanning.
- Only functions present in the currently open file are shown.

**Configuration:**
- `maxCodeLenses`: maximum number of lenses per file (default: 10).
- Only functions in the global top-N hotspots are shown (default N=50, configurable).

### Initialization Options

Passed from the Zed extension via `language_server_initialization_options()`:

```json
{
  "profilePaths": [".", "./profiles", "./pprof"],
  "profileGlob": "*.{pprof,prof}",
  "threshold": {
    "minPercent": 0.1,
    "minFlat": null
  },
  "display": {
    "showFlat": true,
    "showCumulative": true,
    "maxCodeLenses": 10,
    "maxHotspots": 50
  },
  "pathMapping": {
    "trimPrefix": "",
    "sourceRoot": ""
  },
  "watchIntervalSecs": 5
}
```

All fields optional with sensible defaults.

### Profile Reloading

- The LSP server polls configured directories at a configurable interval (default 5 seconds) for new or modified `.pprof`/`.prof` files.
- On detecting a change: re-parse, rebuild cost index, send `workspace/inlayHint/refresh` and `workspace/codeLens/refresh` requests to the client.
- File modification is detected by comparing `mtime` or file hash.

## Zed Extension (WASM Layer)

### `extension.toml`

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

### `Cargo.toml`

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

### `src/lib.rs` (approximately)

```rust
use zed_extension_api as zed;

struct GoProfileExtension {}

impl zed::Extension for GoProfileExtension {
    fn new() -> Self {
        GoProfileExtension {}
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        // Download binary if not cached, return path
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
        _worktree: &zed::Worktree,
    ) -> zed::Result<Option<zed::serde_json::Value>> {
        // Read from Zed LSP settings, pass through
        let settings = zed::settings::LspSettings::for_worktree("go-profile-lsp", _worktree)?;
        Ok(settings.initialization_options)
    }
}

zed::register_extension!(GoProfileExtension);
```

### `languages/go/config.toml`

```toml
name = "Go"
path_suffixes = ["go"]
```

This piggybacks on the existing Go language definition, allowing the LSP to attach to Go files as an additional language server.

### Binary Distribution

- CI builds the LSP server binary for 4 targets:
  - `aarch64-apple-darwin` (macOS ARM)
  - `x86_64-apple-darwin` (macOS Intel)
  - `x86_64-unknown-linux-gnu` (Linux)
  - `x86_64-pc-windows-msvc` (Windows)
- Binaries published as GitHub Release assets.
- Extension downloads the correct binary on first activation using `zed_extension_api::download_file()`.
- Binary cached in the extension's work directory; re-downloaded on version mismatch.

### Required Capabilities

The extension requires these Zed capabilities (user must grant):
- `process:exec` — to launch the LSP server binary
- `download_file` — to download the binary from GitHub

## LSP Server Dependencies (Rust Crates)

| Crate | Purpose |
|---|---|
| `tower-lsp` | LSP protocol implementation, JSON-RPC transport |
| `prost` + `prost-build` | Protobuf decoding from `profile.proto` |
| `flate2` | Gzip decompression |
| `tokio` | Async runtime (required by tower-lsp) |
| `serde` + `serde_json` | Configuration deserialization |
| `glob` | File pattern matching for profile discovery |
| `tracing` | Structured logging |

## What Is NOT in v1

These are explicitly deferred to future versions:

- **Flame graph visualization** — no webview API in Zed
- **Interactive pprof navigation** — no custom UI panels
- **Live profiling** (attach to running process) — complexity, security concerns
- **Profile diff mode** (compare two profiles) — v2 feature
- **Semantic token heat-map coloring** — v2 feature (color code lines by cost intensity)
- **Multi-profile merging** — v2 feature
- **Go-to-definition for hotspot** — Zed LSP doesn't support code lens commands that navigate

## Success Criteria

1. Drop a `.pprof` file in a Go project workspace, open a Go file — see inline cost annotations on profiled lines.
2. Hot functions display code lenses with rank and cost above their definition.
3. Annotations update automatically when the profile file changes.
4. No visible delay in editor responsiveness (parsing happens at server startup, not on every keystroke).
5. Works with CPU, heap, block, and mutex profiles.
6. Path resolution correctly maps profile paths to workspace files for typical Go project layouts.
