//! ITU-R BS.1770-4 integrated loudness for mono and stereo, plus the
//! sample-domain checks the sheet prints beside it.
//!
//! Each channel passes through the K-weighting shelf and the RLB high-pass.
//! Energy is gathered in 400 ms blocks with a 100 ms hop. An absolute gate
//! drops blocks below −70 LUFS. A relative gate then drops blocks more than
//! 10 LU below the loudness of what remains. A file shorter than one block
//! is reported from that single block with no gate. A longer file drops its
//! trailing partial block.

use crate::error::Error;

pub const SILENCE_DBFS: f64 = -60.0;
pub const DC_FLAG_ABS: f64 = 0.001;
pub const ALIGNMENT_FLAG_SAMPLES: u64 = 8;
pub const CORRELATION_FLAG_BELOW: f64 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    pub silence_dbfs: f64,
    pub dc_flag_abs: f64,
    pub alignment_flag_samples: u64,
    pub correlation_flag_below: f64,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            silence_dbfs: SILENCE_DBFS,
            dc_flag_abs: DC_FLAG_ABS,
            alignment_flag_samples: ALIGNMENT_FLAG_SAMPLES,
            correlation_flag_below: CORRELATION_FLAG_BELOW,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Measured {
    pub frames: u64,
    pub sample_peak: f64,
    pub clipped_samples: u64,
    pub dc_offset: Vec<f64>,
    pub leading_silence_samples: u64,
    pub trailing_silence_samples: u64,
    pub first_audible_sample: Option<u64>,
    pub integrated_lufs: Option<f64>,
    pub correlation: Option<f64>,
}

#[derive(Clone)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

