//! Open every stem, measure it, and sum the session in one streaming pass.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::aiff::AiffReader;
use crate::error::Error;
use crate::measure::{Meter, Rules};
use crate::wav::{Audio, WavReader};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct FileReport {
    pub path: String,
    pub sha256: String,
    pub format: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub frames: u64,
    pub duration_seconds: f64,
    pub sample_peak: f64,
    pub sample_peak_dbfs: Option<f64>,
    pub clipped_samples: u64,
    pub dc_offset: Vec<f64>,
    pub leading_silence_samples: u64,
    pub trailing_silence_samples: u64,
    pub first_audible_sample: Option<u64>,
    pub integrated_lufs: Option<f64>,
    pub correlation: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct SessionReport {
    pub channels: u16,
    pub sample_rate: u32,
    pub frames: u64,
    pub duration_seconds: f64,
    pub sample_peak: f64,
    pub sample_peak_dbfs: Option<f64>,
    pub clipped_samples: u64,
    pub dc_offset: Vec<f64>,
    pub alignment_spread_samples: u64,
    pub integrated_lufs: Option<f64>,
    pub correlation: Option<f64>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Delivery {
    pub version: u32,
    pub label: String,
    pub rules: Rules,
    pub files: Vec<FileReport>,
    pub session: SessionReport,
}

struct Source {
    audio: Box<dyn Audio>,
    info_frames: u64,
    channels: usize,
    rel: String,
    sha256: String,
    format: String,
    bits: u16,
    buf: Vec<f64>,
    frames: usize,
    at: usize,
    done: bool,
    clips: u64,
    meter: Meter,
}

/// Measure one audio file or every WAV and AIFF file under a directory.
pub fn measure(path: &Path) -> Result<Delivery, Error> {
    let (root, files) = collect(path)?;
    let rules = Rules::default();
    let mut sources = Vec::with_capacity(files.len());
    for file in &files {
        let sha256 = sha256_file(file)?;
        let audio = open_audio(file)?;
        let info = audio.info().clone();
        sources.push(Source {
            audio,
            info_frames: info.frames,
            channels: info.channels as usize,
            rel: relative(&root, file),
            sha256,
            format: info.format.to_string(),
            bits: info.bits_per_sample,
            buf: Vec::new(),
            frames: 0,
            at: 0,
            done: false,
            clips: 0,
            meter: Meter::new(info.channels as usize, info.sample_rate, false)?,
        });
    }
    let rate = sources[0].audio.info().sample_rate;
    let mismatches: Vec<String> = sources
        .iter()
        .filter(|s| s.audio.info().sample_rate != rate)
        .map(|s| format!("{} is {} Hz", s.rel, s.audio.info().sample_rate))
        .collect();
    if !mismatches.is_empty() {
        return Err(Error::Session(format!(
            "mixed sample rates: {} is {rate} Hz, {}",
            sources[0].rel,
            mismatches.join(", ")
        )));
    }
    let session_channels = if sources.iter().any(|s| s.channels == 2) {
        2
    } else {
        1
    };
    let mut session_meter = Meter::new(session_channels, rate, true)?;
    let mut mix = vec![0.0; session_channels];
    loop {
        for source in &mut sources {
            if source.done || source.at < source.frames {
                continue;
            }
            let packet = source.audio.read(4096)?;
            if packet.frames == 0 {
                source.done = true;
                source.buf.clear();
                source.frames = 0;
                source.at = 0;
            } else {
                source.buf = packet.pcm;
                source.frames = packet.frames;
                source.at = 0;
                source.clips += packet.clipped;
            }
        }
        if sources.iter().all(|s| s.done) {
            break;
        }
        let step = sources
            .iter()
            .filter(|s| !s.done)
            .map(|s| s.frames - s.at)
            .min()
            .expect("an open stem");
        for _ in 0..step {
            mix.fill(0.0);
            for source in &mut sources {
                if source.done {
                    continue;
                }
                let start = source.at * source.channels;
                let frame = &source.buf[start..start + source.channels];
                source.meter.push(frame);
                add_to_mix(&mut mix, frame);
                source.at += 1;
            }
            session_meter.push(&mix);
        }
    }

    let mut files_out = Vec::with_capacity(sources.len());
    for source in sources {
        let Source {
            audio,
            info_frames,
            channels,
            rel,
            sha256,
            format,
            bits,
            clips,
            meter,
            ..
        } = source;
        let rate = audio.info().sample_rate;
        let measured = meter.finish();
        if measured.frames != info_frames {
            return Err(Error::format(
                Path::new(&rel),
                format!("read {} frames, header says {info_frames}", measured.frames),
            ));
        }
        files_out.push(FileReport {
            path: rel,
            sha256,
            format,
            channels: channels as u16,
            sample_rate: rate,
            bits_per_sample: bits,
            frames: measured.frames,
            duration_seconds: measured.frames as f64 / rate as f64,
            sample_peak: measured.sample_peak,
            sample_peak_dbfs: dbfs(measured.sample_peak),
            clipped_samples: clips,
            dc_offset: measured.dc_offset,
            leading_silence_samples: measured.leading_silence_samples,
            trailing_silence_samples: measured.trailing_silence_samples,
            first_audible_sample: measured.first_audible_sample,
            integrated_lufs: measured.integrated_lufs,
            correlation: measured.correlation,
        });
    }
    let session_measured = session_meter.finish();
    let (spread, notes) = notes_for(&files_out, session_measured.clipped_samples, &rules);
    let duration = session_measured.frames as f64 / rate as f64;
    Ok(Delivery {
        version: VERSION,
        label: label_of(path),
        rules,
        files: files_out,
        session: SessionReport {
            channels: session_channels as u16,
            sample_rate: rate,
            frames: session_measured.frames,
            duration_seconds: duration,
            sample_peak: session_measured.sample_peak,
            sample_peak_dbfs: dbfs(session_measured.sample_peak),
            clipped_samples: session_measured.clipped_samples,
            dc_offset: session_measured.dc_offset,
            alignment_spread_samples: spread,
            integrated_lufs: session_measured.integrated_lufs,
            correlation: session_measured.correlation,
            notes,
        },
    })
}

fn add_to_mix(mix: &mut [f64], frame: &[f64]) {
    if frame.len() == mix.len() {
        for (slot, sample) in mix.iter_mut().zip(frame) {
            *slot += *sample;
        }
    } else if frame.len() == 1 && mix.len() == 2 {
        // No pan law. A mono stem in a stereo session is heard from both sides.
        mix[0] += frame[0];
        mix[1] += frame[0];
    }
}

fn notes_for(files: &[FileReport], sum_clips: u64, rules: &Rules) -> (u64, Vec<String>) {
    let mut notes = Vec::new();
    for file in files {
        if file.frames == 0 {
            notes.push(format!("{} has no samples", file.path));
        }
        if file.clipped_samples > 0 {
            notes.push(format!(
                "{} has {} clipped {}",
                file.path,
                file.clipped_samples,
                plural(file.clipped_samples, "sample", "samples")
            ));
        }
        for (index, dc) in file.dc_offset.iter().enumerate() {
            if dc.abs() >= rules.dc_flag_abs {
                if file.channels == 1 {
                    notes.push(format!("{} DC offset is {dc:+.4}", file.path));
                } else {
                    notes.push(format!(
                        "{} DC offset is {dc:+.4} on channel {}",
                        file.path,
                        index + 1
                    ));
                }
            }
        }
        if let Some(correlation) = file.correlation {
            if correlation < rules.correlation_flag_below {
                notes.push(format!(
                    "{} stereo correlation is {correlation:.2}",
                    file.path
                ));
            }
        }
        if file.frames > 0 && file.first_audible_sample.is_none() {
            notes.push(format!("{} is silent", file.path));
        }
    }
    let audible: Vec<(&str, u64)> = files
        .iter()
        .filter_map(|file| {
            file.first_audible_sample
                .map(|sample| (file.path.as_str(), sample))
        })
        .collect();
    let spread = if audible.len() >= 2 {
        let earliest = audible.iter().map(|(_, sample)| *sample).min().unwrap();
        let latest = audible.iter().map(|(_, sample)| *sample).max().unwrap();
        let spread = latest - earliest;
        if spread > rules.alignment_flag_samples {
            let early = audible
                .iter()
                .find(|(_, sample)| *sample == earliest)
                .unwrap()
                .0;
            let late = audible
                .iter()
                .find(|(_, sample)| *sample == latest)
                .unwrap()
                .0;
            notes.push(format!("{late} starts {spread} samples after {early}"));
        }
        spread
    } else {
        0
    };
    if sum_clips > 0 {
        notes.push(format!(
            "raw sum exceeds full scale on {sum_clips} {}",
            plural(sum_clips, "sample", "samples")
        ));
    }
    (spread, notes)
}

fn plural<'a>(n: u64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

fn dbfs(peak: f64) -> Option<f64> {
    if peak.is_finite() && peak > 0.0 {
        Some(20.0 * peak.log10())
    } else {
        None
    }
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path).map_err(|source| Error::io(path, source))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65_536];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|source| Error::io(path, source))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn open_audio(path: &Path) -> Result<Box<dyn Audio>, Error> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wav" => Ok(Box::new(WavReader::open(path)?)),
        "aiff" | "aif" => Ok(Box::new(AiffReader::open(path)?)),
        _ => Err(Error::format(path, "not a WAV or AIFF file")),
    }
}

