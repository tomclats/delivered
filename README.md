# Delivered

Point it at a folder of WAV or AIFF stems. It writes a one-page delivery sheet: loudness, sample peak, clipped samples, DC offset, silence, whether the stems start together, stereo correlation, and a checksum of every file.

```
cargo run --release -- scan fixtures/session
```

```
session   2 files   48000 Hz   1.000 s

file        ch   rate    peak    lufs  clips         dc  start  corr
guitar.wav   1  48000   +1.94  -21.03      1  +0.000024      0     -
room.wav     1  48000  -18.00  -21.11      0  +0.000019   2000     -
SESSION      1  48000   +1.94  -21.03      1  +0.000043   2000     -

3 notes
  guitar.wav has 1 clipped sample
  room.wav starts 2000 samples after guitar.wav
  raw sum exceeds full scale on 1 sample

BS.1770-4 integrated loudness. Sample peak, not true peak. Silence at -60 dBFS.
SESSION start is the spread between the earliest and latest stem.
```

`fixtures/sine_stereo_m23.wav` is a 1 kHz stereo sine at −23 dBFS peak. It measures −23.04 LUFS.

## Commands

```
delivered scan ./session
delivered sheet ./session -o delivery.html
delivered receipt ./session
delivered receipt ./session -o receipt.json
delivered scan ./session --strict
```

`scan` prints the table. `sheet` writes one HTML file a client can open or print. `receipt` writes canonical JSON to stdout, or to `-o`. The same folder in, the same JSON out: there is no timestamp in the receipt.

Exit 0 means the session was measured. Exit 1 means it could not be read. Exit 2 is `--strict` with at least one note.

Build from a checkout:

```
cargo build --release
./target/release/delivered scan fixtures/session
```

Regenerate the fixtures, which the test compares byte for byte:

```
cargo run --bin make-fixtures
```

## What each number means

| Column | Meaning |
| --- | --- |
| peak | Sample peak in dBFS. Not true peak. A `+` value is past full scale. |
| lufs | ITU-R BS.1770-4 integrated loudness. `silent` when nothing is above the gate. |
| clips | Integer samples on a rail, or float samples at or past full scale. |
| dc | Mean of the worst channel. Flagged at an absolute value of 0.001. |
| start | First sample at or above −60 dBFS. On the SESSION row, the spread between stems. |
| corr | Uncentered L/R correlation. Empty for mono. Flagged below 0.50. |

The SESSION row is the sample-aligned sum. No faders. A mono stem inside a stereo session is copied to both channels, with no pan law. Stems that do not share a sample rate are an error, and the message names the files.

## Will not do

- Mastering, loudness normalization, or stem separation
- Accounts, uploads, or a clock inside the receipt
- True-peak oversampling
- Sample-rate conversion
- Plug-ins
- RF64, float AIFF, or more than two channels
- Cross-correlation alignment. A quiet intro stays silent until −60 dBFS

The measurement, the gates, and the reason for each refusal are in [DESIGN.md](DESIGN.md).

## Tests

```
cargo test --locked
```

A 1 kHz stereo sine at −23 dBFS peak has to measure −23.0 LUFS within 0.1 LU, both in memory and from `fixtures/sine_stereo_m23.wav`. The same sine at 44.1 kHz is checked. So are the absolute and relative gates, 16-bit and 24-bit WAV, 24-bit samples in a 32-bit container, AIFF, a late stem, a DC offset, mixed sample rates, and a receipt that parses as JSON and does not change between runs.

GitHub Actions runs `cargo test` and `cargo clippy --all-targets -- -D warnings` on every push.

## Layout

```
src/wav.rs        streaming WAV decoder
src/aiff.rs       streaming AIFF decoder
src/measure.rs    K-weighting, gating, peaks, silence
src/session.rs    checksums, the sum, the notes
src/render.rs     table, HTML, JSON
fixtures/         generated stems the tests lock
```

## License

MIT.
