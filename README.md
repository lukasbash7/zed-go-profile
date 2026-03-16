# Go Profile for Zed

A [Zed](https://zed.dev) extension that surfaces Go pprof profiling data directly in your editor. See per-line costs as inlay hints, function-level hotspot summaries as code lenses, and jump to expensive code via the diagnostics panel — all without leaving your editor.

## Features

### Inlay Hints — Per-Line Cost Annotations

Every line with profiling data gets an inlay hint at the end of the line showing flat and cumulative cost, with a color-coded severity indicator:

```
func processRows(ctx context.Context, rows []Row) {  🟡 cum: 830ms (8.6%)
    for _, row := range rows {                        🔴 cum: 4.5s (46.8%)
        row.Transform()
    }
}
```

Severity thresholds:
| Indicator | Cumulative % |
|-----------|-------------|
| 🟢 / `░`  | < 5%        |
| 🟡 / `▒`  | 5% – 15%    |
| 🟠 / `▓`  | 15% – 30%   |
| 🔴 / `█`  | >= 30%      |

### Code Lenses — Function Hotspot Summaries

Functions that appear in the profile's top hotspots get a code lens above them:

```
# #13 hotspot — flat: 0ns (0.0%) | cum: 4.5s (46.8%)
func processRows(ctx context.Context, rows []Row) {
```

### Diagnostics — Jump to Hot Lines

Optionally publish profiling data as diagnostics so they appear in Zed's Diagnostics panel (`cmd+shift+m`), making it easy to jump between all expensive lines across your project.

### Automatic Profile Reloading

The server watches profile files for changes. Re-run your benchmark and the annotations update automatically — no restart needed.

## Installation

### From Zed Extensions (once published)

Open Zed's extension browser (`zed: extensions`) and search for "Go Profile".

### Development / Local Build

1. Build the LSP server:

```sh
cd lsp-server
cargo build --release
```

2. Build the extension:

```sh
cd extension
cargo build --release --target wasm32-wasip1
```

3. Install the extension as a dev extension in Zed (`zed: install dev extension`, point to the `extension/` directory).

4. Point the extension at your local binary in Zed settings (see below).

## Configuration

All configuration goes in your Zed `settings.json` under `lsp.go-profile-lsp.settings`:

```jsonc
{
  "lsp": {
    "go-profile-lsp": {
      "settings": {
        // Directories to search for profile files (relative to workspace root).
        // Default: ["."]
        "profilePaths": [".", "./bench"],

        // Glob pattern for profile files.
        // Default: "*.{pprof,prof}"
        "profileGlob": "*.pprof",

        // Filtering thresholds
        "threshold": {
          // Minimum cumulative % to show a hint. Default: 0.1
          "minPercent": 0.5,
          // Minimum flat value (in profile units, e.g. nanoseconds). Default: null (disabled)
          "minFlat": 1000000
        },

        // Display options
        "display": {
          "showFlat": true,         // Show flat cost in hints. Default: true
          "showCumulative": true,   // Show cumulative cost in hints. Default: true
          "maxCodeLenses": 10,      // Max code lenses per file. Default: 10
          "maxHotspots": 50,        // Max hotspot functions tracked globally. Default: 50
          "hintStyle": "emoji"      // "emoji" (🟢🟡🟠🔴) or "ascii" (░▒▓█). Default: "emoji"
        },

        // Path mapping (for profiles generated in CI/Docker with different paths)
        "pathMapping": {
          "trimPrefix": "/home/ci/go/src/", // Strip this prefix from profile paths
          "sourceRoot": ""                   // Prepend this after trimming
        },

        // Diagnostics (publish profile data to the Diagnostics panel)
        "diagnostics": {
          // "warning" = visible in Diagnostics panel
          // "info"    = visible on hover / inline only
          // "off"     = disabled (default)
          "severity": "off",
          // Minimum cumulative % to publish a diagnostic. Default: 1.0
          "minPercent": 1.0
        },

        // Profile file poll interval in seconds. Default: 30
        "watchIntervalSecs": 30
      }
    }
  }
}
```

### Using a local binary (development)

To use a locally built LSP binary instead of the one bundled with the extension:

```jsonc
{
  "lsp": {
    "go-profile-lsp": {
      "settings": {
        "binary": {
          "path": "/absolute/path/to/go-profile-lsp"
        }
      }
    }
  }
}
```

### Enabling inlay hints

Zed has inlay hints disabled by default. You need to enable them:

```jsonc
{
  // Globally:
  "inlay_hints": {
    "enabled": true
  }

  // Or per-language:
  "languages": {
    "Go": {
      "inlay_hints": {
        "enabled": true
      }
    }
  }
}
```

## How It Works

The extension has two components:

1. **Zed Extension** (`extension/`) — A thin WASM extension that registers the LSP server for Go files and forwards configuration as LSP initialization options.

2. **LSP Server** (`lsp-server/`) — A standalone Rust binary that:
   - Parses pprof protobuf profiles on startup
   - Resolves profile paths to workspace files using prefix trimming, source root mapping, and suffix matching
   - Serves inlay hints (per-line costs), code lenses (function hotspots), and diagnostics via the LSP protocol
   - Watches profile files for changes and automatically reloads

### Profile Path Resolution

Profiles often contain absolute paths from a build environment (CI, Docker) that don't match your local workspace. The server resolves paths using these strategies, in order:

1. **Direct match** — Profile path exists relative to workspace root
2. **Prefix trim** — Strip `trimPrefix`, check if result exists in workspace
3. **Source root** — After trimming, prepend `sourceRoot` and check
4. **Suffix match** — Match profile path suffix against workspace Go files (component-level, e.g. `pkg/foo.go` matches but `notfoo.go` does not)

### Supported Profile Types

Any Go pprof protobuf profile (`.pprof` / `.prof`):
- CPU profiles (`go test -cpuprofile`)
- Memory profiles (`go test -memprofile`)
- Block / mutex profiles
- Custom profiles

The value unit (time, bytes, count) is auto-detected from the profile metadata.

## License

MIT
