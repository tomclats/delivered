//! Streaming WAV reader. PCM at 16, 24, and 32 bits, 24-bit samples carried
//! in a 32-bit container, and 32-bit float. RF64 is refused.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::Error;

#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub format: &'static str,
    pub frames: u64,
}

pub struct Packet {
    pub pcm: Vec<f64>,
    pub frames: usize,
    pub clipped: u64,
}

pub trait Audio {
    fn info(&self) -> &AudioInfo;
    fn read(&mut self, max_frames: usize) -> Result<Packet, Error>;
}

#[derive(Clone, Copy)]
enum Kind {
    Pcm16,
    Pcm24,
    Pcm24In32,
    Pcm32,
    F32,
}

pub struct WavReader {
    path: PathBuf,
    file: File,
    info: AudioInfo,
    data_left: u64,
    kind: Kind,
    bytes_per_frame: usize,
}

impl WavReader {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut file = File::open(path).map_err(|source| Error::io(path, source))?;
        let len = file
            .metadata()
            .map_err(|source| Error::io(path, source))?
            .len();
        let mut hdr = [0u8; 12];
        read_exact(path, &mut file, &mut hdr)?;
        if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
            return Err(Error::format(path, "not a WAV file"));
        }
        let riff_size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
        if riff_size < 4 {
            return Err(Error::format(path, "RIFF size is too small"));
        }
        let limit = len.min(8 + riff_size);
        let mut pos = 12u64;
        let mut fmt_bytes: Option<Vec<u8>> = None;
        let mut data_at: Option<(u64, u64)> = None;
        while pos + 8 <= limit {
            seek(path, &mut file, pos)?;
            let mut chunk = [0u8; 8];
            read_exact(path, &mut file, &mut chunk)?;
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let body = pos + 8;
            if body + size > len {
                return Err(Error::format(
                    path,
                    "a chunk extends past the end of the file",
                ));
            }
            match &chunk[0..4] {
                b"fmt " => {
                    if fmt_bytes.is_some() {
                        return Err(Error::format(path, "more than one fmt chunk"));
                    }
                    if !(16..=65_536).contains(&size) {
                        return Err(Error::format(path, "fmt chunk has an unexpected size"));
                    }
                    let mut buf = vec![0u8; size as usize];
                    seek(path, &mut file, body)?;
                    read_exact(path, &mut file, &mut buf)?;
                    fmt_bytes = Some(buf);
                }
                b"data" => {
                    if data_at.is_some() {
                        return Err(Error::format(path, "more than one data chunk"));
                    }
                    data_at = Some((body, size));
                }
                _ => {}
            }
            pos = body + size + (size & 1);
        }
        let fmt = fmt_bytes.ok_or_else(|| Error::format(path, "fmt chunk is missing"))?;
        let parsed = parse_fmt(path, &fmt)?;
        let (data_pos, data_size) =
            data_at.ok_or_else(|| Error::format(path, "data chunk is missing"))?;
        if data_size % parsed.bytes_per_frame as u64 != 0 {
            return Err(Error::format(
                path,
                "data chunk is not a whole number of frames",
            ));
        }
        let frames = data_size / parsed.bytes_per_frame as u64;
        seek(path, &mut file, data_pos)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            info: AudioInfo {
                channels: parsed.channels,
                sample_rate: parsed.sample_rate,
                bits_per_sample: parsed.bits,
                format: parsed.format,
                frames,
            },
            data_left: data_size,
            kind: parsed.kind,
            bytes_per_frame: parsed.bytes_per_frame,
        })
    }
}

struct ParsedFmt {
    channels: u16,
    sample_rate: u32,
    bits: u16,
    format: &'static str,
    kind: Kind,
    bytes_per_frame: usize,
}

