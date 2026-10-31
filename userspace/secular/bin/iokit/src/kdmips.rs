// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Estimate integer throughput with Dhrystone 2.1, reported as KDMIPS.
//!
//! This is not the kernel `zmips` command. `zmips` times an empty decrement
//! loop to estimate clock rate. `kdmips` runs the Dhrystone 2.1 synthetic
//! integer mix (pointers, records, strings, arrays, and control flow) and
//! converts the run rate into DMIPS / KDMIPS.
//!
//! Scoring:
//! - The VAX 11/780 does 1757 Dhrystones/second; that rate is defined as 1 DMIPS.
//! - 1 KDMIPS = 1000 DMIPS = 1_757_000 Dhrystones/second.

use std::hint;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "Usage: kdmips [-n runs] [-t millis]";

/// Dhrystones/second of the VAX 11/780. By definition this is 1 DMIPS.
const VAX_DHRYSTONES_PER_SECOND: f64 = 1757.0;

/// User-facing scale: 1 KDMIPS = 1000 DMIPS.
const DMIPS_PER_KDMIPS: f64 = 1000.0;

const NANOS_PER_SECOND: f64 = 1_000_000_000.0;
const NANOS_PER_MILLI: u64 = 1_000_000;

/// Auto-calibration stops once a sample lasts at least this long.
const DEFAULT_TARGET_NANOS: u64 = 1000 * NANOS_PER_MILLI;

const DEFAULT_START_RUNS: u64 = 10_000;
const MAX_RUNS: u64 = 1_000_000_000;
const ARRAY_SIZE: usize = 50;

/// Dhrystone 2.1 `Str_30` (31 bytes including a C NUL).
const STRING_SIZE: usize = 31;

/// Index of `Ptr_Glob` in [`Bench::records`].
const GLOB_INDEX: usize = 0;

/// Index of `Next_Ptr_Glob` in [`Bench::records`].
const NEXT_INDEX: usize = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Options {
    /// Fixed iteration count. `None` means grow until `target_nanos`.
    runs: Option<u64>,
    target_nanos: u64,
}

impl Options {
    fn new() -> Self {
        Self { runs: None, target_nanos: DEFAULT_TARGET_NANOS }
    }
}

/// Dhrystone 2.1 `Enumeration` (`Ident_1` ... `Ident_5`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ident {
    One,
    Two,
    Three,
    Four,
    Five, // Dhrystone 2.1 `Ident_5`; timed path never stores it.
}

/// Dhrystone 2.1 `Rec_Type` with the `var_1` variant fields that the timed
/// path actually uses. `structassign` is a Rust `clone`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    pointer_index: Option<usize>,
    discriminant: Ident,
    enum_comp: Ident,
    int_comp: i32,
    string_comp: [u8; STRING_SIZE],
}

/// Mutable Dhrystone 2.1 globals (`Ptr_Glob`, `Int_Glob`, arrays, ...).
struct Bench {
    records: [Record; 2],
    int_glob: i32,
    bool_glob: bool,
    ch_1_glob: u8,
    ch_2_glob: u8,
    arr_1_glob: [i32; ARRAY_SIZE],
    arr_2_glob: [[i32; ARRAY_SIZE]; ARRAY_SIZE],
}

/// Locals the original `main` loop leaves behind; used for the self-check.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Locals {
    int_1: i32,
    int_2: i32,
    int_3: i32,
    enum_loc: Ident,
    string_1: [u8; STRING_SIZE],
    string_2: [u8; STRING_SIZE],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Scores {
    dhrystones_per_second: f64,
    dmips: f64,
    kdmips: f64,
}

#[derive(Clone, Debug)]
struct Report {
    runs: u64,
    duration_nanos: u64,
    scores: Scores,
}

fn parse_count(value: &str) -> Result<u64, ()> {
    let (digits, radix) =
        if let Some(rest) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
            (rest, 16)
        } else {
            (value, 10)
        };
    u64::from_str_radix(digits, radix).map_err(|_| ())
}

fn take_flag_value(
    flags: &[char],
    flag_index: usize,
    args: &[String],
    index: &mut usize,
    flag: char,
) -> Result<String, String> {
    let attached: String = flags[flag_index + 1..].iter().collect();
    if !attached.is_empty() {
        return Ok(attached);
    }
    if *index >= args.len() {
        return Err(format!("kdmips: option requires an argument -- '{flag}'\n{USAGE}"));
    }
    let value = args[*index].clone();
    *index += 1;
    Ok(value)
}