fn collect(path: &Path) -> Result<(PathBuf, Vec<PathBuf>), Error> {
    if path.is_file() {
        if !is_audio(path) {
            return Err(Error::format(path, "not a WAV or AIFF file"));
        }
        let root = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        return Ok((root, vec![path.to_path_buf()]));
    }
    if !path.is_dir() {
        return Err(Error::io(
            path,
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file or directory"),
        ));
    }
    let mut files = Vec::new();
    walk(path, &mut files)?;
    if files.is_empty() {
        return Err(Error::Session(format!(
            "no WAV or AIFF files in {}",
            path.display()
        )));
    }
    files.sort();
    Ok((path.to_path_buf(), files))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|source| Error::io(dir, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| Error::io(dir, source))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| Error::io(&path, source))?;
        if file_type.is_dir() {
            walk(&path, out)?;
        } else if path.is_file() && is_audio(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_audio(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("wav" | "aiff" | "aif")
    )
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>()
        .join("/")
}

fn label_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "session".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::write_wav_f32;
    use std::env;

    fn scratch(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("delivered-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mono_copied_into_a_stereo_session_lands_on_both_channels() {
        let dir = scratch("dual");
        write_wav_f32(&dir.join("mono.wav"), 1, 48_000, &vec![0.25; 1000]).unwrap();
        write_wav_f32(&dir.join("stereo.wav"), 2, 48_000, &vec![0.0; 2000]).unwrap();
        let delivery = measure(&dir).unwrap();
        assert_eq!(delivery.session.channels, 2);
        assert_eq!(delivery.session.dc_offset.len(), 2);
        assert!((delivery.session.dc_offset[0] - 0.25).abs() < 1e-9);
        assert!((delivery.session.dc_offset[1] - 0.25).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_mono_files_stay_mono() {
        let dir = scratch("mono");
        write_wav_f32(&dir.join("a.wav"), 1, 48_000, &vec![0.1; 800]).unwrap();
        write_wav_f32(&dir.join("b.wav"), 1, 48_000, &vec![0.1; 800]).unwrap();
        let delivery = measure(&dir).unwrap();
        assert_eq!(delivery.session.channels, 1);
        let summed = f64::from(0.1f32) * 2.0;
        assert!((delivery.session.dc_offset[0] - summed).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn late_stem_is_named_in_a_note() {
        let dir = scratch("late");
        let mut early = vec![0.2f32; 4000];
        early[..100].fill(0.0);
        let mut late = vec![0.2f32; 4000];
        late[..2100].fill(0.0);
        write_wav_f32(&dir.join("early.wav"), 1, 48_000, &early).unwrap();
        write_wav_f32(&dir.join("late.wav"), 1, 48_000, &late).unwrap();
        let delivery = measure(&dir).unwrap();
        assert_eq!(delivery.session.alignment_spread_samples, 2000);
        assert!(
            delivery
                .session
                .notes
                .iter()
                .any(|note| note == "late.wav starts 2000 samples after early.wav"),
            "{:?}",
            delivery.session.notes
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_rates_name_both_files() {
        let dir = scratch("rates");
        write_wav_f32(&dir.join("a.wav"), 1, 48_000, &[0.1; 64]).unwrap();
        write_wav_f32(&dir.join("b.wav"), 1, 44_100, &[0.1; 64]).unwrap();
        let err = measure(&dir).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("mixed sample rates"), "{text}");
        assert!(text.contains("a.wav"), "{text}");
        assert!(text.contains("b.wav"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_files_and_empty_directories() {
        let dir = scratch("empty");
        let err = measure(&dir).unwrap_err();
        assert!(err.to_string().contains("no WAV or AIFF"));
        write_wav_f32(&dir.join(".secret.wav"), 1, 48_000, &[0.2; 32]).unwrap();
        let err = measure(&dir).unwrap_err();
        assert!(err.to_string().contains("no WAV or AIFF"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ampersand_in_a_file_name_is_preserved() {
        let dir = scratch("name");
        let mut samples = vec![0.0f32; 64];
        samples[0] = 1.25;
        write_wav_f32(&dir.join("a&b.wav"), 1, 48_000, &samples).unwrap();
        let delivery = measure(&dir).unwrap();
        assert_eq!(delivery.files[0].path, "a&b.wav");
        assert_eq!(delivery.files[0].clipped_samples, 1);
        assert!(delivery.files[0].sha256.len() == 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