fn shelf(fs: f64) -> Biquad {
    let f0 = 1681.974450955533;
    let gain = 3.999843853973347;
    let q = 0.7071752369554196;
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(gain / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    Biquad {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
        z1: 0.0,
        z2: 0.0,
    }
}

fn highpass(fs: f64) -> Biquad {
    let f0 = 38.13547087602444;
    let q = 0.5003270373238773;
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    Biquad {
        b0: 1.0 / a0,
        b1: -2.0 / a0,
        b2: 1.0 / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
        z1: 0.0,
        z2: 0.0,
    }
}

pub struct Meter {
    channels: usize,
    block: usize,
    hop: usize,
    filled: usize,
    since_hop: usize,
    pos: usize,
    count_full_scale: bool,
    silence: f64,
    frames: u64,
    peak: f64,
    clips: u64,
    dc_sum: Vec<f64>,
    sum_lr: f64,
    sum_l2: f64,
    sum_r2: f64,
    lead: Option<u64>,
    last_audible: Option<u64>,
    buf: Vec<Vec<f64>>,
    filters: Vec<[Biquad; 2]>,
    block_powers: Vec<f64>,
    ungated: bool,
}

impl Meter {
    pub fn new(channels: usize, sample_rate: u32, count_full_scale: bool) -> Result<Self, Error> {
        if !(1..=2).contains(&channels) {
            return Err(Error::Session(
                "delivered measures mono and stereo".to_string(),
            ));
        }
        if !(8_000..=384_000).contains(&sample_rate) {
            return Err(Error::Session(format!(
                "sample rate {sample_rate} Hz is outside 8000-384000"
            )));
        }
        let block = sample_rate as usize * 4 / 10;
        let hop = block / 4;
        if hop == 0 {
            return Err(Error::Session(
                "sample rate is too low for a 400 ms gating block".to_string(),
            ));
        }
        let fs = sample_rate as f64;
        let mut filters = Vec::with_capacity(channels);
        let mut buf = Vec::with_capacity(channels);
        for _ in 0..channels {
            filters.push([shelf(fs), highpass(fs)]);
            buf.push(vec![0.0; block]);
        }
        Ok(Self {
            channels,
            block,
            hop,
            filled: 0,
            since_hop: 0,
            pos: 0,
            count_full_scale,
            silence: 10f64.powf(SILENCE_DBFS / 20.0),
            frames: 0,
            peak: 0.0,
            clips: 0,
            dc_sum: vec![0.0; channels],
            sum_lr: 0.0,
            sum_l2: 0.0,
            sum_r2: 0.0,
            lead: None,
            last_audible: None,
            buf,
            filters,
            block_powers: Vec::new(),
            ungated: false,
        })
    }

    pub fn push(&mut self, frame: &[f64]) {
        assert_eq!(frame.len(), self.channels);
        let index = self.frames;
        self.frames += 1;
        let mut audible = false;
        for (ch, &x) in frame.iter().enumerate() {
            let ax = x.abs();
            if ax > self.peak {
                self.peak = ax;
            }
            if self.count_full_scale && ax >= 1.0 {
                self.clips += 1;
            }
            self.dc_sum[ch] += x;
            if ax >= self.silence {
                audible = true;
            }
            let y = self.filters[ch][0].process(x);
            let y = self.filters[ch][1].process(y);
            self.buf[ch][self.pos] = y;
        }
        if self.channels == 2 {
            self.sum_lr += frame[0] * frame[1];
            self.sum_l2 += frame[0] * frame[0];
            self.sum_r2 += frame[1] * frame[1];
        }
        if audible {
            if self.lead.is_none() {
                self.lead = Some(index);
            }
            self.last_audible = Some(index);
        }
        self.pos += 1;
        if self.pos == self.block {
            self.pos = 0;
        }
        if self.filled < self.block {
            self.filled += 1;
            if self.filled == self.block {
                self.emit_block(self.block);
                self.since_hop = 0;
            }
        } else {
            self.since_hop += 1;
            if self.since_hop == self.hop {
                self.emit_block(self.block);
                self.since_hop = 0;
            }
        }
    }

    fn emit_block(&mut self, n: usize) {
        let mut power = 0.0;
        for ch in 0..self.channels {
            // Left, right, and the single mono channel all use weight 1.
            let sum_sq: f64 = self.buf[ch][..n].iter().map(|y| y * y).sum();
            power += sum_sq / n as f64;
        }
        self.block_powers.push(power);
    }

    pub fn finish(mut self) -> Measured {
        if self.frames > 0 && self.block_powers.is_empty() {
            self.emit_block(self.filled);
            self.ungated = true;
        }
        let frames = self.frames;
        let dc_offset = if frames == 0 {
            vec![0.0; self.channels]
        } else {
            self.dc_sum.iter().map(|s| s / frames as f64).collect()
        };
        let (leading, trailing, first) = if frames == 0 {
            (0, 0, None)
        } else if let Some(start) = self.lead {
            let end = self.last_audible.expect("audible end");
            (start, frames - 1 - end, Some(start))
        } else {
            (frames, frames, None)
        };
        Measured {
            frames,
            sample_peak: self.peak,
            clipped_samples: self.clips,
            dc_offset,
            leading_silence_samples: leading,
            trailing_silence_samples: trailing,
            first_audible_sample: first,
            integrated_lufs: self.loudness(),
            correlation: self.correlation(),
        }
    }

    fn loudness(&self) -> Option<f64> {
        if self.block_powers.is_empty() {
            return None;
        }
        if self.ungated {
            return power_to_lufs(self.block_powers[0]);
        }
        integrated(&self.block_powers)
    }

    fn correlation(&self) -> Option<f64> {
        if self.channels != 2 {
            return None;
        }
        let den = (self.sum_l2 * self.sum_r2).sqrt();
        if !den.is_finite() || den <= 1e-20 {
            return None;
        }
        Some((self.sum_lr / den).clamp(-1.0, 1.0))
    }
}

fn power_to_lufs(power: f64) -> Option<f64> {
    if !power.is_finite() || power <= 1e-20 {
        None
    } else {
        Some(-0.691 + 10.0 * power.log10())
    }
}

fn lufs_to_power(lufs: f64) -> f64 {
    10f64.powf((lufs + 0.691) / 10.0)
}

fn integrated(powers: &[f64]) -> Option<f64> {
    let abs_gate = lufs_to_power(-70.0);
    let above: Vec<f64> = powers.iter().copied().filter(|p| *p >= abs_gate).collect();
    if above.is_empty() {
        return None;
    }
    let mean = above.iter().sum::<f64>() / above.len() as f64;
    let relative = power_to_lufs(mean)? - 10.0;
    let rel_gate = lufs_to_power(relative);
    let gated: Vec<f64> = above.into_iter().filter(|p| *p >= rel_gate).collect();
    if gated.is_empty() {
        return None;
    }
    let mean = gated.iter().sum::<f64>() / gated.len() as f64;
    power_to_lufs(mean)
}

#[cfg(test)]
pub fn measure_samples(
    channels: usize,
    sample_rate: u32,
    interleaved: &[f64],
) -> Result<Measured, Error> {
    let mut meter = Meter::new(channels, sample_rate, true)?;
    let mut offset = 0;
    while offset + channels <= interleaved.len() {
        meter.push(&interleaved[offset..offset + channels]);
        offset += channels;
    }
    Ok(meter.finish())
}

#[cfg(test)]
fn cosine(frames: usize, channels: usize, rate: u32, freq: f64, amp: f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let s = (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).cos() * amp;
        for _ in 0..channels {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelf_matches_the_published_48k_coefficients() {
        let s = shelf(48_000.0);
        assert!((s.b0 - 1.53512485958697).abs() < 1e-12);
        assert!((s.b1 + 2.69169618940638).abs() < 1e-12);
        assert!((s.b2 - 1.19839281085285).abs() < 1e-12);
        assert!((s.a1 + 1.69065929318241).abs() < 1e-12);
        assert!((s.a2 - 0.73248077421585).abs() < 1e-12);
        let h = highpass(48_000.0);
        assert!((h.b0 - 0.9950299263000474).abs() < 1e-12);
        assert!((h.a1 + 1.99004745483398).abs() < 1e-12);
        assert!((h.a2 - 0.99007225036621).abs() < 1e-12);
    }

    #[test]
    fn stereo_1khz_at_minus_23_dbfs_is_minus_23_lufs() {
        let amp = 10f64.powf(-23.0 / 20.0);
        let tone = cosine(48_000 * 3, 2, 48_000, 1_000.0, amp);
        let measured = measure_samples(2, 48_000, &tone).unwrap();
        let lufs = measured.integrated_lufs.expect("loudness");
        assert!(
            (lufs + 23.0).abs() < 0.1,
            "stereo 1 kHz at -23 dBFS peak measured {lufs} LUFS"
        );
    }

    #[test]
    fn mono_1khz_is_about_three_lu_quieter_than_stereo() {
        let amp = 10f64.powf(-23.0 / 20.0);
        let tone = cosine(48_000 * 3, 1, 48_000, 1_000.0, amp);
        let measured = measure_samples(1, 48_000, &tone).unwrap();
        let lufs = measured.integrated_lufs.expect("loudness");
        assert!(
            (lufs + 26.01).abs() < 0.1,
            "mono 1 kHz at -23 dBFS peak measured {lufs} LUFS"
        );
    }

    #[test]
    fn same_sine_holds_at_44_1k() {
        let amp = 10f64.powf(-23.0 / 20.0);
        let frames = 44_100 * 3;
        let tone = cosine(frames, 2, 44_100, 1_000.0, amp);
        let measured = measure_samples(2, 44_100, &tone).unwrap();
        let lufs = measured.integrated_lufs.expect("loudness");
        assert!(
            (lufs + 23.0).abs() < 0.1,
            "44.1 kHz stereo sine measured {lufs} LUFS"
        );
    }

    #[test]
    fn digital_silence_has_no_loudness() {
        let silence = vec![0.0; 48_000 * 2];
        let measured = measure_samples(1, 48_000, &silence).unwrap();
        assert!(measured.integrated_lufs.is_none());
        assert_eq!(measured.first_audible_sample, None);
        assert_eq!(measured.leading_silence_samples, 48_000 * 2);
    }

    #[test]
    fn dc_is_the_mean_and_is_rejected_by_the_highpass() {
        let dc = vec![0.05; 48_000];
        let measured = measure_samples(1, 48_000, &dc).unwrap();
        assert!((measured.dc_offset[0] - 0.05).abs() < 1e-12);
        let lufs = measured.integrated_lufs.unwrap_or(f64::NEG_INFINITY);
        assert!(lufs < -40.0, "DC measured {lufs} LUFS");
    }

    #[test]
    fn full_scale_samples_count_as_clips() {
        let mut tone = cosine(48_000, 1, 48_000, 440.0, 0.2);
        for sample in tone.iter_mut().take(14).skip(10) {
            *sample = 1.0;
        }
        tone[20] = -1.25;
        let measured = measure_samples(1, 48_000, &tone).unwrap();
        assert_eq!(measured.clipped_samples, 5);
        assert!((measured.sample_peak - 1.25).abs() < 1e-12);
    }

    #[test]
    fn inverted_stereo_correlation_is_minus_one() {
        let mut tone = cosine(48_000, 2, 48_000, 440.0, 0.2);
        for i in (1..tone.len()).step_by(2) {
            tone[i] = -tone[i];
        }
        let measured = measure_samples(2, 48_000, &tone).unwrap();
        let corr = measured.correlation.expect("correlation");
        assert!((corr + 1.0).abs() < 1e-6, "{corr}");
    }

    #[test]
    fn cosine_starts_on_a_peak_so_leading_silence_is_zero() {
        let amp = 10f64.powf(-20.0 / 20.0);
        let tone = cosine(4_800, 1, 48_000, 1_000.0, amp);
        let measured = measure_samples(1, 48_000, &tone).unwrap();
        assert_eq!(measured.first_audible_sample, Some(0));
        assert_eq!(measured.leading_silence_samples, 0);
        assert_eq!(measured.trailing_silence_samples, 0);
    }

    #[test]
    fn absolute_gate_drops_a_block_below_minus_70() {
        let loud = lufs_to_power(-20.0);
        let buried = lufs_to_power(-90.0);
        let mut powers = vec![loud; 10];
        powers.push(buried);
        let got = integrated(&powers).unwrap();
        assert!((got + 20.0).abs() < 1e-9, "{got}");
    }

    #[test]
    fn relative_gate_drops_blocks_quieter_than_10_lu() {
        let loud = lufs_to_power(-20.0);
        let quiet = lufs_to_power(-50.0);
        let mut powers = vec![loud; 20];
        powers.extend(std::iter::repeat(quiet).take(20));
        let got = integrated(&powers).unwrap();
        assert!((got + 20.0).abs() < 0.05, "{got}");
    }

    #[test]
    fn empty_energy_is_silent() {
        assert!(integrated(&[]).is_none());
        assert!(integrated(&[0.0]).is_none());
    }
}
