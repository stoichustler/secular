// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Animate a console progress bar drawn with solid block columns.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "Usage: runman [-a] [-w width] [-i millis] [-n steps]";

const DEFAULT_WIDTH: u64 = 32;
const DEFAULT_INTERVAL_MILLIS: u64 = 50;
const DEFAULT_STEPS: u64 = 51;
const MIN_WIDTH: u64 = 1;
const MAX_WIDTH: u64 = 120;

/// Unicode full block (U+2588). One solid column per filled cell.
const SOLID_COLUMN: char = '█';
const ASCII_COLUMN: char = '#';
const EMPTY_CELL: char = ' ';

const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const CLEAR_LINE: &[u8] = b"\r\x1b[2K";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Options {
    ascii: bool,
    width: u64,
    interval_millis: u64,
    steps: u64,
}

impl Options {
    fn new() -> Self {
        Self {
            ascii: false,
            width: DEFAULT_WIDTH,
            interval_millis: DEFAULT_INTERVAL_MILLIS,
            steps: DEFAULT_STEPS,
        }
    }

    fn column(self) -> char {
        if self.ascii { ASCII_COLUMN } else { SOLID_COLUMN }
    }
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
        return Err(format!("runman: option requires an argument -- '{flag}'\n{USAGE}"));
    }
    let value = args[*index].clone();
    *index += 1;
    Ok(value)
}

/// Parses operands after `argv[0]`.
///
/// `-a` draws ASCII `#` columns. `-w` is bar width in columns. `-i` is the
/// frame interval in milliseconds. `-n` is how many frames to draw (progress
/// goes 0% … 100%).
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
                return Err(format!("runman: invalid option -- '-'\n{USAGE}"));
            }
            let mut flag_index = 0;
            while flag_index < flags.len() {
                let flag = flags[flag_index];
                match flag {
                    'a' => options.ascii = true,
                    'h' => return Err(USAGE.to_string()),
                    'w' | 'i' | 'n' => {
                        let value = take_flag_value(&flags, flag_index, args, &mut index, flag)?;
                        let count = parse_count(&value)
                            .map_err(|_| format!("runman: invalid number '{value}'\n{USAGE}"))?;
                        match flag {
                            'w' => {
                                if count < MIN_WIDTH || count > MAX_WIDTH {
                                    return Err(format!(
                                        "runman: width must be {MIN_WIDTH}..={MAX_WIDTH}\n{USAGE}"
                                    ));
                                }
                                options.width = count;
                            }
                            'i' => {
                                if count == 0 {
                                    return Err(format!(
                                        "runman: interval must be greater than 0\n{USAGE}"
                                    ));
                                }
                                options.interval_millis = count;
                            }
                            _ => {
                                if count == 0 {
                                    return Err(format!(
                                        "runman: steps must be greater than 0\n{USAGE}"
                                    ));
                                }
                                options.steps = count;
                            }
                        }
                        break;
                    }
                    _ => return Err(format!("runman: invalid option -- '{flag}'\n{USAGE}")),
                }
                flag_index += 1;
            }
            continue;
        }
        return Err(format!("runman: extra operand '{arg}'\n{USAGE}"));
    }
    Ok(options)
}

fn percent_for_step(step: u64, steps: u64) -> u32 {
    if steps <= 1 {
        return 100;
    }
    let last = steps.saturating_sub(1);
    ((step.saturating_mul(100)) / last).min(100) as u32
}

fn filled_columns(percent: u32, width: u64) -> u64 {
    (u64::from(percent).saturating_mul(width) / 100).min(width)
}

/// One progress frame: a row of solid columns, then empty cells, then percent.
fn format_frame(percent: u32, width: u64, column: char) -> String {
    let filled = filled_columns(percent, width);
    let empty = width.saturating_sub(filled);
    let mut bar = String::new();
    for _ in 0..filled {
        bar.push(column);
    }
    for _ in 0..empty {
        bar.push(EMPTY_CELL);
    }
    format!("runman [{bar}] {percent:3}%")
}

