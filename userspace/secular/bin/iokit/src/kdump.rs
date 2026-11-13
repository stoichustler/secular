// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Summarize ktrace FXT files, defaulting to `/stow/*.fxt`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "Usage: kdump [-v] [file|dir...]";
const DEFAULT_ROOT: &str = "/stow";
const FXT_EXTENSION: &str = "fxt";
const LABEL_WIDTH: usize = 10;
const TYPE_WIDTH: usize = 12;
const CATEGORY_WIDTH: usize = 16;
const NAME_WIDTH: usize = 24;
const COUNT_WIDTH: usize = 8;
const PCT_WIDTH: usize = 6;
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const TOP_EVENTS: usize = 12;

/// 8-byte FXT magic number record (`0x0016547846040010` little-endian).
const FXT_MAGIC: u64 = 0x0016_5478_4604_0010;

const REC_METADATA: u8 = 0;
const REC_INIT: u8 = 1;
const REC_STRING: u8 = 2;
const REC_THREAD: u8 = 3;
const REC_EVENT: u8 = 4;
const REC_BLOB: u8 = 5;
const REC_USER_OBJ: u8 = 6;
const REC_KERNEL_OBJ: u8 = 7;
const REC_SCHEDULER: u8 = 8;
const REC_LOG: u8 = 9;
const REC_PROFILER: u8 = 10;
const REC_LARGE: u8 = 15;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Options {
    verbose: bool,
    paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Summary {
    bytes: u64,
    magic: bool,
    ticks_per_second: Option<u64>,
    min_ticks: Option<u64>,
    max_ticks: Option<u64>,
    records: u64,
    truncated: bool,
    by_type: BTreeMap<u8, u64>,
    events: BTreeMap<(String, String), u64>,
}

fn record_type_name(rec_type: u8) -> &'static str {
    match rec_type {
        REC_METADATA => "metadata",
        REC_INIT => "init",
        REC_STRING => "string",
        REC_THREAD => "thread",
        REC_EVENT => "event",
        REC_BLOB => "blob",
        REC_USER_OBJ => "user-obj",
        REC_KERNEL_OBJ => "kernel-obj",
        REC_SCHEDULER => "scheduler",
        REC_LOG => "log",
        REC_PROFILER => "profiler",
        REC_LARGE => "large",
        _ => "unknown",
    }
}

fn is_fxt_path(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case(FXT_EXTENSION))
}

/// Parses operands after `argv[0]`. `-v` lists every event name, not only the top 12.
fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            for flag in arg.chars().skip(1) {
                if flag != 'v' {
                    return Err(format!("kdump: invalid option -- '{flag}'\n{USAGE}"));
                }
                options.verbose = true;
            }
            continue;
        }
        options.paths.push(arg.clone());
    }
    Ok(options)
}

fn read_u64_le(data: &[u8], offset: usize) -> Option<u64> {
    data.get(offset..offset + 8)?.try_into().ok().map(u64::from_le_bytes)
}

fn record_size_bytes(header: u64) -> Option<usize> {
    let rec_type = (header & 0xf) as u8;
    let words = if rec_type == REC_LARGE { (header >> 4) & 0xffff_ffff } else { (header >> 4) & 0xfff };
    if words == 0 {
        return None;
    }
    usize::try_from(words.saturating_mul(8)).ok()
}

fn string_ref_index(raw: u16) -> Option<u16> {
    if raw == 0 || raw & 0x8000 != 0 {
        None
    } else {
        Some(raw)
    }
}

fn note_ticks(summary: &mut Summary, ticks: u64) {
    summary.min_ticks = Some(summary.min_ticks.map_or(ticks, |min| min.min(ticks)));
    summary.max_ticks = Some(summary.max_ticks.map_or(ticks, |max| max.max(ticks)));
}

fn utf8_padded(bytes: &[u8], len: usize) -> Option<&str> {
    if len > bytes.len() {
        return None;
    }
    std::str::from_utf8(&bytes[..len]).ok()
}

