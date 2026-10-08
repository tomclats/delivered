//! Text table, printable HTML, and canonical JSON.
//!
//! The JSON writer is hand rolled so every number has a fixed width and the
//! keys stay in one order. Two runs on the same bytes match, including the
//! trailing newline. There is no clock in the receipt.

use crate::session::{Delivery, FileReport, SessionReport};

pub fn render_table(delivery: &Delivery) -> String {
    let headers = [
        "file", "ch", "rate", "peak", "lufs", "clips", "dc", "start", "corr",
    ];
    let mut rows: Vec<Vec<String>> = Vec::new();
    for file in &delivery.files {
        rows.push(file_cells(file));
    }
    rows.push(session_cells(&delivery.session));
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let left = [true, false, false, false, false, false, false, false, false];
    let mut out = String::new();
    let n = delivery.files.len();
    let word = if n == 1 { "file" } else { "files" };
    out.push_str(&format!(
        "{}   {n} {word}   {} Hz   {} s\n\n",
        delivery.label,
        delivery.session.sample_rate,
        fmt_plain(delivery.session.duration_seconds, 3),
    ));
    out.push_str(&join_row(&header_strings(&headers), &widths, &left));
    out.push('\n');
    for row in &rows {
        out.push_str(&join_row(row, &widths, &left));
        out.push('\n');
    }
    if delivery.session.notes.is_empty() {
        out.push_str("\nNo delivery notes.\n");
    } else {
        let count = delivery.session.notes.len();
        let word = if count == 1 { "note" } else { "notes" };
        out.push_str(&format!("\n{count} {word}\n"));
        for note in &delivery.session.notes {
            out.push_str("  ");
            out.push_str(note);
            out.push('\n');
        }
    }
    out.push_str(
        "\nBS.1770-4 integrated loudness. Sample peak, not true peak. Silence at -60 dBFS.\n",
    );
    out.push_str("SESSION start is the spread between the earliest and latest stem.\n");
    out
}

pub fn render_html(delivery: &Delivery) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>Delivery · ");
    out.push_str(&escape(&delivery.label));
    out.push_str("</title>\n<style>\n");
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n<body>\n<main>\n");
    out.push_str("<p class=\"kicker\">Delivered</p>\n<h1>");
    out.push_str(&escape(&delivery.label));
    out.push_str("</h1>\n<p class=\"meta\">");
    let n = delivery.files.len();
    let word = if n == 1 { "file" } else { "files" };
    out.push_str(&format!(
        "{n} {word} · {} Hz · {} s",
        delivery.session.sample_rate,
        escape(&fmt_plain(delivery.session.duration_seconds, 3)),
    ));
    out.push_str("</p>\n<div class=\"scroll\">\n<table>\n<thead><tr>");
    for header in [
        "file", "ch", "rate", "peak", "lufs", "clips", "dc", "start", "corr",
    ] {
        out.push_str("<th>");
        out.push_str(header);
        out.push_str("</th>");
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    let min_start = delivery
        .files
        .iter()
        .filter_map(|file| file.first_audible_sample)
        .min();
    for file in &delivery.files {
        out.push_str("<tr>");
        push_cells(
            &mut out,
            &file_cells(file),
            &file_flags(file, min_start, &delivery.rules),
        );
        out.push_str("</tr>\n");
    }
    out.push_str("<tr class=\"session\">");
    push_cells(
        &mut out,
        &session_cells(&delivery.session),
        &session_flags(&delivery.session, &delivery.rules),
    );
    out.push_str("</tr>\n</tbody>\n</table>\n</div>\n");
    out.push_str("<h2>Notes</h2>\n");
    if delivery.session.notes.is_empty() {
        out.push_str("<p>No delivery notes.</p>\n");
    } else {
        out.push_str("<ol>\n");
        for note in &delivery.session.notes {
            out.push_str("<li>");
            out.push_str(&escape(note));
            out.push_str("</li>\n");
        }
        out.push_str("</ol>\n");
    }
    out.push_str("<h2>Checksums</h2>\n<div class=\"scroll\">\n<table class=\"hashes\">\n");
    out.push_str("<thead><tr><th>file</th><th>format</th><th>sha256</th></tr></thead>\n<tbody>\n");
    for file in &delivery.files {
        out.push_str("<tr><td>");
        out.push_str(&escape(&file.path));
        out.push_str("</td><td>");
        out.push_str(&escape(&format!(
            "{} · {}-bit",
            file.format, file.bits_per_sample
        )));
        out.push_str("</td><td class=\"hash\">");
        out.push_str(&file.sha256);
        out.push_str("</td></tr>\n");
    }
    out.push_str("</tbody>\n</table>\n</div>\n<footer>\n");
    out.push_str(
        "ITU-R BS.1770-4 integrated loudness, mono and stereo. Sample peak, not true peak. ",
    );
    out.push_str("Silence is -60 dBFS. A mono stem in a stereo session is copied to both channels with no pan law. ");
    out.push_str("The raw sum has no faders. SESSION start is the spread between the earliest and latest stem.\n");
    out.push_str("</footer>\n</main>\n</body>\n</html>\n");
    out
}

