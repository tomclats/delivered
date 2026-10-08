//! Regenerates the committed fixtures. `make-fixtures` writes them into
//! `fixtures/`, and a test fails if those bytes drift.

use std::fs;
use std::path::Path;

use crate::aiff::write_aiff_i16;
use crate::error::Error;
use crate::wav::write_wav_f32;

pub fn write_fixtures(root: &Path) -> Result<(), Error> {
    fs::create_dir_all(root.join("session")).map_err(|source| Error::io(root, source))?;
    let amp_23 = 10f64.powf(-23.0 / 20.0);
    let amp_18 = 10f64.powf(-18.0 / 20.0);
    write_wav_f32(
        &root.join("sine_stereo_m23.wav"),
        2,
        48_000,
        &cosine(48_000 * 2, 2, 48_000, 1_000.0, amp_23, 0),
    )?;
    write_wav_f32(&root.join("dc_offset.wav"), 1, 48_000, &vec![0.05; 48_000])?;
    let mut clipped = cosine(24_000, 1, 48_000, 440.0, 0.2, 0);
    for sample in clipped.iter_mut().take(14).skip(10) {
        *sample = 1.0;
    }
    write_wav_f32(&root.join("clipped.wav"), 1, 48_000, &clipped)?;
    let mut guitar = cosine(48_000, 1, 48_000, 1_000.0, amp_18, 0);
    guitar[100] = 1.25;
    write_wav_f32(&root.join("session").join("guitar.wav"), 1, 48_000, &guitar)?;
    let room = cosine(48_000, 1, 48_000, 1_000.0, amp_18, 2_000);
    write_wav_f32(&root.join("session").join("room.wav"), 1, 48_000, &room)?;
    let mut tone = Vec::with_capacity(24_000);
    for i in 0..24_000 {
        let sample = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48_000.0).cos() * 0.5;
        tone.push((sample * 32_767.0).round() as i16);
    }
    write_aiff_i16(&root.join("tone.aiff"), 1, 48_000, &tone)?;
    Ok(())
}

fn cosine(
    frames: usize,
    channels: usize,
    rate: u32,
    freq: f64,
    amp: f64,
    delay: usize,
) -> Vec<f32> {
    let mut out = vec![0.0; frames * channels];
    for i in delay..frames {
        let sample = ((2.0 * std::f64::consts::PI * freq * (i - delay) as f64 / rate as f64).cos()
            * amp) as f32;
        for channel in 0..channels {
            out[i * channels + channel] = sample;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn committed_fixtures_match_the_generator() {
        let dir = env::temp_dir().join(format!("delivered-fix-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_fixtures(&dir).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        for rel in [
            "sine_stereo_m23.wav",
            "dc_offset.wav",
            "clipped.wav",
            "session/guitar.wav",
            "session/room.wav",
            "tone.aiff",
        ] {
            let got = fs::read(dir.join(rel)).unwrap();
            let committed = fs::read(root.join(rel))
                .unwrap_or_else(|_| panic!("{rel} is missing; run cargo run --bin make-fixtures"));
            assert_eq!(
                got, committed,
                "{rel} drifted; run cargo run --bin make-fixtures"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
