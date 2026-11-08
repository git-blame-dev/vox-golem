//! One Neural reply output session; clause synthesis feeds bounded native playback.
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use voxgolem_audio::playback::AudioPlaybackStream;
use voxgolem_platform::neural_tts::{AudioEvent, Error};

use super::{AppState, SynthesizeLocalTtsPayload};

pub(super) struct Session {
    runtime: Arc<super::tts::LocalTtsRuntime>,
    voice: String,
    generation: u64,
    output: Arc<Mutex<Option<AudioPlaybackStream>>>,
    sample_rate: Arc<AtomicU32>,
    sample_count: Arc<AtomicUsize>,
    submitted: Arc<AtomicBool>,
    valid: Arc<AtomicBool>,
    playback: Arc<voxgolem_audio::playback::AudioPlaybackService>,
    playback_id: u64,
    gain: f32,
    maximum_samples: usize,
    reply: bool,
    _work: super::tts::PhysicalWork,
}

impl Session {
    pub(super) fn new(
        state: &AppState,
        runtime: Arc<super::tts::LocalTtsRuntime>,
        playback_id: u64,
        reply: bool,
        generation: u64,
    ) -> Self {
        Self {
            generation,
            voice: runtime.selected_neural_voice().unwrap_or_default(),
            _work: runtime.track_neural_work(),
            maximum_samples: runtime.sample_rate_hz() as usize
                * state
                    .runtime_config
                    .as_ref()
                    .map_or(300, |config| config.local_tts.max_duration_s)
                    as usize,
            runtime,
            output: Arc::new(Mutex::new(None)),
            sample_rate: Arc::new(AtomicU32::new(0)),
            sample_count: Arc::new(AtomicUsize::new(0)),
            submitted: Arc::new(AtomicBool::new(false)),
            valid: Arc::new(AtomicBool::new(false)),
            playback: Arc::clone(&state.tts_audio_playback),
            playback_id,
            gain: state
                .runtime_config
                .as_ref()
                .map_or(0.0, |config| config.local_tts.output_gain_db),
            reply,
        }
    }

    /// Completion here acknowledges delivered PCM, not audible completion. The
    /// adapter retains the output session/update lease through finish/drain.
    pub(super) async fn append(&self, text: &str) -> Result<(), String> {
        let client = self
            .runtime
            .neural_client()
            .ok_or("Neural TTS runtime unavailable")?;
        let (cancellation, _) = self.runtime.neural_request(self.generation)?;
        #[cfg(feature = "app-benchmark")]
        if self.reply {
            super::app_benchmark::mark_once("reply_synthesis_requested");
        }
        client
            .synthesize(text, &self.voice, cancellation, |event| {
                let output = Arc::clone(&self.output);
                let sample_rate = Arc::clone(&self.sample_rate);
                let sample_count = Arc::clone(&self.sample_count);
                let submitted = Arc::clone(&self.submitted);
                let valid = Arc::clone(&self.valid);
                let playback = Arc::clone(&self.playback);
                let maximum_samples = self.maximum_samples;
                let playback_id = self.playback_id;
                let gain = self.gain;
                let reply = self.reply;
                async move {
                    let _ = reply;
                    match event {
                        AudioEvent::Format(format) => {
                            let previous =
                                sample_rate.swap(format.sample_rate_hz, Ordering::AcqRel);
                            if previous != 0 && previous != format.sample_rate_hz {
                                return Err(Error::Protocol);
                            }
                        }
                        AudioEvent::Samples(samples) => {
                            if sample_count
                                .fetch_add(samples.len(), Ordering::Relaxed)
                                .saturating_add(samples.len())
                                > maximum_samples
                            {
                                return Err(Error::Protocol);
                            }
                            if samples.iter().any(|sample| *sample != 0.0)
                                && !valid.swap(true, Ordering::Relaxed)
                            {
                                #[cfg(feature = "app-benchmark")]
                                if reply {
                                    super::app_benchmark::mark("reply_synthesis_valid");
                                }
                            }
                            let rate = sample_rate.load(Ordering::Acquire);
                            let existing = output.lock().map_err(|_| Error::Output)?.clone();
                            // Provider/queue waits do not hold an empty output device.
                            let stream = if let Some(stream) = existing {
                                stream
                            } else {
                                let stream = playback
                                    .open_stream_owned(playback_id, rate, gain, move || {
                                        #[cfg(feature = "app-benchmark")]
                                        if reply {
                                            super::app_benchmark::mark_once(
                                                "reply_output_activated",
                                            );
                                        }
                                    })
                                    .map_err(|_| Error::Output)?;
                                *output.lock().map_err(|_| Error::Output)? = Some(stream.clone());
                                stream
                            };
                            super::spawn_runtime_blocking(move || {
                                if samples.len() <= (rate as usize).min(16_384) {
                                    stream.append(samples)
                                } else {
                                    for chunk in samples.chunks((rate as usize).min(16_384)) {
                                        stream.append(chunk.to_vec())?;
                                    }
                                    Ok(())
                                }
                            })
                            .await
                            .map_err(|_| Error::Output)?
                            .map_err(|_| Error::Output)?;
                            if !submitted.swap(true, Ordering::Relaxed) {
                                #[cfg(feature = "app-benchmark")]
                                if reply {
                                    super::app_benchmark::mark("reply_output_submitted");
                                }
                            }
                        }
                    }
                    Ok(())
                }
            })
            .await
            .map_err(|error| error.to_string())
    }

    pub(super) async fn finish(&self, failed: bool) -> Result<u64, String> {
        let stream = self
            .output
            .lock()
            .map_err(|_| "Neural output state unavailable")?
            .take();
        let stream = stream.ok_or("Neural TTS returned no audio")?;
        let completion = super::spawn_runtime_blocking(move || {
            if failed {
                stream.fail();
                stream.wait()
            } else {
                stream.finish()
            }
        })
        .await
        .map_err(|_| "Neural output retirement worker failed")?
        .map_err(|_| "Neural TTS native playback failed")?;
        #[cfg(feature = "app-benchmark")]
        if self.reply && !failed {
            super::app_benchmark::mark("reply_output_completed");
        }
        Ok(completion.duration_ms)
    }
}

pub(super) async fn speak(
    state: &AppState,
    runtime: Arc<super::tts::LocalTtsRuntime>,
    text: String,
    playback_id: u64,
    reply: bool,
    generation: u64,
) -> Result<SynthesizeLocalTtsPayload, String> {
    let session = Session::new(state, runtime, playback_id, reply, generation);
    let produced = session.append(&text).await;
    let retired = session.finish(produced.is_err()).await;
    super::finish_tts_playback_state(&state.tts_playback, playback_id)?;
    if let Err(error) = produced {
        super::log_tts_runtime_event(
            &state.telemetry_sink,
            super::diagnostics::TtsEvent::SynthesisFailed,
        );
        return Err(error);
    }
    retired
        .map(|duration_ms| SynthesizeLocalTtsPayload { duration_ms })
        .inspect_err(|_| {
            super::log_tts_runtime_event(
                &state.telemetry_sink,
                super::diagnostics::TtsEvent::PlaybackFailed,
            );
        })
}
