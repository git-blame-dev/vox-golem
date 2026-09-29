#[allow(dead_code)]
#[path = "../src/livekit_wakeword/mod.rs"]
mod livekit_wakeword;
#[allow(dead_code)]
#[path = "../src/wake_diagnostics.rs"]
mod wake_diagnostics;
#[allow(dead_code)]
#[path = "../src/wake_word.rs"]
mod wake_word;

use hound::{SampleFormat, WavReader};
use livekit_wakeword::WakeWordModel;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Cursor, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use tempfile::NamedTempFile;
use wake_word::WakeWordRuntime;

const SAMPLE_RATE_HZ: u32 = 16_000;
const WINDOW_SAMPLES: usize = 32_000;
const HOP_SAMPLES: usize = 1_280;
const RUNTIME_FRAME_SAMPLES: usize = 480;
const THRESHOLD: f32 = 0.68;

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    sample_rate_hz: u32,
    cases: Vec<ManifestCase>,
}

#[derive(Debug, Deserialize)]
struct ManifestCase {
    id: String,
    wav: PathBuf,
    duration_samples: u64,
    sha256: String,
}

type LoadedCase = (ManifestCase, Vec<i16>);

fn main() {
    if let Err(error) = run() {
        eprintln!("wakeword_bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let (model_path, manifest_path, output_path) = parse_args()?;
    let model_path = fs::canonicalize(model_path)?;
    let manifest_path = fs::canonicalize(manifest_path)?;
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    let cases = load_cases(&manifest_path, manifest)?;
    let model_bytes = fs::read(&model_path)?;
    let mut model_snapshot = NamedTempFile::new()?;
    model_snapshot.write_all(&model_bytes)?;
    model_snapshot.flush()?;
    let model_sha256 = sha256_bytes(&fs::read(model_snapshot.path())?)?;

    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;
    let mut writer = BufWriter::new(output);
    write_json_line(
        &mut writer,
        &json!({
            "kind": "metadata",
            "schema_version": 1,
            "sample_rate_hz": SAMPLE_RATE_HZ,
            "hop_samples": HOP_SAMPLES,
            "window_samples": WINDOW_SAMPLES,
            "baseline_threshold": THRESHOLD,
            "feature_profile": "normalized_f32",
            "model_sha256": model_sha256
        }),
    )?;

    let mut model = WakeWordModel::new(&[model_snapshot.path()], SAMPLE_RATE_HZ)?;
    for (case, samples) in &cases {
        let started = Instant::now();
        let mut scores = Vec::new();
        let mut score_values = Vec::new();
        for sample_end in (WINDOW_SAMPLES..=samples.len()).step_by(HOP_SAMPLES) {
            let window = &samples[sample_end - WINDOW_SAMPLES..sample_end];
            let predictions = model.predict(window)?;
            let score = predictions
                .values()
                .next()
                .copied()
                .ok_or_else(|| io_error("wake-word model returned no classifier score"))?;
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err(io_error("wake-word model returned an invalid score").into());
            }
            scores.push(json!({"sample_end": sample_end, "score": score}));
            score_values.push((sample_end, score));
        }
        let baseline_detection_sample = first_hit_sample(&score_values);

        let mut runtime =
            WakeWordRuntime::new(model_snapshot.path(), THRESHOLD).map_err(io_error)?;
        let mut runtime_detection_sample = None;
        for frame in samples.chunks(RUNTIME_FRAME_SAMPLES) {
            let normalized_frame = frame
                .iter()
                .map(|sample| *sample as f32 / 32_768.0)
                .collect::<Vec<_>>();
            if let Some(detection) = runtime
                .process_sleeping_frame(&normalized_frame)
                .map_err(io_error)?
            {
                runtime_detection_sample = Some((detection.detected_at_ms * 16) as usize);
                break;
            }
        }
        if baseline_detection_sample != runtime_detection_sample {
            return Err(io_error(format!(
                "case {} detection parity failed: baseline {:?}, runtime {:?}",
                case.id, baseline_detection_sample, runtime_detection_sample
            ))
            .into());
        }

        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        write_json_line(
            &mut writer,
            &json!({
                "kind": "case",
                "id": case.id,
                "input_samples": samples.len(),
                "baseline_detection_sample": baseline_detection_sample,
                "scores": scores,
                "elapsed_ms": elapsed_ms
            }),
        )?;
        println!(
            "{}: parity passed ({:?})",
            case.id, baseline_detection_sample
        );
    }
    writer.flush()?;
    println!(
        "completed {} cases; output {}",
        cases.len(),
        output_path.display()
    );
    Ok(())
}

fn parse_args() -> Result<(PathBuf, PathBuf, PathBuf), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut model = None;
    let mut manifest = None;
    let mut output = None;
    while let Some(argument) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| io_error(format!("missing value for {}", argument.to_string_lossy())))?;
        match argument.to_str() {
            Some("--model") if model.is_none() => model = Some(PathBuf::from(value)),
            Some("--manifest") if manifest.is_none() => manifest = Some(PathBuf::from(value)),
            Some("--output") if output.is_none() => output = Some(PathBuf::from(value)),
            _ => {
                return Err(io_error(format!(
                    "unexpected or duplicate argument {}",
                    argument.to_string_lossy()
                ))
                .into())
            }
        }
    }
    Ok((
        model.ok_or_else(|| io_error("required argument --model is missing"))?,
        manifest.ok_or_else(|| io_error("required argument --manifest is missing"))?,
        output.ok_or_else(|| io_error("required argument --output is missing"))?,
    ))
}

