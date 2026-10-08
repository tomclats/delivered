//! Streaming AIFF PCM reader. 16-bit and 24-bit, mono and stereo.
//! AIFC and float AIFF are refused.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::wav::{Audio, AudioInfo, Packet};

#[derive(Clone, Copy)]
enum Kind {
    Pcm16,
    Pcm24,
}

pub struct AiffReader {
    path: PathBuf,
    file: File,
    info: AudioInfo,
    data_left: u64,
    kind: Kind,
    bytes_per_frame: usize,
}

impl AiffReader {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut file = File::open(path).map_err(|source| Error::io(path, source))?;
        let len = file
            .metadata()
            .map_err(|source| Error::io(path, source))?
            .len();
        let mut hdr = [0u8; 12];
        read_exact(path, &mut file, &mut hdr)?;
        if &hdr[0..4] != b"FORM" {
            return Err(Error::format(path, "not an AIFF file"));
        }
        if &hdr[8..12] == b"AIFC" {
            return Err(Error::format(
                path,
                "compressed AIFF is not measured; export AIFF PCM",
            ));
        }
        if &hdr[8..12] != b"AIFF" {
            return Err(Error::format(path, "not an AIFF file"));
        }
        let form_size = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
        let limit = len.min(8 + form_size);
        let mut pos = 12u64;
        let mut comm: Option<Vec<u8>> = None;
        let mut ssnd: Option<(u64, u64)> = None;
        while pos + 8 <= limit {
            seek(path, &mut file, pos)?;
            let mut chunk = [0u8; 8];
            read_exact(path, &mut file, &mut chunk)?;
            let size = u32::from_be_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let body = pos + 8;
            if body + size > len {
                return Err(Error::format(
                    path,
                    "a chunk extends past the end of the file",
                ));
            }
            match &chunk[0..4] {
                b"COMM" => {
                    if comm.is_some() {
                        return Err(Error::format(path, "more than one COMM chunk"));
                    }
                    if !(18..=65_536).contains(&size) {
                        return Err(Error::format(path, "COMM chunk has an unexpected size"));
                    }
                    let mut buf = vec![0u8; size as usize];
                    seek(path, &mut file, body)?;
                    read_exact(path, &mut file, &mut buf)?;
                    comm = Some(buf);
                }
                b"SSND" => {
                    if ssnd.is_some() {
                        return Err(Error::format(path, "more than one SSND chunk"));
                    }
                    ssnd = Some((body, size));
                }
                _ => {}
            }
            pos = body + size + (size & 1);
        }
        let comm = comm.ok_or_else(|| Error::format(path, "COMM chunk is missing"))?;
        let (ssnd_pos, ssnd_size) =
            ssnd.ok_or_else(|| Error::format(path, "SSND chunk is missing"))?;
        let channels = i16::from_be_bytes([comm[0], comm[1]]);
        let frames = u32::from_be_bytes([comm[2], comm[3], comm[4], comm[5]]) as u64;
        let bits = i16::from_be_bytes([comm[6], comm[7]]);
        let sample_rate = decode_rate(path, &comm[8..18])?;
        if channels != 1 && channels != 2 {
            return Err(Error::format(
                path,
                format!("{channels} channels; delivered measures mono and stereo"),
            ));
        }
        if !(8_000..=384_000).contains(&sample_rate) {
            return Err(Error::format(
                path,
                format!("sample rate {sample_rate} Hz is outside 8000-384000"),
            ));
        }
        let kind = match bits {
            16 => Kind::Pcm16,
            24 => Kind::Pcm24,
            _ => {
                return Err(Error::format(
                    path,
                    format!("unsupported AIFF encoding ({bits} bits)"),
                ));
            }
        };
        let bytes_per_sample = (bits / 8) as usize;
        let bytes_per_frame = channels as usize * bytes_per_sample;
        if ssnd_size < 8 {
            return Err(Error::format(path, "SSND chunk is too short"));
        }
        seek(path, &mut file, ssnd_pos)?;
        let mut ssnd_hdr = [0u8; 8];
        read_exact(path, &mut file, &mut ssnd_hdr)?;
        let offset =
            u32::from_be_bytes([ssnd_hdr[0], ssnd_hdr[1], ssnd_hdr[2], ssnd_hdr[3]]) as u64;
        let audio_at = ssnd_pos + 8 + offset;
        let available = ssnd_size - 8;
        if offset > available {
            return Err(Error::format(path, "SSND offset extends past the chunk"));
        }
        let audio_bytes = available - offset;
        let need = frames * bytes_per_frame as u64;
        if need > audio_bytes {
            return Err(Error::format(path, "COMM frame count does not fit in SSND"));
        }
        seek(path, &mut file, audio_at)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            info: AudioInfo {
                channels: channels as u16,
                sample_rate,
                bits_per_sample: bits as u16,
                format: "aiff-pcm",
                frames,
            },
            data_left: need,
            kind,
            bytes_per_frame,
        })
    }
}