/// Parses operands after `argv[0]`.
///
/// `-n` sets a fixed Dhrystone iteration count. `-t` sets the auto-calibration
/// target in milliseconds (ignored when `-n` is present).
fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::new();
    let mut options_ended = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            let flags: Vec<char> = arg.chars().skip(1).collect();
            if flags.is_empty() {
                return Err(format!("kdmips: invalid option -- '-'\n{USAGE}"));
            }
            let flag = flags[0];
            match flag {
                'n' | 't' => {
                    let value = take_flag_value(&flags, 0, args, &mut index, flag)?;
                    let count = parse_count(&value)
                        .map_err(|_| format!("kdmips: invalid number '{value}'\n{USAGE}"))?;
                    if flag == 'n' {
                        if count == 0 {
                            return Err(format!("kdmips: runs must be greater than 0\n{USAGE}"));
                        }
                        options.runs = Some(count.min(MAX_RUNS));
                    } else if count == 0 {
                        return Err(format!(
                            "kdmips: target milliseconds must be greater than 0\n{USAGE}"
                        ));
                    } else {
                        options.target_nanos = count.saturating_mul(NANOS_PER_MILLI);
                    }
                }
                _ => return Err(format!("kdmips: invalid option -- '{flag}'\n{USAGE}")),
            }
            continue;
        }
        return Err(format!("kdmips: extra operand '{arg}'\n{USAGE}"));
    }
    Ok(options)
}

fn copy_c_string(destination: &mut [u8; STRING_SIZE], source: &[u8]) {
    destination.fill(0);
    let copied = source.len().min(STRING_SIZE - 1);
    destination[..copied].copy_from_slice(&source[..copied]);
}

fn compare_c_string(left: &[u8; STRING_SIZE], right: &[u8; STRING_SIZE]) -> i32 {
    for index in 0..STRING_SIZE {
        let left_byte = left[index];
        let right_byte = right[index];
        if left_byte != right_byte {
            return i32::from(left_byte) - i32::from(right_byte);
        }
        if left_byte == 0 {
            return 0;
        }
    }
    0
}

fn empty_record() -> Record {
    Record {
        pointer_index: None,
        discriminant: Ident::One,
        enum_comp: Ident::One,
        int_comp: 0,
        string_comp: [0; STRING_SIZE],
    }
}

impl Bench {
    /// Dhrystone 2.1 `main` setup: two records, `Ptr_Glob->Ptr_Comp = Next_Ptr_Glob`.
    fn new() -> Self {
        let mut glob = empty_record();
        let next = empty_record();
        glob.pointer_index = Some(NEXT_INDEX);
        glob.discriminant = Ident::One;
        glob.enum_comp = Ident::Three;
        glob.int_comp = 40;
        copy_c_string(&mut glob.string_comp, b"DHRYSTONE PROGRAM, SOME STRING");

        let mut bench = Self {
            records: [glob, next],
            int_glob: 0,
            bool_glob: false,
            ch_1_glob: 0,
            ch_2_glob: 0,
            arr_1_glob: [0; ARRAY_SIZE],
            arr_2_glob: [[0; ARRAY_SIZE]; ARRAY_SIZE],
        };
        // Prevents overflow diagnostics in the original; incremented once per run.
        bench.arr_2_glob[8][7] = 10;
        // Keep Ident_5 in the binary so the Dhrystone enumeration stays complete.
        hint::black_box(Ident::Five);
        bench
    }

    /// Timed inner loop of Dhrystone 2.1 `main`.
    fn run_iterations(&mut self, runs: u64) -> Locals {
        let mut string_1 = [0u8; STRING_SIZE];
        copy_c_string(&mut string_1, b"DHRYSTONE PROGRAM, 1'ST STRING");
        let mut int_1 = 0i32;
        let mut int_2 = 0i32;
        let mut int_3 = 0i32;
        let mut enum_loc = Ident::One;
        let mut string_2 = [0u8; STRING_SIZE];

        for run_index in 1..=runs {
            self.proc_5();
            self.proc_4();
            int_1 = 2;
            int_2 = 3;
            copy_c_string(&mut string_2, b"DHRYSTONE PROGRAM, 2'ND STRING");
            enum_loc = Ident::Two;
            self.bool_glob = !self.func_2(&string_1, &string_2);
            // Body runs once: Int_1_Loc becomes 3, Int_3_Loc becomes 7.
            while int_1 < int_2 {
                // Original computes this, then Proc_7 overwrites Int_3_Loc.
                let computed = 5 * int_1 - int_2;
                hint::black_box(computed);
                int_3 = proc_7(int_1, int_2);
                int_1 += 1;
            }
            self.proc_8(int_1, int_3);
            self.proc_1(GLOB_INDEX);
            // Ch_2_Glob is 'B', so this body runs for 'A' and 'B'.
            let mut ch_index = b'A';
            while ch_index <= self.ch_2_glob {
                if enum_loc == self.func_1(ch_index, b'C') {
                    enum_loc = self.proc_6(Ident::One);
                    copy_c_string(&mut string_2, b"DHRYSTONE PROGRAM, 3'RD STRING");
                    int_2 = run_index.min(i32::MAX as u64) as i32;
                    self.int_glob = int_2;
                }
                ch_index = ch_index.saturating_add(1);
            }
            int_2 *= int_1;
            int_1 = if int_3 == 0 { 0 } else { int_2 / int_3 };
            int_2 = 7 * (int_2 - int_3) - int_1;
            int_1 = self.proc_2(int_1);
            // Keep the computed locals live so the optimizer cannot drop the mix.
            hint::black_box(int_1);
        }

        Locals { int_1, int_2, int_3, enum_loc, string_1, string_2 }
    }

