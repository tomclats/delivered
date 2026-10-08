# How Delivered measures a session

Delivered reads every WAV or AIFF file under the path you give it. Hidden names are skipped. Files are ordered by relative path, and that order is the order in the receipt. A single file is a session of one.

The tool never holds a take in memory. It hashes each file in 64 KiB reads, then opens it again and pulls frames in blocks of 4096. Loudness keeps only the current 400 ms of K-weighted samples per channel. A two-hour session uses the same memory as a one-minute one, plus one open file per stem.

## Loudness

Integrated loudness follows ITU-R BS.1770-4 for mono and stereo. Each channel runs through the pre-filter shelf and the RLB high-pass. Coefficients are computed for the file's sample rate, and the 48 kHz pair is pinned against the published figures. Mean square is taken over 400 ms blocks that overlap by 75 percent. The block length is the sample rate times 0.4, rounded down to a whole number of samples. Blocks whose loudness is below −70 LUFS are dropped. A second pass drops blocks quieter than 10 LU below the loudness of what remains. The printed number is −0.691 + 10 log10 of the mean energy of the blocks that survive.

A file shorter than one 400 ms block becomes a single ungated block. The tail of a longer file, when it is shorter than 400 ms, is left out. That is the incomplete gating block the standard leaves out.

A steady 1 kHz stereo sine whose peak is −23 dBFS measures −23.0 LUFS, within a tenth of a loudness unit. That check is a crate test and a committed fixture, and CI fails when it moves. The same sine on one channel measures about −26.0 LUFS. A mono signal has half the weighted energy of the same sine played on both channels. Left, right, and the single mono channel all use weight 1.

There is no surround layout, no dialogue gate, and no true-peak oversampling. The peak column is the largest absolute sample after integer audio is scaled so that full scale is 1. A positive 16-bit rail is 32767/32768, which is just under 1, and it still counts as a clip.

## The sum

Every stem has to share a sample rate. Rates outside 8000–384000 Hz are refused. The session is the sample-aligned sum with no fader, trim, or delay. If every stem is mono, the sum is mono. If any stem is stereo, the sum is stereo, and each mono stem is copied to both channels. There is no pan law: copying is the "heard from both speakers" level, about 3 LU above a single mono channel. Samples in the sum at or beyond full scale are counted on their own line. That count is about the mix, not about a clipped file. A short stem contributes silence after it ends. It is not padded inside its own report.

## Clips, silence, DC, correlation

An integer sample is clipped on a rail: the 16-bit, 24-bit, or 32-bit extremes. 24-bit audio packed in a 32-bit WAV container is read from the high 24 bits. A float sample is clipped when its absolute value is at least 1. Values past full scale still count, and a non-finite float sample stops the file.

Silence is −60 dBFS, linear 0.001. A sample at that level counts as audible. Leading silence is the prefix before any channel reaches it. Trailing silence is the suffix after the last such sample. Alignment compares the first audible sample of each stem. The sheet names the earliest and latest stem when those indexes differ by more than 8 samples. This is not a cross-correlation. A quiet intro is silence until it crosses the threshold.

DC is the mean of each channel. The sheet flags a mean at or above 0.001 in absolute value. Stereo correlation is the uncentered coefficient, the sum of L·R divided by the square root of the two energies. Phase meters use that form. It is empty for mono. A value below 0.50 is flagged.

## The receipt

`receipt` writes canonical JSON by hand, not through a serializer, so the widths stay fixed. Keys stay in one order, paths use `/`, and numbers use a fixed number of digits. There is no timestamp, so two runs on the same bytes match. The HTML sheet is for a person and can be printed. `--strict` still writes the sheet, then exits 2 when any note was raised, so a delivery script can fail a take that clips or starts late.

## What this refuses

No sample-rate conversion. No plug-in processing. No loudness normalization. No RF64, no float AIFF, no channel count above two. Checksums cover the file bytes, not a decoded interpretation. A delivery sheet that quietly resampled a stem would be worse than one that stops and names the file.

WAV PCM at 16, 24, and 32 bits and WAV 32-bit float are read, including WAVEFORMATEXTENSIBLE PCM and float. AIFF PCM at 16 and 24 bits is read. AIFC is refused with an export instruction. Unknown chunks (`fact`, `bext`, `LIST`) are skipped. A chunk that claims to extend past the file is an error, and a short read is an error. The frame count in the report has to match the header.
