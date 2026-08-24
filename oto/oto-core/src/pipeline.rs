//! The consumer thread: converts captured frames and drives the encoder.

use oto_capture::AudioFrameOwned;
use oto_encode::{AudioChunk, AudioEncoder, Converter, Error as EncodeError, PcmFormat};

use crate::recorder::RecordingError;

/// Builds a platform-agnostic [`AudioChunk`] from a captured frame.
fn to_chunk(frame: &AudioFrameOwned) -> AudioChunk<'_> {
    AudioChunk {
        data: &frame.data,
        format: match frame.format {
            oto_capture::AudioFormat::S16 => PcmFormat::S16,
            oto_capture::AudioFormat::F32 => PcmFormat::F32,
        },
        sample_rate: u32::try_from(frame.sample_rate).unwrap_or(0),
        channels: u8::try_from(frame.channels).unwrap_or(0),
    }
}

/// Placeholder chunk for the resampler-tail flush; the Opus encoder ignores the
/// chunk and reads only the converted samples.
static EMPTY_CHUNK: AudioChunk<'_> = AudioChunk {
    data: &[],
    format: PcmFormat::S16,
    sample_rate: 0,
    channels: 1,
};

/// Drives the consumer loop to completion on the caller's thread.
///
/// Reads every frame from `receiver` until the channel closes, converts each
/// (when a converter is present), forwards it to the encoder, then flushes and
/// finalizes. Returns the encoder statistics. Used as the body of the
/// recording session's consumer thread.
///
/// # Errors
///
/// Returns [`RecordingError`] when conversion, encoding, or finalization fails.
#[allow(clippy::needless_pass_by_value)] // moved into the consumer thread
pub(crate) fn run_consumer(
    receiver: std::sync::mpsc::Receiver<AudioFrameOwned>,
    mut converter: Option<Converter>,
    mut encoder: Box<dyn AudioEncoder>,
) -> Result<oto_encode::EncoderStats, RecordingError> {
    while let Ok(frame) = receiver.recv() {
        let chunk = to_chunk(&frame);
        let i16_pcm = match &mut converter {
            Some(c) => Some(c.convert_chunk(&chunk).map_err(EncodeError::from)?),
            None => None,
        };
        encoder.write(&chunk, i16_pcm.as_deref())?;
    }

    // Drain any resampler tail (Opus path only).
    if let Some(c) = &mut converter {
        let flushed = c.flush().map_err(EncodeError::from)?;
        if !flushed.is_empty() {
            encoder.write(&EMPTY_CHUNK, Some(&flushed))?;
        }
    }

    Ok(encoder.finalize()?)
}