    /// `Proc_1`: copy `Ptr_Glob` onto `Next_Ptr_Glob`, then chase the record.
    fn proc_1(&mut self, pointer_index: usize) {
        let next_index = match self.records[pointer_index].pointer_index {
            Some(index) if index < self.records.len() => index,
            _ => return,
        };
        self.records[next_index] = self.records[GLOB_INDEX].clone();
        self.records[pointer_index].int_comp = 5;
        self.records[next_index].int_comp = self.records[pointer_index].int_comp;
        self.records[next_index].pointer_index = self.records[pointer_index].pointer_index;
        self.proc_3(next_index);
        if self.records[next_index].discriminant == Ident::One {
            self.records[next_index].int_comp = 6;
            let enum_value = self.records[pointer_index].enum_comp;
            self.records[next_index].enum_comp = self.proc_6(enum_value);
            self.records[next_index].pointer_index = self.records[GLOB_INDEX].pointer_index;
            let int_comp = self.records[next_index].int_comp;
            self.records[next_index].int_comp = proc_7(int_comp, 10);
        } else {
            self.records[pointer_index] = self.records[next_index].clone();
        }
    }

    /// `Proc_2`: executed once; sets `Int_1_Loc` to 5.
    fn proc_2(&self, mut int_par: i32) -> i32 {
        let mut int_loc = int_par + 10;
        let mut enum_loc = Ident::Two;
        while enum_loc != Ident::One {
            if self.ch_1_glob == b'A' {
                int_loc -= 1;
                int_par = int_loc - self.int_glob;
                enum_loc = Ident::One;
            }
        }
        int_par
    }

    /// `Proc_3 (Ptr_Ref_Par)` where `Ptr_Ref_Par` is `&Next_Record->Ptr_Comp`.
    fn proc_3(&mut self, pointer_field_of: usize) {
        self.records[pointer_field_of].pointer_index = self.records[GLOB_INDEX].pointer_index;
        self.records[GLOB_INDEX].int_comp = proc_7(10, self.int_glob);
    }

    /// `Proc_4`.
    fn proc_4(&mut self) {
        let bool_loc = self.ch_1_glob == b'A';
        self.bool_glob = bool_loc || self.bool_glob;
        self.ch_2_glob = b'B';
    }

    /// `Proc_5`.
    fn proc_5(&mut self) {
        self.ch_1_glob = b'A';
        self.bool_glob = false;
    }

    /// `Proc_6`; returns `*Enum_Ref_Par`.
    fn proc_6(&self, enum_val: Ident) -> Ident {
        let mut enum_ref = enum_val;
        if !func_3(enum_val) {
            enum_ref = Ident::Four;
        }
        match enum_val {
            Ident::One => Ident::One,
            Ident::Two => {
                if self.int_glob > 100 {
                    Ident::One
                } else {
                    Ident::Four
                }
            }
            Ident::Three => Ident::Two,
            Ident::Four => enum_ref,
            // Timed path never reaches Ident_5; keep the original switch.
            Ident::Five => Ident::Three,
        }
    }