pub fn render_json(delivery: &Delivery) -> String {
    let mut out = String::from("{");
    {
        let mut top = Obj::new(&mut out, 1);
        top.field_u64("version", delivery.version as u64);
        top.field_raw("rules", &rules_json(&delivery.rules));
        top.field_raw("files", &files_json(&delivery.files));
        top.field_raw("session", &session_json(&delivery.session));
    }
    out.push_str("\n}\n");
    out
}

fn rules_json(rules: &crate::measure::Rules) -> String {
    let mut out = String::from("{");
    {
        let mut obj = Obj::new(&mut out, 2);
        obj.field_str("loudness", "BS.1770-4 integrated");
        obj.field_str("peak", "sample peak");
        obj.field_str("channels", "mono and stereo");
        obj.field_raw("silence_dbfs", &fmt_plain(rules.silence_dbfs, 1));
        obj.field_raw("dc_flag_abs", &fmt_plain(rules.dc_flag_abs, 3));
        obj.field_u64("alignment_flag_samples", rules.alignment_flag_samples);
        obj.field_raw(
            "correlation_flag_below",
            &fmt_plain(rules.correlation_flag_below, 1),
        );
    }
    out.push_str("\n  }");
    out
}

fn files_json(files: &[FileReport]) -> String {
    if files.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[");
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    ");
        out.push_str(&file_json(file));
    }
    out.push_str("\n  ]");
    out
}

fn file_json(file: &FileReport) -> String {
    let mut out = String::from("{");
    {
        let mut obj = Obj::new(&mut out, 3);
        obj.field_str("path", &file.path);
        obj.field_str("sha256", &file.sha256);
        obj.field_str("format", &file.format);
        obj.field_u64("channels", file.channels as u64);
        obj.field_u64("sample_rate", file.sample_rate as u64);
        obj.field_u64("bits_per_sample", file.bits_per_sample as u64);
        obj.field_u64("frames", file.frames);
        obj.field_raw("duration_seconds", &fmt_plain(file.duration_seconds, 3));
        obj.field_raw("sample_peak", &fmt_plain(file.sample_peak, 6));
        obj.field_opt("sample_peak_dbfs", file.sample_peak_dbfs, 2);
        obj.field_u64("clipped_samples", file.clipped_samples);
        obj.field_raw("dc_offset", &dc_json(&file.dc_offset));
        obj.field_u64("leading_silence_samples", file.leading_silence_samples);
        obj.field_u64("trailing_silence_samples", file.trailing_silence_samples);
        obj.field_opt_u64("first_audible_sample", file.first_audible_sample);
        obj.field_opt("integrated_lufs", file.integrated_lufs, 2);
        obj.field_opt("correlation", file.correlation, 4);
    }
    out.push_str("\n    }");
    out
}

fn session_json(session: &SessionReport) -> String {
    let mut out = String::from("{");
    {
        let mut obj = Obj::new(&mut out, 2);
        obj.field_u64("channels", session.channels as u64);
        obj.field_u64("sample_rate", session.sample_rate as u64);
        obj.field_u64("frames", session.frames);
        obj.field_raw("duration_seconds", &fmt_plain(session.duration_seconds, 3));
        obj.field_raw("sample_peak", &fmt_plain(session.sample_peak, 6));
        obj.field_opt("sample_peak_dbfs", session.sample_peak_dbfs, 2);
        obj.field_u64("clipped_samples", session.clipped_samples);
        obj.field_raw("dc_offset", &dc_json(&session.dc_offset));
        obj.field_u64("alignment_spread_samples", session.alignment_spread_samples);
        obj.field_opt("integrated_lufs", session.integrated_lufs, 2);
        obj.field_opt("correlation", session.correlation, 4);
        obj.field_raw("notes", &notes_json(&session.notes));
    }
    out.push_str("\n  }");
    out
}