fn load_cases(manifest_path: &Path, manifest: Manifest) -> Result<Vec<LoadedCase>, Box<dyn Error>> {
    if manifest.schema_version != 1 || manifest.sample_rate_hz != SAMPLE_RATE_HZ {
        return Err(io_error("manifest schema or sample rate is unsupported").into());
    }
    if manifest.cases.is_empty() {
        return Err(io_error("manifest contains no cases").into());
    }
    let root = manifest_path
        .parent()
        .ok_or_else(|| io_error("manifest has no parent directory"))?;
    let canonical_root = fs::canonicalize(root)?;
    let mut seen_ids = HashSet::new();
    let mut cases = Vec::with_capacity(manifest.cases.len());
    for case in manifest.cases {
        if case.id.trim().is_empty() || !seen_ids.insert(case.id.clone()) {
            return Err(io_error(format!("case ID is empty or duplicated: {:?}", case.id)).into());
        }
        validate_relative_path(&case.wav)?;
        let unresolved_path = root.join(&case.wav);
        let wav_path = fs::canonicalize(&unresolved_path)?;
        if !wav_path.starts_with(&canonical_root) {
            return Err(
                io_error(format!("case {} WAV escapes manifest directory", case.id)).into(),
            );
        }
        let wav_bytes = fs::read(&wav_path)?;
        validate_sha256(&case.sha256)?;
        let actual_sha256 = sha256_bytes(&wav_bytes)?;
        if case.sha256 != actual_sha256 {
            return Err(io_error(format!(
                "case {} WAV SHA-256 mismatch: manifest {}, actual {}",
                case.id, case.sha256, actual_sha256
            ))
            .into());
        }
        let mut reader = WavReader::new(Cursor::new(wav_bytes.as_slice()))?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.sample_rate != SAMPLE_RATE_HZ
            || spec.sample_format != SampleFormat::Int
            || spec.bits_per_sample != 16
        {
            return Err(io_error(format!(
                "case {} WAV must be mono PCM16 at 16000 Hz",
                case.id
            ))
            .into());
        }
        let samples = reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?;
        if samples.len() as u64 != case.duration_samples {
            return Err(io_error(format!(
                "case {} declares {} samples but WAV contains {}",
                case.id,
                case.duration_samples,
                samples.len()
            ))
            .into());
        }
        cases.push((case, samples));
    }
    Ok(cases)
}

fn validate_relative_path(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(io_error(format!(
            "WAV path must be contained and relative: {}",
            path.display()
        ))
        .into());
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(
            io_error("SHA-256 fingerprint must be 64 lowercase hexadecimal characters").into(),
        );
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> Result<String, Box<dyn Error>> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            io_error(format!(
                "unable to run sha256sum (install coreutils): {error}"
            ))
        })?;
    if let Err(error) = child
        .stdin
        .take()
        .ok_or_else(|| io_error("sha256sum stdin pipe was unavailable"))?
        .write_all(bytes)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io_error(format!("failed to provide bytes to sha256sum: {error}")).into());
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io_error(format!(
            "sha256sum failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
        .into());
    }
    let digest = String::from_utf8(output.stdout)?
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| io_error("sha256sum returned no digest"))?
        .to_owned();
    validate_sha256(&digest)?;
    Ok(digest)
}

fn first_hit_sample(scores: &[(usize, f32)]) -> Option<usize> {
    scores
        .iter()
        .find_map(|(sample_end, score)| (*score >= THRESHOLD).then_some(*sample_end))
}