    /// `Proc_8`.
    fn proc_8(&mut self, int_1: i32, int_2: i32) {
        let int_loc = (int_1 + 5) as usize;
        if int_loc >= ARRAY_SIZE {
            self.int_glob = 5;
            return;
        }
        self.arr_1_glob[int_loc] = int_2;
        if int_loc + 1 < ARRAY_SIZE {
            self.arr_1_glob[int_loc + 1] = self.arr_1_glob[int_loc];
        }
        if int_loc + 30 < ARRAY_SIZE {
            self.arr_1_glob[int_loc + 30] = int_loc as i32;
        }
        for index in int_loc..=int_loc.saturating_add(1) {
            if index < ARRAY_SIZE {
                self.arr_2_glob[int_loc][index] = int_loc as i32;
            }
        }
        if int_loc > 0 {
            self.arr_2_glob[int_loc][int_loc - 1] =
                self.arr_2_glob[int_loc][int_loc - 1].wrapping_add(1);
        }
        if int_loc + 20 < ARRAY_SIZE {
            self.arr_2_glob[int_loc + 20][int_loc] = self.arr_1_glob[int_loc];
        }
        self.int_glob = 5;
    }

    /// `Func_1`.
    fn func_1(&mut self, ch_1: u8, ch_2: u8) -> Ident {
        let ch_1_loc = ch_1;
        let ch_2_loc = ch_1_loc;
        if ch_2_loc != ch_2 {
            Ident::One
        } else {
            self.ch_1_glob = ch_1_loc;
            Ident::Two
        }
    }

    /// `Func_2`.
    fn func_2(&mut self, string_1: &[u8; STRING_SIZE], string_2: &[u8; STRING_SIZE]) -> bool {
        let mut int_loc: i32 = 2;
        let mut ch_loc = 0u8;
        while int_loc <= 2 {
            let left = string_1[int_loc as usize];
            let right = string_2[(int_loc as usize) + 1];
            if self.func_1(left, right) == Ident::One {
                ch_loc = b'A';
                int_loc += 1;
            }
        }
        if (b'W'..=b'Y').contains(&ch_loc) {
            int_loc = 7;
        }
        if ch_loc == b'R' {
            return true;
        }
        if compare_c_string(string_1, string_2) > 0 {
            int_loc += 7;
            self.int_glob = int_loc;
            true
        } else {
            false
        }
    }
}

/// `Proc_7`: `*Int_Par_Ref = Int_2_Par_Val + Int_1_Par_Val + 2`.
fn proc_7(int_1: i32, int_2: i32) -> i32 {
    int_2 + int_1 + 2
}

/// `Func_3`.
fn func_3(enum_val: Ident) -> bool {
    enum_val == Ident::Three
}

/// Convert a timed run into DMIPS and KDMIPS.
///
/// Returns `None` when `duration_nanos` is zero (would divide by zero).
fn scores_from_sample(runs: u64, duration_nanos: u64) -> Option<Scores> {
    if duration_nanos == 0 {
        return None;
    }
    let dhrystones_per_second = (runs as f64) * NANOS_PER_SECOND / (duration_nanos as f64);
    let dmips = dhrystones_per_second / VAX_DHRYSTONES_PER_SECOND;
    let kdmips = dmips / DMIPS_PER_KDMIPS;
    Some(Scores { dhrystones_per_second, dmips, kdmips })
}

fn elapsed_nanos(start: Instant) -> u64 {
    let elapsed = start.elapsed();
    let seconds = elapsed.as_secs().saturating_mul(1_000_000_000);
    seconds.saturating_add(u64::from(elapsed.subsec_nanos()))
}

fn time_runs(runs: u64) -> u64 {
    let mut bench = Bench::new();
    let start = Instant::now();
    let locals = bench.run_iterations(runs);
    hint::black_box((bench.int_glob, locals.int_1));
    elapsed_nanos(start)
}

/// Either use `-n` or double the iteration count until the sample is long enough.
fn measure(options: Options) -> Result<Report, String> {
    let (runs, duration_nanos) = if let Some(runs) = options.runs {
        (runs, time_runs(runs))
    } else {
        let mut runs = DEFAULT_START_RUNS;
        loop {
            let duration_nanos = time_runs(runs);
            if duration_nanos >= options.target_nanos || runs >= MAX_RUNS {
                break (runs, duration_nanos);
            }
            let doubled = runs.saturating_mul(2);
            if doubled == runs {
                break (runs, duration_nanos);
            }
            runs = doubled.min(MAX_RUNS);
        }
    };

    let scores = scores_from_sample(runs, duration_nanos)
        .ok_or_else(|| "kdmips: measurement duration was zero".to_string())?;
    Ok(Report { runs, duration_nanos, scores })
}

fn format_duration(duration_nanos: u64) -> String {
    let seconds = duration_nanos / 1_000_000_000;
    let frac = duration_nanos % 1_000_000_000;
    format!("{seconds}.{frac:09}s")
}

