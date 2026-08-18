//! JSON (`koe-transcript` v1) transcript formatter.

use koe_ffi::{AudioSourceConfig, TranscriptionSegment};
use serde_json::{Value, json};

use super::{SegmentBuffer, TranscriptFormatter, TranscriptMeta};

/// Structured JSON transcript with locale, source, and timed segments.
pub struct JsonFormatter {
    meta: TranscriptMeta,
    buffer: SegmentBuffer,
}

impl JsonFormatter {
    /// Creates a JSON formatter with the given session metadata.
    #[must_use]
    pub const fn new(meta: TranscriptMeta) -> Self {
        Self {
            meta,
            buffer: SegmentBuffer::new(),
        }
    }

    fn render(
        &self,
        segments: &[TranscriptionSegment],
    ) -> String {
        let segment_values: Vec<Value> = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                json!({
                    "index": index,
                    "start_ms": segment.start_ms,
                    "end_ms": segment.end_ms,
                    "text": segment.text,
                    "confidence": segment.confidence,
                })
            })
            .collect();

        let doc = json!({
            "format": "koe-transcript",
            "version": 1,
            "locale": self.meta.locale,
            "created_at": self.meta.created_at,
            "source": source_value(&self.meta.source),
            "segments": segment_values,
        });

        // Pretty-print for readable files. `Value` serialization is infallible in practice.
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| {
            "{\"format\":\"koe-transcript\",\"version\":1,\"segments\":[]}".to_owned()
        })
    }
}

impl TranscriptFormatter for JsonFormatter {
    fn write_segment(
        &mut self,
        segment: &TranscriptionSegment,
    ) {
        // JSON schema has no partial field — only finalized segments.
        if segment.is_final {
            self.buffer.write(segment);
        }
    }

    fn current_output(&self) -> String {
        self.committed_output()
    }

    fn committed_output(&self) -> String {
        self.render(&self.buffer.finals)
    }
}

fn source_value(source: &AudioSourceConfig) -> Value {
    match source {
        AudioSourceConfig::AppAudio { bundle_id } => json!({
            "type": "system",
            "app_bundle_id": bundle_id,
        }),
        AudioSourceConfig::PidAudio { pid } => json!({
            "type": "pid",
            "pid": pid,
        }),
        AudioSourceConfig::Microphone => json!({
            "type": "microphone",
        }),
        AudioSourceConfig::Both { bundle_id } => json!({
            "type": "both",
            "app_bundle_id": bundle_id,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> TranscriptMeta {
        TranscriptMeta {
            locale: "en-US".into(),
            created_at: "2026-08-10T15:30:00+09:00".into(),
            source: AudioSourceConfig::AppAudio {
                bundle_id: "com.google.Chrome".into(),
            },
        }
    }

    fn seg(
        text: &str,
        start_ms: i64,
        end_ms: i64,
        confidence: f32,
        is_final: bool,
    ) -> TranscriptionSegment {
        TranscriptionSegment {
            text: text.to_owned(),
            start_ms,
            end_ms,
            is_final,
            confidence,
        }
    }

    #[test]
    fn json_matches_spec_schema() {
        let mut fmt = JsonFormatter::new(meta());
        fmt.write_segment(&seg(
            "This is what was spoken in the first utterance.",
            1_250,
            4_800,
            0.95,
            true,
        ));
        fmt.write_segment(&seg(
            "This is the second utterance, which is longer.",
            5_100,
            9_200,
            0.92,
            true,
        ));
        // Partial must not appear.
        fmt.write_segment(&seg("ignored partial", 9_200, 9_500, 0.4, false));

        let value: Value = serde_json::from_str(&fmt.finalize()).expect("valid json");
        assert_eq!(value["format"], "koe-transcript");
        assert_eq!(value["version"], 1);
        assert_eq!(value["locale"], "en-US");
        assert_eq!(value["created_at"], "2026-08-10T15:30:00+09:00");
        assert_eq!(value["source"]["type"], "system");
        assert_eq!(value["source"]["app_bundle_id"], "com.google.Chrome");

        let segments = value["segments"].as_array().expect("segments array");
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0]["index"], 0);
        assert_eq!(segments[0]["start_ms"], 1_250);
        assert_eq!(segments[0]["end_ms"], 4_800);
        assert_eq!(
            segments[0]["text"],
            "This is what was spoken in the first utterance."
        );
        assert!((segments[0]["confidence"].as_f64().unwrap_or(0.0) - 0.95).abs() < 1e-5);
        assert_eq!(segments[1]["index"], 1);
        assert!((segments[1]["confidence"].as_f64().unwrap_or(0.0) - 0.92).abs() < 1e-5);
    }
}
