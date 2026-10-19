use std::io::Cursor;

use serde::Deserialize;

use crate::tts::LocalTtsAudio;

pub(crate) const START_ACKNOWLEDGMENT: &str = "Yes?";
pub(crate) const STOP_ACKNOWLEDGMENT: &str = "Understood, sir. Working on that now for you.";
pub(crate) const SHORT_STOP_ACKNOWLEDGMENT: &str = "Understood, sir. Working on that now.";
const MAX_CUE_DURATION_SECONDS: usize = 8;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CueType {
    StartListening,
    StopListening,
    StopListeningShort,
}

pub(crate) struct PreparedCues {
    start: LocalTtsAudio,
    stop: LocalTtsAudio,
    short_stop: LocalTtsAudio,
    pub(crate) gain_db: f32,
}

impl PreparedCues {
    pub(crate) fn audio(&self, cue: CueType) -> &LocalTtsAudio {
        match cue {
            CueType::StartListening => &self.start,
            CueType::StopListening => &self.stop,
            CueType::StopListeningShort => &self.short_stop,
        }
    }
}

pub(crate) fn prepare_cues<F>(mut synthesize: F, gain_db: f32) -> Result<PreparedCues, String>
where
    F: FnMut(&str) -> Result<LocalTtsAudio, String>,
{
    if !gain_db.is_finite() || !10_f32.powf(gain_db / 20.0).is_finite() {
        return Err(String::from("local voice cue gain is invalid"));
    }
    let start = synthesize(START_ACKNOWLEDGMENT)?;
    validate_audio(&start)?;
    let stop = synthesize(STOP_ACKNOWLEDGMENT)?;
    validate_audio(&stop)?;
    let short_stop = synthesize(SHORT_STOP_ACKNOWLEDGMENT)?;
    validate_audio(&short_stop)?;
    Ok(PreparedCues {
        start,
        stop,
        short_stop,
        gain_db,
    })
}

fn validate_audio(audio: &LocalTtsAudio) -> Result<(), String> {
    if audio.sample_rate_hz == 0
        || audio.sample_rate_hz > 96_000
        || audio.pcm_f32.is_empty()
        || audio.pcm_f32.len() > audio.sample_rate_hz as usize * MAX_CUE_DURATION_SECONDS
        || audio.duration_ms > (MAX_CUE_DURATION_SECONDS as u64 * 1_000)
        || audio.pcm_f32.iter().any(|sample| !sample.is_finite())
    {
        return Err(String::from("local voice cue audio is invalid or too long"));
    }
    Ok(())
}

/// Only compile-time embedded assets reach this decoder, never renderer paths.
pub(crate) fn decode_embedded(bytes: &[u8]) -> Result<LocalTtsAudio, String> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|_| String::from("embedded cue WAV is invalid"))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate == 0
        || spec.sample_rate > 96_000
        || reader.duration() as usize > spec.sample_rate as usize * MAX_CUE_DURATION_SECONDS
        || spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
    {
        return Err(String::from("embedded cue WAV format is unsupported"));
    }
    let samples: Result<Vec<f32>, _> = reader
        .samples::<i16>()
        .map(|sample| sample.map(|sample| f32::from(sample) / 32768.0))
        .collect();
    let pcm_f32 = samples.map_err(|_| String::from("embedded cue WAV samples are invalid"))?;
    let audio = LocalTtsAudio {
        duration_ms: (pcm_f32.len() as u64 * 1_000 / u64::from(spec.sample_rate)).max(1),
        pcm_f32,
        sample_rate_hz: spec.sample_rate,
    };
    validate_audio(&audio)?;
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_audio(samples: Vec<f32>) -> LocalTtsAudio {
        LocalTtsAudio {
            pcm_f32: samples,
            sample_rate_hz: 22_050,
            duration_ms: 100,
        }
    }

    #[test]
    fn keeps_three_validated_native_pcm_variants_and_gain() {
        let mut phrases = Vec::new();
        let cues = prepare_cues(
            |phrase| {
                phrases.push(phrase.to_owned());
                Ok(sample_audio(vec![0.0, 0.5, -0.5]))
            },
            3.0,
        )
        .expect("synthetic cues");
        assert_eq!(
            phrases,
            [
                START_ACKNOWLEDGMENT,
                STOP_ACKNOWLEDGMENT,
                SHORT_STOP_ACKNOWLEDGMENT
            ]
        );
        assert_eq!(
            cues.audio(CueType::StartListening).pcm_f32,
            [0.0, 0.5, -0.5]
        );
        assert_eq!(cues.gain_db, 3.0);
    }

    #[test]
    fn rejects_invalid_or_unbounded_voice_audio() {
        for audio in [
            sample_audio(Vec::new()),
            sample_audio(vec![f32::NAN]),
            sample_audio(vec![0.0; 22_050 * (MAX_CUE_DURATION_SECONDS + 1)]),
        ] {
            assert!(validate_audio(&audio).is_err());
        }
    }

    #[test]
    fn decodes_actual_embedded_fallbacks_through_production_decoder() {
        for bytes in [
            crate::START_LISTENING_CUE_WAV,
            crate::STOP_LISTENING_CUE_WAV,
        ] {
            let audio = decode_embedded(bytes).expect("embedded cue");
            assert!(!audio.pcm_f32.is_empty());
            assert!(audio.duration_ms <= 8_000);
        }
    }
}
