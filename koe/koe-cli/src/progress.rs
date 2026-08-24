//! Live progress output for `koe record`.
//!
//! TTY stderr gets a two-line status + segment block; non-TTY stderr gets
//! newline-delimited JSON (one object per event).

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use koe_core::{
    AudioSourceConfig, OutputFormat, RecordingState, RecordingStatus, RecordingSummary,
    TranscriptionSegment,
};

/// Braille spinner frames (rotated on each status update).
const SPINNER_FRAMES: &[char] = &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

/// Static labels painted beside dynamic status (format + capture source).
#[derive(Debug, Clone)]
pub struct ProgressMeta {
    /// e.g. `OGG 48kHz stereo`
    pub format_label: String,
    /// e.g. `App: Google Chrome (PID 4201)` or `Mic`
    pub source_label: String,
    /// `[SYS]` / `[MIC]` / `[SYS+MIC]` prefix pinned on live transcript lines.
    pub source_tag: &'static str,
    /// Encoded audio output path (shown in the finish summary).
    pub output_path: PathBuf,
    /// Transcript path when transcription is enabled.
    pub transcript_path: Option<PathBuf>,
}

impl ProgressMeta {
    /// Builds labels from the session's audio format and capture source.
    #[must_use]
    pub fn new(
        audio_format: &OutputFormat,
        source: &AudioSourceConfig,
        output_path: PathBuf,
        transcript_path: Option<PathBuf>,
    ) -> Self {
        Self {
            format_label: format_label(audio_format),
            source_label: source_label(source),
            source_tag: source_tag(source),
            output_path,
            transcript_path,
        }
    }
}

/// Renders recording progress to stderr (TTY ANSI or NDJSON).
pub trait ProgressRenderer: Send {
    /// Updates the live status line / status JSON object.
    fn render_status(
        &mut self,
        status: &RecordingStatus,
    );

    /// Updates the partial segment line or emits a final / JSON segment.
    fn render_segment(
        &mut self,
        segment: &TranscriptionSegment,
    );

    /// Clears the live block (TTY) and prints the session summary.
    fn finish(
        &mut self,
        summary: &RecordingSummary,
    );

    /// Clears any in-place TTY painting so a subsequent `eprintln!` stays clean.
    fn prepare_message(&mut self) {}
}

/// Selects TTY vs JSON based on whether stderr is a terminal.
#[must_use]
pub fn create_renderer(meta: ProgressMeta) -> Box<dyn ProgressRenderer> {
    create_renderer_with(std::io::stderr().is_terminal(), meta)
}

/// Test / override entry for forcing TTY or JSON mode.
#[must_use]
pub fn create_renderer_with(
    is_tty: bool,
    meta: ProgressMeta,
) -> Box<dyn ProgressRenderer> {
    if is_tty {
        Box::new(TtyRenderer::new(meta))
    } else {
        Box::new(JsonRenderer::new(meta))
    }
}

/// ANSI live-updating status + segment block on stderr.
pub struct TtyRenderer {
    meta: ProgressMeta,
    spinner_idx: usize,
    status_line: String,
    /// Open partial line (without trailing newline), if any.
    partial_line: Option<String>,
    /// Live content rows painted (0–2). The cursor rests on an anchor row
    /// directly below this block so the next CUU lands on the first row.
    painted_lines: u8,
}

impl TtyRenderer {
    #[must_use]
    pub const fn new(meta: ProgressMeta) -> Self {
        Self {
            meta,
            spinner_idx: 0,
            status_line: String::new(),
            partial_line: None,
            painted_lines: 0,
        }
    }

    fn paint(&mut self) {
        let mut err = std::io::stderr().lock();
        let prev_painted = self.painted_lines;
        if prev_painted > 0 {
            let _ = write!(err, "\x1b[{prev_painted}A");
        }
        let _ = write!(err, "\r\x1b[2K{}", self.status_line);
        let content_lines = self.partial_line.as_ref().map_or(1, |partial| {
            let _ = write!(err, "\n\x1b[2K{partial}");
            2
        });
        if content_lines == 2 {
            // Anchor row below a two-line block.
            let _ = writeln!(err);
        } else if prev_painted > 1 {
            // Shrunk from two lines to one — clear the old partial row and
            // reuse it as the anchor so CUU(1) still reaches the status row.
            let _ = write!(err, "\n\x1b[2K");
        } else {
            let _ = writeln!(err);
        }
        self.painted_lines = content_lines;
        let _ = err.flush();
    }