fn ingest_record(
    summary: &mut Summary,
    strings: &mut BTreeMap<u16, String>,
    rec_type: u8,
    header: u64,
    record: &[u8],
) {
    *summary.by_type.entry(rec_type).or_insert(0) += 1;
    summary.records = summary.records.saturating_add(1);
    if header == FXT_MAGIC {
        summary.magic = true;
    }

    match rec_type {
        REC_INIT if record.len() >= 16 => {
            summary.ticks_per_second = read_u64_le(record, 8);
        }
        REC_STRING => {
            let index = ((header >> 16) & 0x7fff) as u16;
            let len = ((header >> 32) & 0x7fff) as usize;
            if index != 0 {
                if let Some(text) = utf8_padded(record.get(8..).unwrap_or(&[]), len) {
                    strings.insert(index, text.to_string());
                }
            }
        }
        REC_EVENT | REC_SCHEDULER if record.len() >= 16 => {
            if let Some(ticks) = read_u64_le(record, 8) {
                note_ticks(summary, ticks);
            }
            if rec_type == REC_EVENT {
                let name_ref = (header >> 48) as u16;
                let category_ref = (header >> 32) as u16;
                let category = string_ref_index(category_ref)
                    .and_then(|id| strings.get(&id).cloned())
                    .unwrap_or_else(|| {
                        string_ref_index(category_ref).map(|id| format!("#{id}")).unwrap_or_default()
                    });
                let name = string_ref_index(name_ref)
                    .and_then(|id| strings.get(&id).cloned())
                    .unwrap_or_else(|| {
                        string_ref_index(name_ref).map(|id| format!("#{id}")).unwrap_or_default()
                    });
                *summary.events.entry((category, name)).or_insert(0) += 1;
            }
        }
        _ => {}
    }
}

fn parse_fxt(bytes: &[u8]) -> Result<Summary, String> {
    if bytes.len() < 8 {
        return Err("too short for an FXT record".into());
    }
    let mut summary = Summary { bytes: bytes.len() as u64, ..Summary::default() };
    let mut strings = BTreeMap::new();
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let Some(header) = read_u64_le(bytes, offset) else {
            break;
        };
        if header == 0 {
            break;
        }
        let rec_type = (header & 0xf) as u8;
        let Some(size) = record_size_bytes(header) else {
            return Err(format!("zero-size record at offset {offset}"));
        };
        if offset.saturating_add(size) > bytes.len() {
            summary.truncated = true;
            break;
        }
        ingest_record(&mut summary, &mut strings, rec_type, header, &bytes[offset..offset + size]);
        offset += size;
    }
    Ok(summary)
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB && bytes % MIB == 0 {
        format!("{}M", bytes / MIB)
    } else if bytes >= MIB {
        format!("{:.2}M", bytes as f64 / MIB as f64)
    } else if bytes >= KIB && bytes % KIB == 0 {
        format!("{}k", bytes / KIB)
    } else if bytes >= KIB {
        format!("{:.2}k", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes}")
    }
}

fn format_duration_ns(nanoseconds: u64) -> String {
    format!("{:.3}s", nanoseconds as f64 / NANOS_PER_SECOND as f64)
}

fn format_pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        return format!("{:>PCT_WIDTH$}", "0.0%");
    }
    format!("{:>PCT_WIDTH$}", format!("{:.1}%", (part as f64) * 100.0 / whole as f64))
}

fn clip(text: &str, width: usize) -> String {
    let n = text.chars().count();
    if n <= width {
        let mut out = text.to_string();
        for _ in n..width {
            out.push(' ');
        }
        return out;
    }
    if width <= 2 {
        return ".".repeat(width);
    }
    let mut out: String = text.chars().take(width - 2).collect();
    out.push_str("..");
    out
}