impl Audio for AiffReader {
    fn info(&self) -> &AudioInfo {
        &self.info
    }

    fn read(&mut self, max_frames: usize) -> Result<Packet, Error> {
        if max_frames == 0 || self.data_left == 0 {
            return Ok(Packet {
                pcm: Vec::new(),
                frames: 0,
                clipped: 0,
            });
        }
        let bpf = self.bytes_per_frame;
        let frames = max_frames.min((self.data_left / bpf as u64) as usize);
        let nbytes = frames * bpf;
        let mut raw = vec![0u8; nbytes];
        read_exact(&self.path, &mut self.file, &mut raw)?;
        self.data_left -= nbytes as u64;
        let mut pcm = Vec::with_capacity(frames * self.info.channels as usize);
        let mut clipped = 0u64;
        let mut i = 0;
        let samples = frames * self.info.channels as usize;
        for _ in 0..samples {
            let (sample, clip) = match self.kind {
                Kind::Pcm16 => {
                    let raw_s = i16::from_be_bytes([raw[i], raw[i + 1]]);
                    i += 2;
                    let clip = raw_s == i16::MAX || raw_s == i16::MIN;
                    (raw_s as f64 / 32768.0, clip)
                }
                Kind::Pcm24 => {
                    let mut v =
                        ((raw[i] as i32) << 16) | ((raw[i + 1] as i32) << 8) | raw[i + 2] as i32;
                    i += 3;
                    if v & 0x0080_0000 != 0 {
                        v |= !0x00FF_FFFF;
                    }
                    let clip = v == 8_388_607 || v == -8_388_608;
                    (v as f64 / 8_388_608.0, clip)
                }
            };
            if clip {
                clipped += 1;
            }
            pcm.push(sample);
        }
        Ok(Packet {
            pcm,
            frames,
            clipped,
        })
    }
}

fn decode_rate(path: &Path, bytes: &[u8]) -> Result<u32, Error> {
    let rate =
        decode_extended(bytes).ok_or_else(|| Error::format(path, "sample rate is not a number"))?;
    if !rate.is_finite() || rate <= 0.0 {
        return Err(Error::format(path, "sample rate is not a number"));
    }
    let rounded = rate.round();
    if (rate - rounded).abs() > 0.01 || rounded > u32::MAX as f64 {
        return Err(Error::format(path, "sample rate is not an integer"));
    }
    Ok(rounded as u32)
}

fn decode_extended(bytes: &[u8]) -> Option<f64> {
    if bytes.len() < 10 {
        return None;
    }
    let sign = bytes[0] & 0x80 != 0;
    let exp = (((bytes[0] & 0x7f) as u16) << 8) | bytes[1] as u16;
    let mut mant_bytes = [0u8; 8];
    mant_bytes.copy_from_slice(&bytes[2..10]);
    let mant = u64::from_be_bytes(mant_bytes);
    if exp == 0 && mant == 0 {
        return Some(0.0);
    }
    if exp == 0x7fff {
        return None;
    }
    let magnitude = (mant as f64) / ((1u64 << 63) as f64) * 2f64.powi(i32::from(exp) - 16383);
    Some(if sign { -magnitude } else { magnitude })
}

fn encode_extended(value: f64) -> [u8; 10] {
    if value == 0.0 {
        return [0; 10];
    }
    let sign = value.is_sign_negative();
    let bits = value.abs().to_bits();
    let f_exp = ((bits >> 52) & 0x7ff) as i32;
    let f_mant = bits & 0x000f_ffff_ffff_ffff;
    let exp = (f_exp - 1023 + 16383) as u16;
    let mant = (1u64 << 63) | (f_mant << 11);
    let mut out = [0u8; 10];
    out[0] = ((exp >> 8) as u8) | if sign { 0x80 } else { 0 };
    out[1] = exp as u8;
    out[2..10].copy_from_slice(&mant.to_be_bytes());
    out
}