fn write_frame<W: Write>(writer: &mut W, frame: &str, in_place: bool) -> io::Result<()> {
    if in_place {
        writer.write_all(CLEAR_LINE)?;
    }
    writer.write_all(frame.as_bytes())?;
    if !in_place {
        writer.write_all(b"\n")?;
    }
    writer.flush()
}

fn run_animation<W: Write>(
    mut writer: W,
    options: Options,
    in_place: bool,
) -> io::Result<()> {
    let column = options.column();
    if in_place {
        writer.write_all(HIDE_CURSOR)?;
    }
    let result = (|| {
        for step in 0..options.steps {
            let percent = percent_for_step(step, options.steps);
            let frame = format_frame(percent, options.width, column);
            write_frame(&mut writer, &frame, in_place)?;
            if step + 1 < options.steps {
                std::thread::sleep(Duration::from_millis(options.interval_millis));
            }
        }
        Ok(())
    })();
    if in_place {
        let _ = writer.write_all(b"\n");
        let _ = writer.write_all(SHOW_CURSOR);
        let _ = writer.flush();
    }
    result
}

fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let stdout = io::stdout();
    let in_place = true;
    if let Err(err) = run_animation(stdout.lock(), options, in_place) {
        let _ = io::stdout().lock().write_all(SHOW_CURSOR);
        let _ = writeln!(io::stderr(), "runman: {err}");
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
        assert_eq!(parse_args(&args).unwrap(), Options::new());
    }

    #[test]
    fn parse_args_ascii_width_interval_steps() {
        let args = [
            "-a".to_string(),
            "-w".to_string(),
            "16".to_string(),
            "-i80".to_string(),
            "-n".to_string(),
            "11".to_string(),
        ];
        let options = parse_args(&args).unwrap();
        assert!(options.ascii);
        assert_eq!(options.width, 16);
        assert_eq!(options.interval_millis, 80);
        assert_eq!(options.steps, 11);
        assert_eq!(options.column(), ASCII_COLUMN);
    }

    #[test]
    fn parse_args_rejects_zero_interval() {
        let args = ["-i".to_string(), "0".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("interval must be greater than 0"));
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
    fn percent_for_step_spans_zero_to_hundred() {
        assert_eq!(percent_for_step(0, 5), 0);
        assert_eq!(percent_for_step(2, 5), 50);
        assert_eq!(percent_for_step(4, 5), 100);
        assert_eq!(percent_for_step(0, 1), 100);
    }

    #[test]
    fn format_frame_uses_solid_columns() {
        let empty = format_frame(0, 4, SOLID_COLUMN);
        assert_eq!(empty, "runman [    ]   0%");
        let half = format_frame(50, 4, SOLID_COLUMN);
        assert_eq!(half, format!("runman [{SOLID_COLUMN}{SOLID_COLUMN}  ]  50%"));
        let full = format_frame(100, 4, SOLID_COLUMN);
        assert_eq!(
            full,
            format!("runman [{SOLID_COLUMN}{SOLID_COLUMN}{SOLID_COLUMN}{SOLID_COLUMN}] 100%")
        );
    }

    #[test]
    fn format_frame_ascii_columns() {
        assert_eq!(format_frame(50, 4, ASCII_COLUMN), "runman [##  ]  50%");
    }

    #[test]
    fn write_frame_in_place_clears_line() {
        let mut output = Vec::new();
        write_frame(&mut output, "runman [█] 100%", true).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with("\r\u{1b}[2K"));
        assert!(text.contains("runman [█] 100%"));
        assert!(!text.ends_with('\n'));
    }

    #[test]
    fn run_animation_reaches_full_bar() {
        let mut output = Vec::new();
        let options = Options { ascii: true, width: 4, interval_millis: 1, steps: 3 };
        run_animation(&mut output, options, false).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("runman [    ]   0%"));
        assert!(text.contains("runman [##  ]  50%"));
        assert!(text.contains("runman [####] 100%"));
    }
}