fn dc_json(values: &[f64]) -> String {
    let body = values
        .iter()
        .map(|value| fmt_plain(*value, 6))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{body}]")
}

fn notes_json(notes: &[String]) -> String {
    if notes.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[");
    for (index, note) in notes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n      ");
        out.push_str(&json_string(note));
    }
    out.push_str("\n    ]");
    out
}

struct Obj<'a> {
    out: &'a mut String,
    indent: usize,
    first: bool,
}

impl<'a> Obj<'a> {
    fn new(out: &'a mut String, indent: usize) -> Self {
        Self {
            out,
            indent,
            first: true,
        }
    }

    fn field_raw(&mut self, key: &str, raw: &str) {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
        self.out.push('\n');
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(&json_string(key));
        self.out.push_str(": ");
        self.out.push_str(raw);
    }

    fn field_str(&mut self, key: &str, value: &str) {
        self.field_raw(key, &json_string(value));
    }

    fn field_u64(&mut self, key: &str, value: u64) {
        self.field_raw(key, &value.to_string());
    }

    fn field_opt(&mut self, key: &str, value: Option<f64>, digits: usize) {
        match value {
            Some(value) => self.field_raw(key, &fmt_plain(value, digits)),
            None => self.field_raw(key, "null"),
        }
    }

    fn field_opt_u64(&mut self, key: &str, value: Option<u64>) {
        match value {
            Some(value) => self.field_u64(key, value),
            None => self.field_raw(key, "null"),
        }
    }
}

fn file_cells(file: &FileReport) -> Vec<String> {
    vec![
        file.path.clone(),
        file.channels.to_string(),
        file.sample_rate.to_string(),
        fmt_db(file.sample_peak_dbfs),
        fmt_lufs(file.integrated_lufs),
        file.clipped_samples.to_string(),
        fmt_dc(&file.dc_offset),
        fmt_start(file.first_audible_sample),
        fmt_corr(file.correlation),
    ]
}

fn session_cells(session: &SessionReport) -> Vec<String> {
    vec![
        "SESSION".to_string(),
        session.channels.to_string(),
        session.sample_rate.to_string(),
        fmt_db(session.sample_peak_dbfs),
        fmt_lufs(session.integrated_lufs),
        session.clipped_samples.to_string(),
        fmt_dc(&session.dc_offset),
        session.alignment_spread_samples.to_string(),
        fmt_corr(session.correlation),
    ]
}

fn file_flags(
    file: &FileReport,
    earliest: Option<u64>,
    rules: &crate::measure::Rules,
) -> Vec<bool> {
    let late = match (file.first_audible_sample, earliest) {
        (Some(start), Some(earliest)) => {
            start.saturating_sub(earliest) > rules.alignment_flag_samples
        }
        _ => false,
    };
    let dc = file
        .dc_offset
        .iter()
        .any(|value| value.abs() >= rules.dc_flag_abs);
    let corr = file
        .correlation
        .is_some_and(|value| value < rules.correlation_flag_below);
    vec![
        false,
        false,
        false,
        false,
        false,
        file.clipped_samples > 0,
        dc,
        late,
        corr,
    ]
}

fn session_flags(session: &SessionReport, rules: &crate::measure::Rules) -> Vec<bool> {
    let dc = session
        .dc_offset
        .iter()
        .any(|value| value.abs() >= rules.dc_flag_abs);
    let corr = session
        .correlation
        .is_some_and(|value| value < rules.correlation_flag_below);
    vec![
        false,
        false,
        false,
        false,
        false,
        session.clipped_samples > 0,
        dc,
        session.alignment_spread_samples > rules.alignment_flag_samples,
        corr,
    ]
}