    fn clear_live(&mut self) {
        if self.painted_lines == 0 {
            return;
        }
        let mut err = std::io::stderr().lock();
        // Cursor sits on the anchor row directly below the content block.
        let _ = write!(err, "\x1b[{}A", self.painted_lines);
        for i in 0..self.painted_lines {
            if i > 0 {
                let _ = writeln!(err);
            }
            let _ = write!(err, "\r\x1b[2K");
        }
        let _ = write!(err, "\r\x1b[2K");
        let _ = err.flush();
        self.painted_lines = 0;
    }
}

/// Max grapheme-ish chars shown for a live segment line (keeps TTY row count stable).
const SEGMENT_TEXT_DISPLAY_CHARS: usize = 80;

impl ProgressRenderer for TtyRenderer {
    fn render_status(
        &mut self,
        status: &RecordingStatus,
    ) {
        let frame = SPINNER_FRAMES[self.spinner_idx % SPINNER_FRAMES.len()];
        self.spinner_idx = self.spinner_idx.wrapping_add(1);
        self.status_line = format!(
            "{} | {frame} {} | {} | {}",
            state_verb(status.state),
            format_hms(status.elapsed_ms),
            self.meta.format_label,
            self.meta.source_label,
        );
        self.paint();
    }

    fn render_segment(
        &mut self,
        segment: &TranscriptionSegment,
    ) {
        let max_chars = if segment.is_final {
            usize::MAX
        } else {
            SEGMENT_TEXT_DISPLAY_CHARS
        };
        let text = truncate_display(&segment.text, max_chars);
        let line = format_segment_line(self.meta.source_tag, segment.start_ms, &text);
        if segment.is_final {
            // Commit the segment as a permanent line above the live block.
            self.clear_live();
            eprintln!("{line}");
            self.partial_line = None;
            if !self.status_line.is_empty() {
                self.paint();
            }
            return;
        }

        self.partial_line = Some(line);
        self.paint();
    }

    fn finish(
        &mut self,
        summary: &RecordingSummary,
    ) {
        self.clear_live();
        self.partial_line = None;
        print_human_summary(
            summary,
            &self.meta.output_path,
            self.meta.transcript_path.as_deref(),
        );
    }

    fn prepare_message(&mut self) {
        self.clear_live();
        self.partial_line = None;
    }
}

/// Newline-delimited JSON progress on stderr (for pipes / CI).
pub struct JsonRenderer {
    meta: ProgressMeta,
}

impl JsonRenderer {
    #[must_use]
    pub const fn new(meta: ProgressMeta) -> Self {
        Self { meta }
    }
}

impl ProgressRenderer for JsonRenderer {
    fn render_status(
        &mut self,
        status: &RecordingStatus,
    ) {
        eprintln!("{}", status_json_line(status));
    }

    fn render_segment(
        &mut self,
        segment: &TranscriptionSegment,
    ) {
        eprintln!("{}", segment_json_line(segment));
    }

    fn finish(
        &mut self,
        summary: &RecordingSummary,
    ) {
        eprintln!(
            "{}",
            summary_json_line(
                summary,
                &self.meta.output_path,
                self.meta.transcript_path.as_deref()
            )
        );
    }
}

fn status_json_line(status: &RecordingStatus) -> String {
    serde_json::json!({
        "type": "status",
        "elapsed_ms": status.elapsed_ms,
        "bytes_written": status.bytes_written,
        "state": state_verb(status.state),
    })
    .to_string()
}

fn segment_json_line(segment: &TranscriptionSegment) -> String {
    serde_json::json!({
        "type": "segment",
        "start_ms": segment.start_ms,
        "end_ms": segment.end_ms,
        "text": segment.text,
        "is_final": segment.is_final,
    })
    .to_string()
}