fn parse_fmt(path: &Path, fmt: &[u8]) -> Result<ParsedFmt, Error> {
    let tag = u16::from_le_bytes([fmt[0], fmt[1]]);
    let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
    let sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
    let block_align = u16::from_le_bytes([fmt[12], fmt[13]]);
    let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
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
    let (kind, valid_bits, format) = match tag {
        1 => pcm_kind(path, bits, bits, block_align, channels)?,
        3 => {
            if bits == 32 && block_align == channels * 4 {
                (Kind::F32, 32, "wav-float")
            } else {
                return Err(unsupported(path, tag, bits, block_align));
            }
        }
        0xFFFE => {
            if fmt.len() < 40 {
                return Err(Error::format(path, "extensible fmt chunk is too short"));
            }
            let mut valid = u16::from_le_bytes([fmt[18], fmt[19]]);
            if valid == 0 {
                valid = bits;
            }
            let guid = &fmt[24..40];
            let sub = u16::from_le_bytes([guid[0], guid[1]]);
            let pcm_tail = [
                0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
            ];
            if guid[2..] != pcm_tail {
                return Err(Error::format(
                    path,
                    "extensible subformat is not PCM or float",
                ));
            }
            match sub {
                1 => pcm_kind(path, bits, valid, block_align, channels)?,
                3 if bits == 32 && valid == 32 && block_align == channels * 4 => {
                    (Kind::F32, 32, "wav-float")
                }
                _ => return Err(unsupported(path, tag, bits, block_align)),
            }
        }
        _ => return Err(unsupported(path, tag, bits, block_align)),
    };
    Ok(ParsedFmt {
        channels,
        sample_rate,
        bits: valid_bits,
        format,
        kind,
        bytes_per_frame: block_align as usize,
    })
}

fn pcm_kind(
    path: &Path,
    container_bits: u16,
    valid_bits: u16,
    block_align: u16,
    channels: u16,
) -> Result<(Kind, u16, &'static str), Error> {
    let kind = match (container_bits, valid_bits, block_align) {
        (16, 16, ba) if ba == channels * 2 => Kind::Pcm16,
        (24, 24, ba) if ba == channels * 3 => Kind::Pcm24,
        (32, 24, ba) if ba == channels * 4 => Kind::Pcm24In32,
        (24, 24, ba) if ba == channels * 4 => Kind::Pcm24In32,
        (32, 32, ba) if ba == channels * 4 => Kind::Pcm32,
        _ => {
            return Err(unsupported(path, 1, container_bits, block_align));
        }
    };
    let bits = match kind {
        Kind::Pcm24In32 => 24,
        Kind::Pcm16 => 16,
        Kind::Pcm24 => 24,
        Kind::Pcm32 => 32,
        Kind::F32 => 32,
    };
    Ok((kind, bits, "wav-pcm"))
}

fn unsupported(path: &Path, tag: u16, bits: u16, block_align: u16) -> Error {
    Error::format(
        path,
        format!("unsupported WAV encoding (format {tag}, {bits} bits, block align {block_align})"),
    )
}

impl Audio for WavReader {
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
        decode(
            &self.path,
            self.kind,
            self.info.channels as usize,
            frames,
            &raw,
        )
    }
}