fn push_cells(out: &mut String, cells: &[String], flags: &[bool]) {
    for (cell, flag) in cells.iter().zip(flags) {
        if *flag {
            out.push_str("<td class=\"bad\">");
        } else {
            out.push_str("<td>");
        }
        out.push_str(&escape(cell));
        out.push_str("</td>");
    }
}

fn header_strings(headers: &[&str]) -> Vec<String> {
    headers.iter().map(|header| (*header).to_string()).collect()
}

fn join_row(cells: &[String], widths: &[usize], left: &[bool]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            if left[index] {
                pad_right(cell, widths[index])
            } else {
                pad_left(cell, widths[index])
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

fn pad_left(text: &str, width: usize) -> String {
    let n = text.chars().count();
    if n >= width {
        text.to_string()
    } else {
        format!("{}{text}", " ".repeat(width - n))
    }
}

fn pad_right(text: &str, width: usize) -> String {
    let n = text.chars().count();
    if n >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - n))
    }
}

fn fmt_plain(value: f64, digits: usize) -> String {
    let mut text = format!("{value:.digits$}");
    if text.starts_with('-') {
        let rest = &text[1..];
        if !rest.is_empty() && rest.chars().all(|c| c == '0' || c == '.') {
            text.remove(0);
        }
    }
    text
}

fn fmt_db(value: Option<f64>) -> String {
    match value {
        Some(value) => {
            let text = fmt_plain(value, 2);
            if text.starts_with('-') {
                text
            } else {
                format!("+{text}")
            }
        }
        None => "-".to_string(),
    }
}

fn fmt_lufs(value: Option<f64>) -> String {
    match value {
        Some(value) => fmt_plain(value, 2),
        None => "silent".to_string(),
    }
}

fn fmt_dc(values: &[f64]) -> String {
    let worst = values
        .iter()
        .copied()
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(0.0);
    let text = fmt_plain(worst, 6);
    if text.starts_with('-') {
        text
    } else {
        format!("+{text}")
    }
}

fn fmt_start(sample: Option<u64>) -> String {
    match sample {
        Some(sample) => sample.to_string(),
        None => "-".to_string(),
    }
}

fn fmt_corr(value: Option<f64>) -> String {
    match value {
        Some(value) => fmt_plain(value, 2),
        None => "-".to_string(),
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const CSS: &str = r#"
:root { color-scheme: light; }
body {
  margin: 0;
  background: #e6e1d6;
  color: #1b1914;
  font-family: "Iowan Old Style", Palatino, "Palatino Linotype", Georgia, serif;
}
main {
  max-width: 920px;
  margin: 32px auto;
  background: #fbf8f2;
  border: 1px solid #1b1914;
  padding: 36px 40px 28px;
}
.kicker {
  margin: 0;
  font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  font-size: 12px;
  letter-spacing: 0.22em;
  text-transform: uppercase;
}
h1 {
  font-weight: 500;
  font-size: 40px;
  line-height: 1.05;
  margin: 8px 0 0;
  letter-spacing: -0.03em;
}
.meta, h2, th, td, footer, ol {
  font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
}
.meta { font-size: 13px; margin: 12px 0 0; }
table { width: 100%; border-collapse: collapse; margin-top: 28px; }
th {
  font-size: 11px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  font-weight: 500;
  text-align: right;
  border-bottom: 1px solid #1b1914;
  padding: 0 8px 8px;
}
td {
  font-size: 13px;
  text-align: right;
  padding: 8px;
  border-bottom: 1px solid #d9d3c7;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}
th:first-child, td:first-child { text-align: left; }
tr.session td { border-bottom: 1px solid #1b1914; font-weight: 600; }
.bad { color: #8f2d2d; }
h2 {
  font-size: 12px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  font-weight: 500;
  margin: 32px 0 8px;
}
ol, p { font-size: 13px; }
ol { margin: 0; padding-left: 18px; }
li { margin: 4px 0; }
.hash { text-align: left; font-size: 12px; word-break: break-all; white-space: normal; }
footer {
  margin-top: 28px;
  font-size: 11px;
  line-height: 1.5;
  color: #5c564c;
}
@media (max-width: 720px) {
  main { margin: 0; border: none; padding: 20px 16px 24px; }
  h1 { font-size: 32px; }
}
@media print {
  body { background: white; }
  main { margin: 0; border: none; max-width: none; }
}
.scroll { overflow-x: auto; }
"#;