fn summary_json_line(
    summary: &RecordingSummary,
    output: &Path,
    transcript: Option<&Path>,
) -> String {
    serde_json::json!({
        "type": "summary",
        "duration_sec": summary.duration_sec,
        "bytes_written": summary.bytes_written,
        "transcript_segment_count": summary.transcript_segment_count,
        "dropped_audio_frames": summary.dropped_audio_frames,
        "output": output.display().to_string(),
        "transcript": transcript.map(|p| p.display().to_string()),
    })
    .to_string()
}

const fn state_verb(state: RecordingState) -> &'static str {
    match state {
        RecordingState::Recording => "Recording",
        RecordingState::Paused => "Paused",
        RecordingState::Stopping => "Stopping",
        RecordingState::Stopped => "Stopped",
    }
}

fn format_label(format: &OutputFormat) -> String {
    let name = match format {
        OutputFormat::Ogg { .. } => "OGG",
    };
    format!("{name} 48kHz stereo")
}

/// Tag pinning the capture source on live transcript lines: system audio is
/// `[SYS]`, the microphone is `[MIC]`, and the mixed (AEC) stream — which
/// cannot be attributed per utterance — is `[SYS+MIC]`.
const fn source_tag(source: &AudioSourceConfig) -> &'static str {
    match source {
        AudioSourceConfig::Microphone => "[MIC]",
        AudioSourceConfig::AppAudio { .. } | AudioSourceConfig::PidAudio { .. } => "[SYS]",
        AudioSourceConfig::Both { .. } => "[SYS+MIC]",
    }
}

/// Formats one live transcript line with its capture-source prefix.
fn format_segment_line(
    source_tag: &str,
    start_ms: i64,
    text: &str,
) -> String {
    format!(
        "{source_tag} [{}] \"{text}\"",
        format_hms(millis_u64(start_ms)),
    )
}

fn source_label(source: &AudioSourceConfig) -> String {
    match source {
        AudioSourceConfig::Microphone => "Mic".to_owned(),
        AudioSourceConfig::AppAudio { bundle_id } => describe_app(bundle_id, None),
        AudioSourceConfig::PidAudio { pid } => format!("PID {pid}"),
        AudioSourceConfig::Both { bundle_id } => {
            format!("Both · {}", describe_app(bundle_id, None))
        },
    }
}

fn describe_app(
    bundle_id: &str,
    pid_hint: Option<i32>,
) -> String {
    // Best-effort name lookup; fall back to the bundle id.
    let (name, pid) = koe_core::enumerate_apps()
        .into_iter()
        .find(|app| app.bundle_id.as_deref() == Some(bundle_id))
        .map_or_else(
            || (bundle_id.to_owned(), pid_hint),
            |app| (app.name, Some(app.pid)),
        );
    pid.map_or_else(
        || format!("App: {name}"),
        |pid| format!("App: {name} (PID {pid})"),
    )
}