fn write_count_row<W: Write>(
    writer: &mut W,
    name: &str,
    name_width: usize,
    count: u64,
    whole: u64,
) -> io::Result<()> {
    writeln!(
        writer,
        "  {} {count:>COUNT_WIDTH$} {}",
        clip(name, name_width),
        format_pct(count, whole)
    )
}

fn write_rule<W: Write>(writer: &mut W, widths: &[usize]) -> io::Result<()> {
    write!(writer, "  ")?;
    for (i, width) in widths.iter().enumerate() {
        if i > 0 {
            write!(writer, " ")?;
        }
        write!(writer, "{}", "-".repeat(*width))?;
    }
    writeln!(writer)
}

fn duration_ns(summary: &Summary) -> Option<u64> {
    let ticks_per_second = summary.ticks_per_second?;
    if ticks_per_second == 0 {
        return None;
    }
    let min = summary.min_ticks?;
    let max = summary.max_ticks?;
    let delta = max.saturating_sub(min);
    Some(((delta as u128 * NANOS_PER_SECOND as u128) / ticks_per_second as u128) as u64)
}

fn count_type(summary: &Summary, rec_type: u8) -> u64 {
    summary.by_type.get(&rec_type).copied().unwrap_or(0)
}

fn write_field<W: Write>(writer: &mut W, label: &str, value: &str) -> io::Result<()> {
    writeln!(writer, "  {label:<LABEL_WIDTH$} {value}")
}

fn write_type_table<W: Write>(writer: &mut W, summary: &Summary) -> io::Result<()> {
    writeln!(writer)?;
    writeln!(
        writer,
        "  {:<TYPE_WIDTH$} {:>COUNT_WIDTH$} {:>PCT_WIDTH$}",
        "type", "count", "pct"
    )?;
    write_rule(writer, &[TYPE_WIDTH, COUNT_WIDTH, PCT_WIDTH])?;
    let mut rows: Vec<(u8, u64)> = summary.by_type.iter().map(|(k, v)| (*k, *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (rec_type, count) in rows {
        write_count_row(writer, record_type_name(rec_type), TYPE_WIDTH, count, summary.records)?;
    }
    Ok(())
}

fn write_event_table<W: Write>(writer: &mut W, summary: &Summary, verbose: bool) -> io::Result<()> {
    if summary.events.is_empty() {
        return Ok(());
    }
    let event_total = count_type(summary, REC_EVENT);
    let mut rows: Vec<(&(String, String), u64)> =
        summary.events.iter().map(|(k, v)| (k, *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let shown = if verbose { rows.len() } else { rows.len().min(TOP_EVENTS) };
    let hidden = rows.len().saturating_sub(shown);

    writeln!(writer)?;
    writeln!(
        writer,
        "  {:<CATEGORY_WIDTH$} {:<NAME_WIDTH$} {:>COUNT_WIDTH$} {:>PCT_WIDTH$}",
        "category", "name", "count", "pct"
    )?;
    write_rule(writer, &[CATEGORY_WIDTH, NAME_WIDTH, COUNT_WIDTH, PCT_WIDTH])?;
    for ((category, name), count) in rows.into_iter().take(shown) {
        writeln!(
            writer,
            "  {} {} {count:>COUNT_WIDTH$} {}",
            clip(category, CATEGORY_WIDTH),
            clip(name, NAME_WIDTH),
            format_pct(count, event_total)
        )?;
    }
    if hidden > 0 {
        writeln!(writer, "  ... {hidden} more (kdump -v)")?;
    }
    Ok(())
}

fn write_summary<W: Write>(
    mut writer: W,
    summary: &Summary,
    verbose: bool,
) -> io::Result<()> {
    write_field(
        &mut writer,
        "size",
        &format!("{} ({})", summary.bytes, format_size(summary.bytes)),
    )?;
    write_field(&mut writer, "magic", if summary.magic { "FXT" } else { "no" })?;
    match summary.ticks_per_second {
        Some(rate) => write_field(&mut writer, "ticks/s", &rate.to_string())?,
        None => write_field(&mut writer, "ticks/s", "-")?,
    }
    match duration_ns(summary) {
        Some(ns) => write_field(&mut writer, "duration", &format_duration_ns(ns))?,
        None => write_field(&mut writer, "duration", "-")?,
    }
    write_field(&mut writer, "records", &summary.records.to_string())?;
    if summary.truncated {
        write_field(&mut writer, "truncated", "yes")?;
    }
    write_type_table(&mut writer, summary)?;
    write_event_table(&mut writer, summary, verbose)?;
    Ok(())
}

fn collect_fxt_files(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for path in paths {
        let meta = fs::metadata(path).map_err(|err| format!("kdump: {path}: {err}"))?;
        if meta.is_dir() {
            let entries = fs::read_dir(path).map_err(|err| format!("kdump: {path}: {err}"))?;
            let mut found = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|err| format!("kdump: {path}: {err}"))?;
                let child = entry.path();
                if child.is_file() && is_fxt_path(&child) {
                    found.push(child);
                }
            }
            found.sort();
            files.extend(found);
        } else if meta.is_file() {
            files.push(PathBuf::from(path));
        } else {
            return Err(format!("kdump: {path}: not a file or directory\n{USAGE}"));
        }
    }
    Ok(files)
}

fn load_file(path: &Path) -> Result<Vec<u8>, String> {
    let meta = fs::metadata(path).map_err(|err| err.to_string())?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("larger than {} bytes", MAX_FILE_BYTES));
    }
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|err| err.to_string())?;
    Ok(bytes)
}