fn decode(
    path: &Path,
    kind: Kind,
    channels: usize,
    frames: usize,
    raw: &[u8],
) -> Result<Packet, Error> {
    let mut pcm = Vec::with_capacity(frames * channels);
    let mut clipped = 0u64;
    let mut i = 0;
    for _ in 0..frames * channels {
        let (sample, clip) = match kind {
            Kind::Pcm16 => {
                let raw_s = i16::from_le_bytes([raw[i], raw[i + 1]]);
                i += 2;
                let clip = raw_s == i16::MAX || raw_s == i16::MIN;
                (raw_s as f64 / 32768.0, clip)
            }
            Kind::Pcm24 => {
                let raw_s = sign_extend_24(raw[i], raw[i + 1], raw[i + 2]);
                i += 3;
                let clip = raw_s == 8_388_607 || raw_s == -8_388_608;
                (raw_s as f64 / 8_388_608.0, clip)
            }
            Kind::Pcm24In32 => {
                let word = i32::from_le_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
                i += 4;
                let raw_s = word >> 8;
                let clip = raw_s == 8_388_607 || raw_s == -8_388_608;
                (raw_s as f64 / 8_388_608.0, clip)
            }
            Kind::Pcm32 => {
                let raw_s = i32::from_le_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
                i += 4;
                let clip = raw_s == i32::MAX || raw_s == i32::MIN;
                (raw_s as f64 / 2_147_483_648.0, clip)
            }
            Kind::F32 => {
                let raw_s = f32::from_le_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
                i += 4;
                if !raw_s.is_finite() {
                    return Err(Error::format(path, "sample is not finite"));
                }
                let sample = raw_s as f64;
                (sample, sample.abs() >= 1.0)
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

fn sign_extend_24(b0: u8, b1: u8, b2: u8) -> i32 {
    let mut v = b0 as i32 | ((b1 as i32) << 8) | ((b2 as i32) << 16);
    if v & 0x0080_0000 != 0 {
        v |= !0x00FF_FFFF;
    }
    v
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

pub fn write_wav_f32(
    path: &Path,
    channels: u16,
    sample_rate: u32,
    interleaved: &[f32],
) -> Result<(), Error> {
    if channels == 0 || channels > 2 {
        return Err(Error::format(path, "WAV writer supports mono and stereo"));
    }
    if interleaved.len() % channels as usize != 0 {
        return Err(Error::format(
            path,
            "frame count does not match the channel count",
        ));
    }
    let data_bytes = interleaved.len() * 4;
    if data_bytes > u32::MAX as usize {
        return Err(Error::format(path, "WAV data does not fit in 32 bits"));
    }
    let block = u32::from(channels) * 4;
    let mut body = Vec::with_capacity(44 + data_bytes);
    body.extend(b"RIFF");
    body.extend(0u32.to_le_bytes());
    body.extend(b"WAVE");
    body.extend(b"fmt ");
    body.extend(16u32.to_le_bytes());
    body.extend(3u16.to_le_bytes());
    body.extend(channels.to_le_bytes());
    body.extend(sample_rate.to_le_bytes());
    body.extend((sample_rate * block).to_le_bytes());
    body.extend((block as u16).to_le_bytes());
    body.extend(32u16.to_le_bytes());
    body.extend(b"data");
    body.extend((data_bytes as u32).to_le_bytes());
    for sample in interleaved {
        body.extend(sample.to_le_bytes());
    }
    let riff_size = (body.len() - 8) as u32;
    body[4..8].copy_from_slice(&riff_size.to_le_bytes());
    let mut file = File::create(path).map_err(|source| Error::io(path, source))?;
    file.write_all(&body)
        .map_err(|source| Error::io(path, source))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn scratch(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("delivered-wav-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_all(path: &Path) -> Packet {
        let mut reader = WavReader::open(path).unwrap();
        let mut pcm = Vec::new();
        let mut clipped = 0;
        let mut frames = 0;
        loop {
            let packet = reader.read(3).unwrap();
            if packet.frames == 0 {
                break;
            }
            frames += packet.frames;
            clipped += packet.clipped;
            pcm.extend(packet.pcm);
        }
        Packet {
            pcm,
            frames,
            clipped,
        }
    }

    #[test]
    fn float_roundtrip_and_small_reads() {
        let dir = scratch("float");
        let path = dir.join("t.wav");
        write_wav_f32(&path, 1, 48_000, &[0.25, -0.5, 1.5]).unwrap();
        let packet = read_all(&path);
        assert_eq!(packet.frames, 3);
        assert_eq!(packet.clipped, 1);
        assert!((packet.pcm[0] - 0.25).abs() < 1e-6);
        assert!((packet.pcm[1] + 0.5).abs() < 1e-6);
        assert!((packet.pcm[2] - 1.5).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pcm16_rail_is_a_clip() {
        let dir = scratch("pcm16");
        let path = dir.join("t.wav");
        let mut body = Vec::new();
        body.extend(b"RIFF");
        body.extend(0u32.to_le_bytes());
        body.extend(b"WAVE");
        body.extend(b"fmt ");
        body.extend(16u32.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(48_000u32.to_le_bytes());
        body.extend((48_000u32 * 2).to_le_bytes());
        body.extend(2u16.to_le_bytes());
        body.extend(16u16.to_le_bytes());
        // A fact chunk sits between fmt and data in files from a lot of DAWs.
        body.extend(b"fact");
        body.extend(4u32.to_le_bytes());
        body.extend(2u32.to_le_bytes());
        body.extend(b"data");
        body.extend(4u32.to_le_bytes());
        body.extend(i16::MAX.to_le_bytes());
        body.extend(0i16.to_le_bytes());
        let riff_size = (body.len() - 8) as u32;
        body[4..8].copy_from_slice(&riff_size.to_le_bytes());
        std::fs::write(&path, &body).unwrap();
        let info_frames = WavReader::open(&path).unwrap().info().frames;
        assert_eq!(info_frames, 2);
        let packet = read_all(&path);
        assert_eq!(packet.clipped, 1);
        assert!((packet.pcm[0] - (i16::MAX as f64 / 32768.0)).abs() < 1e-12);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pcm24_packed_peak() {
        let dir = scratch("pcm24");
        let path = dir.join("t.wav");
        // One frame, sample 2^22 = 0.5 when scaled by 2^23.
        let sample = 4_194_304i32;
        let mut body = Vec::new();
        body.extend(b"RIFF");
        body.extend(0u32.to_le_bytes());
        body.extend(b"WAVE");
        body.extend(b"fmt ");
        body.extend(16u32.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(48_000u32.to_le_bytes());
        body.extend((48_000u32 * 3).to_le_bytes());
        body.extend(3u16.to_le_bytes());
        body.extend(24u16.to_le_bytes());
        body.extend(b"data");
        body.extend(3u32.to_le_bytes());
        body.extend((sample as u32).to_le_bytes()[..3].iter().copied());
        // 3-byte data chunk is padded to an even size. The pad is outside `data`.
        let riff_size = (body.len() - 8 + 1) as u32;
        body[4..8].copy_from_slice(&riff_size.to_le_bytes());
        body.push(0);
        std::fs::write(&path, &body).unwrap();
        let packet = read_all(&path);
        assert_eq!(packet.clipped, 0);
        assert!((packet.pcm[0] - 0.5).abs() < 1e-12, "{}", packet.pcm[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extensible_24_in_32() {
        let dir = scratch("ext");
        let path = dir.join("t.wav");
        let sample = (4_194_304i32) << 8;
        let mut body = Vec::new();
        body.extend(b"RIFF");
        body.extend(0u32.to_le_bytes());
        body.extend(b"WAVE");
        body.extend(b"fmt ");
        body.extend(40u32.to_le_bytes());
        body.extend(0xFFFE_u16.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(48_000u32.to_le_bytes());
        body.extend((48_000u32 * 4).to_le_bytes());
        body.extend(4u16.to_le_bytes());
        body.extend(32u16.to_le_bytes());
        body.extend(22u16.to_le_bytes());
        body.extend(24u16.to_le_bytes());
        body.extend(0x4u32.to_le_bytes());
        body.extend([
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71,
        ]);
        body.extend(b"data");
        body.extend(4u32.to_le_bytes());
        body.extend(sample.to_le_bytes());
        let riff_size = (body.len() - 8) as u32;
        body[4..8].copy_from_slice(&riff_size.to_le_bytes());
        std::fs::write(&path, &body).unwrap();
        let reader = WavReader::open(&path).unwrap();
        assert_eq!(reader.info().bits_per_sample, 24);
        assert_eq!(reader.info().format, "wav-pcm");
        drop(reader);
        let packet = read_all(&path);
        assert!((packet.pcm[0] - 0.5).abs() < 1e-12, "{}", packet.pcm[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_six_channels() {
        let dir = scratch("ch6");
        let path = dir.join("t.wav");
        let mut body = Vec::new();
        body.extend(b"RIFF");
        body.extend(36u32.to_le_bytes());
        body.extend(b"WAVE");
        body.extend(b"fmt ");
        body.extend(16u32.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(6u16.to_le_bytes());
        body.extend(48_000u32.to_le_bytes());
        body.extend((48_000u32 * 12).to_le_bytes());
        body.extend(12u16.to_le_bytes());
        body.extend(16u16.to_le_bytes());
        std::fs::write(&path, &body).unwrap();
        let text = match WavReader::open(&path) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("a 6-channel file should be refused"),
        };
        assert!(text.contains("6 channels"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