fn write_report<W: Write>(mut writer: W, report: &Report) -> io::Result<()> {
    writeln!(writer, "Dhrystone 2.1 (not zmips)")?;
    writeln!(writer, "runs            {}", report.runs)?;
    writeln!(writer, "duration        {}", format_duration(report.duration_nanos))?;
    writeln!(writer, "dhrystones/s    {:.2}", report.scores.dhrystones_per_second)?;
    writeln!(writer, "DMIPS           {:.2}", report.scores.dmips)?;
    writeln!(writer, "KDMIPS          {:.4}", report.scores.kdmips)?;
    writeln!(writer, "1 KDMIPS = 1000 DMIPS = {VAX_DHRYSTONES_PER_SECOND:.0} * 1000 Dhrystones/s")
}

fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let report = match measure(options) {
        Ok(report) => report,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let mut stdout = io::stdout().lock();
    if let Err(err) = write_report(&mut stdout, &report) {
        let _ = writeln!(io::stderr(), "kdmips: {err}");
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

    fn c_string(text: &[u8]) -> [u8; STRING_SIZE] {
        let mut buffer = [0u8; STRING_SIZE];
        copy_c_string(&mut buffer, text);
        buffer
    }

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert_eq!(parse_args(&args).unwrap(), Options::new());
    }

    #[test]
    fn parse_args_runs_and_target() {
        let args = ["-n".to_string(), "20000".to_string(), "-t250".to_string()];
        let options = parse_args(&args).unwrap();
        assert_eq!(options.runs, Some(20_000));
        assert_eq!(options.target_nanos, 250 * NANOS_PER_MILLI);
    }

    #[test]
    fn parse_args_rejects_zero_runs() {
        let args = ["-n".to_string(), "0".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("greater than 0"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_operand() {
        let args = ["file".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn one_kdmips_is_one_thousand_dmips() {
        let scores = scores_from_sample(1_757_000, 1_000_000_000).unwrap();
        assert!((scores.dmips - 1000.0).abs() < 0.001);
        assert!((scores.kdmips - 1.0).abs() < 0.000_001);
        assert!((scores.dhrystones_per_second - 1_757_000.0).abs() < 0.001);
    }

    #[test]
    fn vax_rate_is_one_dmips() {
        let scores = scores_from_sample(1757, 1_000_000_000).unwrap();
        assert!((scores.dmips - 1.0).abs() < 0.001);
        assert!((scores.kdmips - 0.001).abs() < 0.000_001);
    }

    #[test]
    fn zero_duration_has_no_score() {
        assert!(scores_from_sample(100, 0).is_none());
    }

    #[test]
    fn one_run_matches_dhrystone_self_check() {
        let mut bench = Bench::new();
        let locals = bench.run_iterations(1);
        assert_eq!(bench.int_glob, 5);
        assert!(bench.bool_glob);
        assert_eq!(bench.ch_1_glob, b'A');
        assert_eq!(bench.ch_2_glob, b'B');
        assert_eq!(bench.arr_1_glob[8], 7);
        assert_eq!(bench.arr_2_glob[8][7], 11);
        assert_eq!(bench.records[GLOB_INDEX].discriminant, Ident::One);
        assert_eq!(bench.records[GLOB_INDEX].enum_comp, Ident::Three);
        assert_eq!(bench.records[GLOB_INDEX].int_comp, 17);
        assert_eq!(
            bench.records[GLOB_INDEX].string_comp,
            c_string(b"DHRYSTONE PROGRAM, SOME STRING")
        );
        assert_eq!(bench.records[NEXT_INDEX].int_comp, 18);
        assert_eq!(locals.int_1, 5);
        assert_eq!(locals.int_2, 13);
        assert_eq!(locals.int_3, 7);
        assert_eq!(locals.enum_loc, Ident::Two);
        assert_eq!(locals.string_1, c_string(b"DHRYSTONE PROGRAM, 1'ST STRING"));
        assert_eq!(locals.string_2, c_string(b"DHRYSTONE PROGRAM, 2'ND STRING"));
    }

    #[test]
    fn write_report_lists_kdmips() {
        let report = Report {
            runs: 1_757_000,
            duration_nanos: 1_000_000_000,
            scores: scores_from_sample(1_757_000, 1_000_000_000).unwrap(),
        };
        let mut output = Vec::new();
        write_report(&mut output, &report).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Dhrystone 2.1 (not zmips)"));
        assert!(text.contains("DMIPS           1000.00"));
        assert!(text.contains("KDMIPS          1.0000"));
        assert!(text.contains("1 KDMIPS = 1000 DMIPS"));
    }
}
