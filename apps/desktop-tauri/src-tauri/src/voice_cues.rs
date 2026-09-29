use base64::Engine;

use crate::tts::LocalTtsAudio;

pub(crate) const START_ACKNOWLEDGMENT: &str = "Yes?";
pub(crate) const STOP_ACKNOWLEDGMENT: &str = "Understood, sir. Working on that now for you.";
pub(crate) const SHORT_STOP_ACKNOWLEDGMENT: &str = "Understood, sir. Working on that now.";
const MAX_CUE_DURATION_SECONDS: usize = 8;

pub(crate) fn prepare_cue_urls<F>(
    mut synthesize: F,
    output_gain_db: f32,
) -> Result<(String, String, String), String>
where
    F: FnMut(&str) -> Result<LocalTtsAudio, String>,
{
    let start = wav_data_url(&synthesize(START_ACKNOWLEDGMENT)?, output_gain_db)?;
    let stop = wav_data_url(&synthesize(STOP_ACKNOWLEDGMENT)?, output_gain_db)?;
    let short_stop = wav_data_url(&synthesize(SHORT_STOP_ACKNOWLEDGMENT)?, output_gain_db)?;
    Ok((start, stop, short_stop))
}

fn wav_data_url(audio: &LocalTtsAudio, output_gain_db: f32) -> Result<String, String> {
    let sample_rate = audio.sample_rate_hz;
    if sample_rate == 0
        || sample_rate > 96_000
        || !output_gain_db.is_finite()
        || audio.pcm_f32.is_empty()
        || audio.pcm_f32.len() > sample_rate as usize * MAX_CUE_DURATION_SECONDS
        || audio.duration_ms > (MAX_CUE_DURATION_SECONDS as u64 * 1_000)
        || audio.pcm_f32.iter().any(|sample| !sample.is_finite())
    {
        return Err(String::from("local voice cue audio is invalid or too long"));
    }

    let data_bytes = u32::try_from(audio.pcm_f32.len() * 2)
        .map_err(|_| String::from("local voice cue audio is too large"))?;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    let gain = 10_f32.powf(output_gain_db / 20.0);
    for sample in &audio.pcm_f32 {
        let sample = (sample * gain).clamp(-1.0, 1.0);
        wav.extend_from_slice(&((sample * i16::MAX as f32) as i16).to_le_bytes());
    }

    Ok(format!(
        "{}{}",
        crate::CUE_AUDIO_DATA_URL_PREFIX,
        base64::engine::general_purpose::STANDARD.encode(wav)
    ))
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
    fn prepares_both_short_acknowledgments_as_pcm_wav_urls() {
        let mut phrases = Vec::new();
        let (start, stop, short_stop) = prepare_cue_urls(
            |phrase| {
                phrases.push(phrase.to_string());
                Ok(sample_audio(match phrase {
                    START_ACKNOWLEDGMENT => vec![0.0, 0.5, -0.5],
                    STOP_ACKNOWLEDGMENT => vec![0.0, 0.25, -0.25],
                    _ => vec![0.0, 0.125, -0.125],
                }))
            },
            0.0,
        )
        .expect("valid synthetic cues");

        assert_eq!(
            phrases,
            [
                "Yes?",
                "Understood, sir. Working on that now for you.",
                "Understood, sir. Working on that now.",
            ]
        );
        assert_ne!(start, stop);
        assert_ne!(stop, short_stop);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(
                start
                    .strip_prefix("data:audio/wav;base64,")
                    .expect("WAV URL"),
            )
            .expect("encoded WAV");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            22_050
        );
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16);
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), 0);
        assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), 16_383);
    }

    #[test]
    fn rejects_invalid_or_unbounded_voice_audio() {
        for audio in [
            sample_audio(Vec::new()),
            sample_audio(vec![f32::NAN]),
            LocalTtsAudio {
                sample_rate_hz: 22_050,
                pcm_f32: vec![0.0; 22_050 * (MAX_CUE_DURATION_SECONDS + 1)],
                duration_ms: 5_000,
            },
        ] {
            assert!(wav_data_url(&audio, 0.0).is_err());
        }
    }
}
