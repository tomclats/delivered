use std::path::Path;
use std::process::Command;

use delivered::{measure, render_html, render_json};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_delivered"))
}

fn fixture(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(rel)
}

#[test]
fn committed_stereo_sine_is_minus_23_within_a_tenth() {
    let delivery = measure(&fixture("sine_stereo_m23.wav")).unwrap();
    let lufs = delivery.files[0].integrated_lufs.expect("loudness");
    assert!(
        (lufs + 23.0).abs() < 0.1,
        "committed fixture measured {lufs} LUFS"
    );
    assert!(
        delivery.session.notes.is_empty(),
        "{:?}",
        delivery.session.notes
    );
    let json = render_json(&delivery);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["version"], 1);
    assert!((parsed["files"][0]["integrated_lufs"].as_f64().unwrap() + 23.0).abs() < 0.1);
    assert_eq!(
        render_json(&measure(&fixture("sine_stereo_m23.wav")).unwrap()),
        json
    );
}

#[test]
fn session_sheet_names_the_late_stem_and_the_clip() {
    let delivery = measure(&fixture("session")).unwrap();
    let notes = delivery.session.notes.join("\n");
    assert!(notes.contains("guitar.wav has 1 clipped sample"), "{notes}");
    assert!(
        notes.contains("room.wav starts 2000 samples after guitar.wav"),
        "{notes}"
    );
    assert!(
        notes.contains("raw sum exceeds full scale on 1 sample"),
        "{notes}"
    );
    let html = render_html(&delivery);
    assert!(html.contains("guitar.wav"));
    assert!(html.contains("room.wav"));
    assert!(html.contains("sha256"));
    assert!(html.contains("class=\"bad\""));
    let json = render_json(&delivery);
    serde_json::from_str::<serde_json::Value>(&json).unwrap();
    assert!(json.contains("\"path\": \"guitar.wav\""));
    assert_eq!(json, render_json(&measure(&fixture("session")).unwrap()));
}

#[test]
fn dc_fixture_is_flagged_and_clipped_fixture_counts_four() {
    let dc = measure(&fixture("dc_offset.wav")).unwrap();
    assert!((dc.files[0].dc_offset[0] - 0.05).abs() < 1e-6);
    assert!(dc
        .session
        .notes
        .iter()
        .any(|note| note.contains("DC offset")));
    let clipped = measure(&fixture("clipped.wav")).unwrap();
    assert_eq!(clipped.files[0].clipped_samples, 4);
}

#[test]
fn aiff_fixture_peaks_near_half_scale() {
    let delivery = measure(&fixture("tone.aiff")).unwrap();
    assert_eq!(delivery.files[0].format, "aiff-pcm");
    assert_eq!(delivery.files[0].sample_rate, 48_000);
    assert!((delivery.files[0].sample_peak - 0.5).abs() < 1e-3);
}

#[test]
fn scan_strict_exits_2_for_the_session_and_0_for_the_sine() {
    let bad = bin()
        .arg("scan")
        .arg(fixture("session"))
        .arg("--strict")
        .output()
        .unwrap();
    assert_eq!(
        bad.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    let text = String::from_utf8(bad.stdout).unwrap();
    assert!(
        text.contains("room.wav starts 2000 samples after guitar.wav"),
        "{text}"
    );
    let good = bin()
        .arg("scan")
        .arg(fixture("sine_stereo_m23.wav"))
        .output()
        .unwrap();
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    assert!(String::from_utf8(good.stdout)
        .unwrap()
        .contains("sine_stereo_m23.wav"));
}

#[test]
fn html_escapes_an_ampersand_in_the_file_name() {
    let dir = std::env::temp_dir().join(format!("delivered-cli-name-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut samples = vec![0.0f32; 32];
    samples[0] = 1.0;
    delivered_write(&dir.join("a&b.wav"), &samples);
    let delivery = measure(&dir).unwrap();
    let html = render_html(&delivery);
    assert!(html.contains("a&amp;b.wav"), "{html}");
    assert!(!html.contains("a&b.wav"));
    let json = render_json(&delivery);
    serde_json::from_str::<serde_json::Value>(&json).expect("json");
    assert!(json.contains("\"path\": \"a&b.wav\""));
    let _ = std::fs::remove_dir_all(&dir);
}

fn delivered_write(path: &Path, samples: &[f32]) {
    // The library writer is crate-private. A one-channel float WAV is enough here.
    let data_bytes = samples.len() * 4;
    let mut body = Vec::new();
    body.extend(b"RIFF");
    body.extend(0u32.to_le_bytes());
    body.extend(b"WAVE");
    body.extend(b"fmt ");
    body.extend(16u32.to_le_bytes());
    body.extend(3u16.to_le_bytes());
    body.extend(1u16.to_le_bytes());
    body.extend(48_000u32.to_le_bytes());
    body.extend((48_000u32 * 4).to_le_bytes());
    body.extend(4u16.to_le_bytes());
    body.extend(32u16.to_le_bytes());
    body.extend(b"data");
    body.extend((data_bytes as u32).to_le_bytes());
    for sample in samples {
        body.extend(sample.to_le_bytes());
    }
    let riff_size = (body.len() - 8) as u32;
    body[4..8].copy_from_slice(&riff_size.to_le_bytes());
    std::fs::write(path, body).unwrap();
}