fn display_roots(paths: &[String]) -> String {
    if paths.len() == 1 {
        paths[0].clone()
    } else {
        paths.join(", ")
    }
}

fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };
    let roots = if options.paths.is_empty() {
        vec![DEFAULT_ROOT.to_string()]
    } else {
        options.paths.clone()
    };
    let files = match collect_fxt_files(&roots) {
        Ok(files) => files,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };
    if files.is_empty() {
        let _ = writeln!(io::stderr(), "kdump: no *.{FXT_EXTENSION} in {}", display_roots(&roots));
        return 1;
    }

    let mut stdout = io::stdout().lock();
    let mut status = 0;
    for (index, path) in files.iter().enumerate() {
        if index > 0 {
            let _ = writeln!(stdout);
        }
        let display = path.display().to_string();
        match load_file(path).and_then(|bytes| parse_fxt(&bytes)) {
            Ok(summary) => {
                if let Err(err) = write_summary(&mut stdout, &summary, options.verbose) {
                    let _ = writeln!(io::stderr(), "kdump: {err}");
                    return 1;
                }
            }
            Err(err) => {
                let _ = writeln!(io::stderr(), "kdump: {display}: {err}");
                status = 1;
            }
        }
    }
    status
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(rec_type: u8, words: u16) -> u64 {
        u64::from(rec_type) | (u64::from(words) << 4)
    }

    fn string_header(index: u16, len: u16, words: u16) -> u64 {
        header(REC_STRING, words) | (u64::from(index) << 16) | (u64::from(len) << 32)
    }

    fn event_header(category: u16, name: u16, words: u16) -> u64 {
        header(REC_EVENT, words) | (u64::from(category) << 32) | (u64::from(name) << 48)
    }

    fn append_u64(buf: &mut Vec<u8>, value: u64) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    fn sample_trace() -> Vec<u8> {
        let mut bytes = Vec::new();
        append_u64(&mut bytes, FXT_MAGIC);
        append_u64(&mut bytes, header(REC_INIT, 2));
        append_u64(&mut bytes, 1_000_000_000);
        append_u64(&mut bytes, string_header(1, 6, 2));
        bytes.extend_from_slice(b"kernel\0\0");
        append_u64(&mut bytes, string_header(2, 3, 2));
        bytes.extend_from_slice(b"irq\0\0\0\0\0");
        append_u64(&mut bytes, event_header(1, 2, 2));
        append_u64(&mut bytes, 1_000);
        append_u64(&mut bytes, event_header(1, 2, 2));
        append_u64(&mut bytes, 2_000_000_000);
        append_u64(&mut bytes, header(REC_SCHEDULER, 2));
        append_u64(&mut bytes, 1_500_000_000);
        bytes
    }

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert_eq!(parse_args(&args).unwrap(), Options::default());
    }

    #[test]
    fn parse_args_verbose_and_paths() {
        let args = ["-v".to_string(), "/stow".to_string(), "a.fxt".to_string()];
        let options = parse_args(&args).unwrap();
        assert!(options.verbose);
        assert_eq!(options.paths, vec!["/stow".to_string(), "a.fxt".to_string()]);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn is_fxt_path_checks_extension() {
        assert!(is_fxt_path(Path::new("/stow/trace.fxt")));
        assert!(is_fxt_path(Path::new("TRACE.FXT")));
        assert!(!is_fxt_path(Path::new("/stow/trace.bin")));
    }

    #[test]
    fn parse_fxt_summarizes_magic_init_events() {
        let summary = parse_fxt(&sample_trace()).unwrap();
        assert!(summary.magic);
        assert_eq!(summary.ticks_per_second, Some(1_000_000_000));
        assert_eq!(summary.records, 7);
        assert_eq!(count_type(&summary, REC_EVENT), 2);
        assert_eq!(count_type(&summary, REC_SCHEDULER), 1);
        assert_eq!(duration_ns(&summary), Some(1_999_999_000));
        assert_eq!(summary.events.get(&("kernel".into(), "irq".into())), Some(&2));
    }

    #[test]
    fn parse_fxt_stops_at_zero_padding() {
        let mut bytes = sample_trace();
        bytes.extend_from_slice(&[0u8; 16]);
        let summary = parse_fxt(&bytes).unwrap();
        assert_eq!(summary.records, 7);
        assert!(!summary.truncated);
    }

    #[test]
    fn format_pct_right_aligns() {
        assert_eq!(format_pct(0, 0), "  0.0%");
        assert_eq!(format_pct(2, 7), " 28.6%");
        assert_eq!(format_pct(7, 7), "100.0%");
    }

    #[test]
    fn clip_pads_and_truncates() {
        assert_eq!(clip("event", 12), "event       ");
        assert_eq!(clip("abcdefghijklmnop", 12), "abcdefghij..");
        assert_eq!(clip("irq", 12).chars().count(), 12);
    }

    #[test]
    fn write_summary_aligns_type_table() {
        let summary = parse_fxt(&sample_trace()).unwrap();
        let mut out = Vec::new();
        write_summary(&mut out, &summary, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("/stow/trace.fxt\n"));
        assert!(text.contains("magic      FXT"));
        assert!(text.contains("type            count    pct"));
        let event_row = text.lines().find(|line| line.contains("event")).expect("event row");
        assert!(event_row.contains("28.6%"), "{event_row}");
        assert!(text.contains("kernel"));
        assert!(text.contains("irq"));
        assert!(!text.contains("more (kdump -v)"));
    }

    #[test]
    fn write_summary_event_pct_uses_event_total() {
        let summary = parse_fxt(&sample_trace()).unwrap();
        let mut out = Vec::new();
        write_summary(&mut out, &summary, true).unwrap();
        let text = String::from_utf8(out).unwrap();
        let irq_row = text.lines().find(|line| line.contains("irq")).expect("irq row");
        assert!(irq_row.contains("100.0%"), "{irq_row}");
        assert_eq!(clip("kernel", CATEGORY_WIDTH).chars().count(), CATEGORY_WIDTH);
        assert_eq!(clip("irq", NAME_WIDTH).chars().count(), NAME_WIDTH);
    }
}