fn format_hms(elapsed_ms: u64) -> String {
    let total = Duration::from_millis(elapsed_ms);
    let secs = total.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn millis_u64(ms: i64) -> u64 {
    u64::try_from(ms.max(0)).unwrap_or(0)
}

fn truncate_display(
    text: &str,
    max_chars: usize,
) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            // Drop CSI sequences (ESC [ ... final) so STT quirks cannot break the TTY.
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if count >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn print_human_summary(
    summary: &RecordingSummary,
    output: &Path,
    transcript: Option<&Path>,
) {
    eprintln!(
        "Done: {:.1}s, {} bytes, {} segments → {}",
        summary.duration_sec,
        summary.bytes_written,
        summary.transcript_segment_count,
        output.display()
    );
    if let Some(path) = transcript {
        eprintln!("Transcript → {}", path.display());
    }
    if summary.dropped_audio_frames > 0 {
        eprintln!(
            "warning: dropped {} audio frames during capture",
            summary.dropped_audio_frames
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta() -> ProgressMeta {
        ProgressMeta {
            format_label: "OGG 48kHz stereo".into(),
            source_label: "Mic".into(),
            source_tag: "[MIC]",
            output_path: PathBuf::from("out.ogg"),
            transcript_path: Some(PathBuf::from("out.txt")),
        }
    }

    fn status(
        elapsed_ms: u64,
        bytes: u64,
    ) -> RecordingStatus {
        RecordingStatus {
            elapsed_ms,
            bytes_written: bytes,
            level_left: 0.1,
            level_right: 0.2,
            state: RecordingState::Recording,
        }
    }

    fn segment(
        text: &str,
        start_ms: i64,
        end_ms: i64,
        is_final: bool,
    ) -> TranscriptionSegment {
        TranscriptionSegment {
            text: text.into(),
            start_ms,
            end_ms,
            is_final,
            confidence: 0.9,
        }
    }

    #[test]
    fn format_hms_pads_hours() {
        assert_eq!(format_hms(0), "00:00:00");
        assert_eq!(format_hms(154_000), "00:02:34");
        assert_eq!(format_hms(3_661_000), "01:01:01");
    }

    #[test]
    fn format_label_names_containers() {
        assert_eq!(
            format_label(&OutputFormat::Ogg { quality: 0.5 }),
            "OGG 48kHz stereo"
        );
    }

    #[test]
    fn source_label_mic() {
        assert_eq!(source_label(&AudioSourceConfig::Microphone), "Mic");
    }

    #[test]
    fn source_tag_maps_capture_sources() {
        assert_eq!(source_tag(&AudioSourceConfig::Microphone), "[MIC]");
        assert_eq!(
            source_tag(&AudioSourceConfig::AppAudio {
                bundle_id: "com.example.app".into()
            }),
            "[SYS]"
        );
        assert_eq!(
            source_tag(&AudioSourceConfig::PidAudio { pid: 42 }),
            "[SYS]"
        );
        assert_eq!(
            source_tag(&AudioSourceConfig::Both {
                bundle_id: "com.example.app".into()
            }),
            "[SYS+MIC]"
        );
    }

    #[test]
    fn segment_line_includes_source_and_timestamp() {
        assert_eq!(
            format_segment_line("[MIC]", 154_000, "hello"),
            "[MIC] [00:02:34] \"hello\""
        );
    }

    #[test]
    fn source_label_pid() {
        assert_eq!(
            source_label(&AudioSourceConfig::PidAudio { pid: 42 }),
            "PID 42"
        );
    }

    #[test]
    fn spinner_cycles_through_braille_frames() {
        assert_eq!(SPINNER_FRAMES.len(), 8);
        assert_eq!(SPINNER_FRAMES[0], '⣾');
        assert_eq!(SPINNER_FRAMES[7], '⣷');
    }

    #[test]
    fn json_status_shape() {
        let status = status(154_000, 1_843_200);
        let line: serde_json::Value =
            serde_json::from_str(&status_json_line(&status)).expect("json");
        assert_eq!(line["type"], "status");
        assert_eq!(line["elapsed_ms"], 154_000);
        assert_eq!(line["bytes_written"], 1_843_200);
        assert_eq!(line["state"], "Recording");
        assert!(line.get("level_left").is_none());
    }

    #[test]
    fn json_segment_shape() {
        let seg = segment("hello", 150_000, 152_400, false);
        let line: serde_json::Value = serde_json::from_str(&segment_json_line(&seg)).expect("json");
        assert_eq!(line["type"], "segment");
        assert_eq!(line["text"], "hello");
        assert_eq!(line["is_final"], false);
    }

    #[test]
    fn truncate_display_strips_controls_and_limits() {
        let long = "a".repeat(100);
        let clipped = truncate_display(&long, 10);
        assert_eq!(clipped.chars().count(), 11); // 10 + ellipsis
        assert!(clipped.ends_with('…'));
        assert_eq!(truncate_display("hi\u{1b}[31mx", 80), "hix");
    }

    #[test]
    fn create_renderer_with_selects_modes() {
        // Construction only — avoid painting ANSI onto the test runner's stderr.
        let _tty = create_renderer_with(true, sample_meta());
        let mut json = create_renderer_with(false, sample_meta());
        json.render_status(&status(0, 0));
        json.render_segment(&segment("hi", 0, 100, false));
    }

    #[test]
    fn state_verb_covers_lifecycle() {
        assert_eq!(state_verb(RecordingState::Recording), "Recording");
        assert_eq!(state_verb(RecordingState::Paused), "Paused");
        assert_eq!(state_verb(RecordingState::Stopping), "Stopping");
        assert_eq!(state_verb(RecordingState::Stopped), "Stopped");
    }
}
