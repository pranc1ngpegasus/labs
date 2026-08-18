//! Far/near mixer for `--source both` (optional AEC).

#![allow(
    clippy::option_if_let_else,
    clippy::similar_names,
    clippy::suboptimal_flops
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::aec::{AcousticEchoCanceller, AecConfig};
use crate::pipeline::chunk::AudioChunk;

/// Samples per stereo block fed into AEC / mix (~5.3 ms at 48 kHz).
const BLOCK_FRAMES: usize = 256;
const BLOCK_SAMPLES: usize = BLOCK_FRAMES * 2;

/// Extra near-end presence on top of capture makeup gain.
const NEAR_GAIN: f32 = 1.5;

/// When |voice| exceeds this, duck system audio so speech stays intelligible.
const VOICE_DUCK_THRESHOLD: f32 = 0.08;
const FAR_DUCK_SCALE: f32 = 0.45;

/// Spawns a task that aligns far (system) and near (mic) PCM into one stream.
///
/// When AEC is enabled, a single mono canceller removes speaker→mic echo from
/// the near end; output is `clamp(ducked_far + NEAR_GAIN * clean)`.
pub fn spawn_both_mixer(
    mut far_rx: mpsc::Receiver<AudioChunk>,
    mut near_rx: mpsc::Receiver<AudioChunk>,
    out: broadcast::Sender<AudioChunk>,
    enable_aec: bool,
    comfort_noise: bool,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // One mono canceller (~21 ms @ 48 kHz) keeps realtime budget in debug.
        let mut aec = enable_aec.then(|| {
            AcousticEchoCanceller::new(AecConfig {
                comfort_noise,
                filter_length: 1024,
                ..AecConfig::default()
            })
        });

        let mut far_buf = Vec::<f32>::new();
        let mut near_buf = Vec::<f32>::new();
        #[allow(unused_assignments)]
        let mut last_ts = 0_u64;

        loop {
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            tokio::select! {
                chunk = far_rx.recv(), if !shutdown.load(Ordering::Acquire) => {
                    match chunk {
                        Some(c) => {
                            last_ts = c.timestamp_ms;
                            far_buf.extend_from_slice(&c.samples);
                        }
                        None => break,
                    }
                }
                chunk = near_rx.recv(), if !shutdown.load(Ordering::Acquire) => {
                    match chunk {
                        Some(c) => {
                            last_ts = c.timestamp_ms;
                            near_buf.extend_from_slice(&c.samples);
                        }
                        None => break,
                    }
                }
            }

            while far_buf.len() >= BLOCK_SAMPLES && near_buf.len() >= BLOCK_SAMPLES {
                let far: Vec<f32> = far_buf.drain(..BLOCK_SAMPLES).collect();
                let near: Vec<f32> = near_buf.drain(..BLOCK_SAMPLES).collect();
                let mixed = mix_block(&far, &near, aec.as_mut());
                if !mixed.is_empty() {
                    let _ = out.send(AudioChunk::new(mixed, last_ts));
                }
            }

            // Keep system audio flowing when the mic side lags.
            while far_buf.len() >= BLOCK_SAMPLES * 8 && near_buf.len() < BLOCK_SAMPLES {
                let far: Vec<f32> = far_buf.drain(..BLOCK_SAMPLES).collect();
                let _ = out.send(AudioChunk::new(far, last_ts));
            }
        }
    })
}

fn mix_block(
    far: &[f32],
    near: &[f32],
    aec: Option<&mut AcousticEchoCanceller>,
) -> Vec<f32> {
    let frames = far.len().min(near.len()) / 2;
    if frames == 0 {
        return Vec::new();
    }

    let mut far_l = Vec::with_capacity(frames);
    let mut far_r = Vec::with_capacity(frames);
    let mut near_m = Vec::with_capacity(frames);
    for i in 0..frames {
        let fl = far[i * 2];
        let fr = far[i * 2 + 1];
        far_l.push(fl);
        far_r.push(fr);
        // Prefer the louder mic channel (mono devices often leave one side quiet).
        let nl = near[i * 2];
        let nr = near[i * 2 + 1];
        near_m.push(if nl.abs() >= nr.abs() { nl } else { nr });
    }

    let clean = if let Some(aec) = aec {
        let far_m: Vec<f32> = far_l
            .iter()
            .zip(far_r.iter())
            .map(|(&l, &r)| 0.5 * (l + r))
            .collect();
        aec.process_block(&far_m, &near_m)
    } else {
        near_m
    };

    if clean.len() != frames {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let voice = NEAR_GAIN * clean[i];
        let far_scale = if voice.abs() > VOICE_DUCK_THRESHOLD {
            FAR_DUCK_SCALE
        } else {
            1.0
        };
        out.push((far_l[i] * far_scale + voice).clamp(-1.0, 1.0));
        out.push((far_r[i] * far_scale + voice).clamp(-1.0, 1.0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_without_aec_sums_far_and_boosted_near() {
        let far = vec![0.2_f32, 0.1, 0.2, 0.1];
        let near = vec![0.4_f32, 0.4, 0.4, 0.4];
        let out = mix_block(&far, &near, None);
        assert_eq!(out.len(), 4);
        let voice = NEAR_GAIN * 0.4;
        let far_scale = if voice.abs() > VOICE_DUCK_THRESHOLD {
            FAR_DUCK_SCALE
        } else {
            1.0
        };
        let expected_l = (0.2 * far_scale + voice).clamp(-1.0, 1.0);
        let expected_r = (0.1 * far_scale + voice).clamp(-1.0, 1.0);
        assert!((out[0] - expected_l).abs() < 1e-5);
        assert!((out[1] - expected_r).abs() < 1e-5);
    }

    #[test]
    fn mix_with_aec_keeps_far_energy() {
        let frames = 256;
        let mut far = Vec::with_capacity(frames * 2);
        let mut near = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (f32::from(u16::try_from(i).unwrap_or(u16::MAX)) * 0.01).sin() * 0.4;
            far.push(s);
            far.push(s * 0.8);
            near.push(s * 0.5);
            near.push(s * 0.4);
        }
        let mut aec = AcousticEchoCanceller::new(AecConfig {
            comfort_noise: false,
            filter_length: 64,
            ..AecConfig::default()
        });
        let out = mix_block(&far, &near, Some(&mut aec));
        assert_eq!(out.len(), frames * 2);
        let energy: f32 = out.iter().map(|s| s * s).sum();
        assert!(energy > 0.01, "mixed output should retain far-end energy");
    }

    #[test]
    fn silent_near_preserves_far_level() {
        let far = vec![0.8_f32, -0.6, 0.8, -0.6];
        let near = vec![0.0_f32; 4];
        let out = mix_block(&far, &near, None);
        assert!((out[0] - 0.8).abs() < f32::EPSILON);
        assert!((out[1] - -0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn louder_mic_channel_wins_downmix() {
        let far = vec![0.0_f32; 4];
        let near = vec![0.0_f32, 0.5, 0.0, 0.5];
        let out = mix_block(&far, &near, None);
        let voice = NEAR_GAIN * 0.5;
        assert!((out[0] - voice.clamp(-1.0, 1.0)).abs() < 1e-5);
        assert!((out[1] - voice.clamp(-1.0, 1.0)).abs() < 1e-5);
    }
}
