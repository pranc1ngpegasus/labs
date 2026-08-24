---
title: 24 — CLI `koe record` Command
status: draft
depends: [15-pipeline-core, 17-audio-encoder-trait-and-ogg, 20-transcript-formatter, 21-echo-cancellation]
spec_refs: [08-cli-interface]
---

# 24 — CLI `koe record` Command

Implement the primary recording command.

## Location

`koe-cli/src/commands/record.rs`

## Subcommand Definition

```rust
#[derive(clap::Parser)]
pub struct RecordArgs {
    // Source Selection
    #[arg(long, default_value = "system")]
    pub source: String,          // system | mic | both
    #[arg(long)]
    pub app_id: Option<String>,
    #[arg(long)]
    pub pid: Option<i32>,
    #[arg(long)]
    pub display: Option<u32>,
    #[arg(long)]
    pub list_sources: bool,

    // Audio Options
    #[arg(long, default_value = "48000")]
    pub sample_rate: u32,
    #[arg(long, default_value = "2")]
    pub channels: u8,
    #[arg(long)]
    pub no_aec: bool,
    #[arg(long)]
    pub no_comfort_noise: bool,

    // Transcription Options
    #[arg(long, default_value = "en-US")]
    pub locale: String,
    #[arg(long)]
    pub no_transcribe: bool,
    #[arg(long)]
    pub list_locales: bool,

    // Output Options
    #[arg(short = 'o', long)]
    pub output: PathBuf,
    #[arg(long, default_value = "ogg")]
    pub format: String,          // ogg
    #[arg(long, default_value = "txt")]
    pub transcript_format: String, // txt | srt | vtt | json
    #[arg(long)]
    pub transcript_output: Option<PathBuf>,

    // Recording Options
    #[arg(long)]
    pub duration: Option<String>,  // e.g. "30m", "1h"
    #[arg(long)]
    pub max_size: Option<String>,   // e.g. "500M", "2G"
    #[arg(long)]
    pub silence_timeout: Option<String>,
    #[arg(short = 'm', long)]
    pub monitor: bool,
}
```

## Command Flow

1. **Parse & validate args**
   - Resolve `--source` + `--app-id`/`--pid` into `AudioSourceConfig`
   - If `--list-sources`, print available sources and exit
   - If `--list-locales`, print supported locales and exit
   - Parse duration/size strings
   - Determine output paths (auto-name from app name + date if not specified)

2. **Check permissions**
   - Run `koe permissions` check for required permissions
   - If denied, print actionable error and exit code 1

3. **Initialize pipeline**
   - Build `PipelineConfig` from args
   - Call `RecordingPipeline::start(config)`

4. **Run & handle signals**
   - Register SIGINT/SIGTERM/SIGUSR1 handlers
   - Run until duration limit, silence timeout, max size, or user interrupt
   - Display progress on stderr (see task 30)

5. **Stop & summarize**
   - Call `pipeline.stop()`
   - Print summary to stderr: duration, file size, segment count, path
   - Exit with appropriate code

## Exit Codes
| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Permission denied |
| 2 | Invalid arguments |
| 3 | Capture error |
| 4 | Disk full / I/O error |
| 5 | Interrupted (SIGINT) |
| 6 | Internal error |

## Verification

- `koe record --list-sources` → prints apps with audio
- `koe record --list-locales` → prints supported locales
- `koe record --source mic --no-transcribe -o test.ogg` → records, no transcription
- `koe record --source system --app-id com.apple.Safari -o test.ogg` → captures system audio
- Ctrl-C during recording → partial output written, exit code 5