pub fn write_aiff_i16(
    path: &Path,
    channels: u16,
    sample_rate: u32,
    interleaved: &[i16],
) -> Result<(), Error> {
    if channels == 0 || channels > 2 {
        return Err(Error::format(path, "AIFF writer supports mono and stereo"));
    }
    if interleaved.len() % channels as usize != 0 {
        return Err(Error::format(
            path,
            "frame count does not match the channel count",
        ));
    }
    let frames = interleaved.len() / channels as usize;
    if frames > u32::MAX as usize {
        return Err(Error::format(
            path,
            "AIFF frame count does not fit in 32 bits",
        ));
    }
    let data_bytes = interleaved.len() * 2;
    let ssnd_size = 8 + data_bytes;
    let form_size = 4 + (8 + 18) + (8 + ssnd_size);
    let mut body = Vec::with_capacity(8 + form_size);
    body.extend(b"FORM");
    body.extend((form_size as u32).to_be_bytes());
    body.extend(b"AIFF");
    body.extend(b"COMM");
    body.extend(18u32.to_be_bytes());
    body.extend((channels as i16).to_be_bytes());
    body.extend((frames as u32).to_be_bytes());
    body.extend(16i16.to_be_bytes());
    body.extend(encode_extended(sample_rate as f64));
    body.extend(b"SSND");
    body.extend((ssnd_size as u32).to_be_bytes());
    body.extend(0u32.to_be_bytes());
    body.extend(0u32.to_be_bytes());
    for sample in interleaved {
        body.extend(sample.to_be_bytes());
    }
    let mut file = File::create(path).map_err(|source| Error::io(path, source))?;
    file.write_all(&body)
        .map_err(|source| Error::io(path, source))?;
    Ok(())
}

fn read_exact(path: &Path, file: &mut File, buf: &mut [u8]) -> Result<(), Error> {
    file.read_exact(buf).map_err(|source| {
        if source.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::format(path, "file ended before the audio data")
        } else {
            Error::io(path, source)
        }
    })
}

fn seek(path: &Path, file: &mut File, pos: u64) -> Result<(), Error> {
    file.seek(SeekFrom::Start(pos))
        .map_err(|source| Error::io(path, source))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn extended_roundtrip_covers_common_rates() {
        for rate in [22_050.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0] {
            let encoded = encode_extended(rate);
            let decoded = decode_extended(&encoded).unwrap();
            assert!((decoded - rate).abs() < 1e-6, "{rate} -> {decoded}");
        }
        let known = [0x40, 0x0E, 0xAC, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(encode_extended(44_100.0), known);
        assert!((decode_extended(&known).unwrap() - 44_100.0).abs() < 1e-6);
    }

    #[test]
    fn hand_built_aiff_reads_half_scale() {
        let dir = env::temp_dir().join(format!("delivered-aiff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one.aiff");
        let mut body = Vec::new();
        body.extend(b"FORM");
        body.extend(48u32.to_be_bytes());
        body.extend(b"AIFF");
        body.extend(b"COMM");
        body.extend(18u32.to_be_bytes());
        body.extend(1i16.to_be_bytes());
        body.extend(1u32.to_be_bytes());
        body.extend(16i16.to_be_bytes());
        body.extend([0x40, 0x0E, 0xAC, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        body.extend(b"SSND");
        body.extend(10u32.to_be_bytes());
        body.extend(0u32.to_be_bytes());
        body.extend(0u32.to_be_bytes());
        body.extend(0x4000i16.to_be_bytes());
        std::fs::write(&path, &body).unwrap();
        let mut reader = AiffReader::open(&path).unwrap();
        assert_eq!(reader.info().sample_rate, 44_100);
        assert_eq!(reader.info().format, "aiff-pcm");
        let packet = reader.read(8).unwrap();
        assert_eq!(packet.frames, 1);
        assert!((packet.pcm[0] - 0.5).abs() < 1e-12);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_roundtrip_peaks_at_half_scale() {
        let dir = env::temp_dir().join(format!("delivered-aiff-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.aiff");
        let mut samples = Vec::new();
        for i in 0..480 {
            let s = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48_000.0).cos() * 0.5;
            samples.push((s * 32_767.0).round() as i16);
        }
        write_aiff_i16(&path, 1, 48_000, &samples).unwrap();
        let mut reader = AiffReader::open(&path).unwrap();
        assert_eq!(reader.info().sample_rate, 48_000);
        let packet = reader.read(480).unwrap();
        let peak = packet.pcm.iter().map(|s| s.abs()).fold(0.0, f64::max);
        assert!((peak - 0.5).abs() < 1e-4, "{peak}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
