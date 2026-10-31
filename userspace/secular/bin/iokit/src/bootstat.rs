// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Print time since boot and related kernel counters.

use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "Usage: bootstat";
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const LABEL_WIDTH: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    boot_nanoseconds: i64,
    monotonic_nanoseconds: i64,
    boot_ticks: i64,
    monotonic_ticks: i64,
    ticks_per_second: i64,
    cpu_count: u32,
    page_size: u32,
    physical_memory_bytes: u64,
    version: String,
}

/// Parses operands after `argv[0]`. `bootstat` takes no options or files.
fn parse_args(args: &[String]) -> Result<(), String> {
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            let flag = arg.chars().nth(1).unwrap_or('-');
            return Err(format!("bootstat: invalid option -- '{flag}'\n{USAGE}"));
        }
        return Err(format!("bootstat: extra operand '{arg}'\n{USAGE}"));
    }
    Ok(())
}

fn format_duration(nanoseconds: i64) -> String {
    let sign = if nanoseconds < 0 { "-" } else { "" };
    let magnitude = nanoseconds.unsigned_abs();
    let seconds = magnitude / NANOS_PER_SECOND as u64;
    let frac = magnitude % NANOS_PER_SECOND as u64;
    format!("{sign}{seconds}.{frac:09}s")
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB && bytes % GIB == 0 {
        format!("{}G", bytes / GIB)
    } else if bytes >= GIB {
        format!("{:.2}G", bytes as f64 / GIB as f64)
    } else if bytes >= MIB && bytes % MIB == 0 {
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

fn write_field<W: Write>(writer: &mut W, label: &str, value: &str) -> io::Result<()> {
    writeln!(writer, "{label:<LABEL_WIDTH$} {value}")
}

fn write_snapshot<W: Write>(mut writer: W, snapshot: &Snapshot) -> io::Result<()> {
    write_field(&mut writer, "boot", &format_duration(snapshot.boot_nanoseconds))?;
    write_field(&mut writer, "monotonic", &format_duration(snapshot.monotonic_nanoseconds))?;
    let suspend = snapshot.boot_nanoseconds.saturating_sub(snapshot.monotonic_nanoseconds);
    write_field(&mut writer, "suspend", &format_duration(suspend))?;
    write_field(&mut writer, "ticks", &snapshot.boot_ticks.to_string())?;
    write_field(&mut writer, "mono-ticks", &snapshot.monotonic_ticks.to_string())?;
    write_field(&mut writer, "ticks/s", &snapshot.ticks_per_second.to_string())?;
    write_field(&mut writer, "cpus", &snapshot.cpu_count.to_string())?;
    write_field(&mut writer, "page-size", &snapshot.page_size.to_string())?;
    write_field(
        &mut writer,
        "physmem",
        &format!(
            "{} ({})",
            snapshot.physical_memory_bytes,
            format_size(snapshot.physical_memory_bytes)
        ),
    )?;
    write_field(&mut writer, "version", &snapshot.version)
}

#[cfg(target_os = "fuchsia")]
fn take_snapshot() -> Snapshot {
    // SAFETY: These vDSO syscalls have no preconditions and return values from the kernel.
    let boot_nanoseconds = unsafe { zx_sys::zx_clock_get_boot() };
    let monotonic_nanoseconds = unsafe { zx_sys::zx_clock_get_monotonic() };
    let boot_ticks = unsafe { zx_sys::zx_ticks_get_boot() };
    let monotonic_ticks = unsafe { zx_sys::zx_ticks_get() };
    let ticks_per_second = unsafe { zx_sys::zx_ticks_per_second() };
    let cpu_count = unsafe { zx_sys::zx_system_get_num_cpus() };
    let page_size = unsafe { zx_sys::zx_system_get_page_size() };
    let physical_memory_bytes = unsafe { zx_sys::zx_system_get_physmem() };
    let version_view = unsafe { zx_sys::zx_system_get_version_string() };

    let version = if version_view.c_str.is_null() {
        String::new()
    } else {
        // SAFETY: The kernel returns a static, NUL-terminated UTF-8 string that
        // lives for the life of the process. `length` is the string byte length
        // excluding the terminator.
        let bytes = unsafe { std::slice::from_raw_parts(version_view.c_str, version_view.length) };
        String::from_utf8_lossy(bytes).into_owned()
    };

    Snapshot {
        boot_nanoseconds,
        monotonic_nanoseconds,
        boot_ticks,
        monotonic_ticks,
        ticks_per_second,
        cpu_count,
        page_size,
        physical_memory_bytes,
        version,
    }
}

#[cfg(not(target_os = "fuchsia"))]
fn take_snapshot() -> Snapshot {
    Snapshot {
        boot_nanoseconds: 0,
        monotonic_nanoseconds: 0,
        boot_ticks: 0,
        monotonic_ticks: 0,
        ticks_per_second: 0,
        cpu_count: 0,
        page_size: 0,
        physical_memory_bytes: 0,
        version: "host".to_string(),
    }
}

fn run(args: &[String]) -> i32 {
    if let Err(message) = parse_args(args) {
        let _ = writeln!(io::stderr(), "{message}");
        return 1;
    }

    let snapshot = take_snapshot();
    let mut stdout = io::stdout().lock();
    if let Err(err) = write_snapshot(&mut stdout, &snapshot) {
        let _ = writeln!(io::stderr(), "bootstat: {err}");
        return 1;
    }
    0
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert!(parse_args(&args).is_ok());
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_operand() {
        let args = ["extra".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn parse_args_double_dash_still_rejects_operand() {
        let args = ["--".to_string(), "-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn format_duration_splits_seconds() {
        assert_eq!(format_duration(1_234_567_890), "1.234567890s");
        assert_eq!(format_duration(-1_000), "-0.000001000s");
        assert_eq!(format_duration(0), "0.000000000s");
    }

    #[test]
    fn format_size_uses_binary_units() {
        assert_eq!(format_size(512), "512");
        assert_eq!(format_size(4096), "4k");
        assert_eq!(format_size(512 * 1024 * 1024), "512M");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3G");
    }

    #[test]
    fn write_snapshot_aligns_fields() {
        let snapshot = Snapshot {
            boot_nanoseconds: 2_000_000_000,
            monotonic_nanoseconds: 1_500_000_000,
            boot_ticks: 48_000_000,
            monotonic_ticks: 36_000_000,
            ticks_per_second: 24_000_000,
            cpu_count: 1,
            page_size: 4096,
            physical_memory_bytes: 512 * 1024 * 1024,
            version: "git:test".to_string(),
        };
        let mut output = Vec::new();
        write_snapshot(&mut output, &snapshot).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "boot         2.000000000s\n\
             monotonic    1.500000000s\n\
             suspend      0.500000000s\n\
             ticks        48000000\n\
             mono-ticks   36000000\n\
             ticks/s      24000000\n\
             cpus         1\n\
             page-size    4096\n\
             physmem      536870912 (512M)\n\
             version      git:test\n"
        );
    }
}