fn write_json_line(
    writer: &mut impl Write,
    value: &serde_json::Value,
) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn io_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::{
        first_hit_sample, load_cases, sha256_bytes, validate_relative_path, validate_sha256,
        Manifest, ManifestCase,
    };
    use hound::{SampleFormat, WavSpec, WavWriter};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wakeword-bench-test-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(cases: Vec<ManifestCase>) -> Manifest {
        Manifest {
            schema_version: 1,
            sample_rate_hz: 16_000,
            cases,
        }
    }

    fn case(id: &str, sha256: String) -> ManifestCase {
        ManifestCase {
            id: id.into(),
            wav: "short.wav".into(),
            duration_samples: 3,
            sha256,
        }
    }

    fn wav(path: &Path, samples: &[i16], channels: u16, sample_rate: u32, bits: u16) {
        let mut writer = WavWriter::create(
            path,
            WavSpec {
                channels,
                sample_rate,
                bits_per_sample: bits,
                sample_format: SampleFormat::Int,
            },
        )
        .expect("create synthetic wav");
        for sample in samples {
            writer
                .write_sample(*sample)
                .expect("write synthetic sample");
        }
        writer.finalize().expect("finalize synthetic wav");
    }

    #[test]
    fn rejects_duplicate_ids_and_sample_count_mismatch() {
        let dir = TestDir::new();
        wav(&dir.0.join("short.wav"), &[0, 1, 2], 1, 16_000, 16);
        let fingerprint = sha256_bytes(&fs::read(dir.0.join("short.wav")).expect("read wav"))
            .expect("hash synthetic wav");
        let cases = vec![
            case("synthetic", fingerprint.clone()),
            case("synthetic", fingerprint.clone()),
        ];
        let manifest_path = dir.0.join("manifest.json");
        assert!(load_cases(&manifest_path, manifest(cases)).is_err());

        let mut mismatch = case("different", fingerprint);
        mismatch.duration_samples = 4;
        let cases = vec![mismatch];
        assert!(load_cases(&manifest_path, manifest(cases)).is_err());
    }

    #[test]
    fn validates_exact_wav_bytes_against_manifest_fingerprint() {
        let dir = TestDir::new();
        wav(&dir.0.join("short.wav"), &[0, 1, 2], 1, 16_000, 16);
        let fingerprint = sha256_bytes(&fs::read(dir.0.join("short.wav")).expect("read wav"))
            .expect("hash synthetic wav");
        let manifest_path = dir.0.join("manifest.json");

        let loaded = load_cases(
            &manifest_path,
            manifest(vec![case("valid", fingerprint.clone())]),
        )
        .expect("matching fingerprint should load");
        assert_eq!(loaded[0].1, [0, 1, 2]);
        assert!(load_cases(
            &manifest_path,
            manifest(vec![case("changed", "0".repeat(64))]),
        )
        .is_err());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
    }

    #[test]
    fn rejects_unsafe_paths_and_malformed_wav_format() {
        assert!(validate_relative_path(Path::new("../escape.wav")).is_err());
        assert!(validate_relative_path(Path::new("/absolute.wav")).is_err());

        let dir = TestDir::new();
        wav(&dir.0.join("stereo.wav"), &[0, 0], 2, 16_000, 16);
        let fingerprint = sha256_bytes(&fs::read(dir.0.join("stereo.wav")).expect("read wav"))
            .expect("hash stereo wav");
        let mut stereo = case("stereo", fingerprint);
        stereo.wav = "stereo.wav".into();
        stereo.duration_samples = 2;
        let cases = vec![stereo];
        assert!(load_cases(&dir.0.join("manifest.json"), manifest(cases)).is_err());
    }

    #[test]
    fn rejects_duplicate_manifest_fields_and_unsupported_version() {
        assert!(serde_json::from_str::<Manifest>(
            r#"{"schema_version":1,"schema_version":1,"sample_rate_hz":16000,"cases":[]}"#
        )
        .is_err());
        let dir = TestDir::new();
        assert!(load_cases(
            &dir.0.join("manifest.json"),
            Manifest {
                schema_version: 2,
                sample_rate_hz: 16_000,
                cases: vec![]
            }
        )
        .is_err());
    }

    #[test]
    fn first_hit_is_the_first_window_at_or_above_threshold() {
        assert_eq!(
            first_hit_sample(&[(32_000, 0.679), (33_280, 0.68), (34_560, 0.9)]),
            Some(33_280)
        );
        assert_eq!(first_hit_sample(&[(32_000, 0.1), (33_280, 0.679)]), None);
    }
}
