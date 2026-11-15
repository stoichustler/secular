// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Delay for a given number of milliseconds.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "Usage: delay millis";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Options {
    Help,
    Delay { millis: u64 },
}

fn parse_millis(value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("delay: invalid number '{value}'\n{USAGE}"));
    }
    value.parse::<u64>().map_err(|_| format!("delay: invalid number '{value}'\n{USAGE}"))
}

/// Parses operands after `argv[0]`. The duration is milliseconds.
fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options_ended = false;
    let mut help = false;
    let mut millis = None;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && (arg == "--help" || arg == "-h") {
            help = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            for flag in arg.chars().skip(1) {
                if flag != 'h' {
                    return Err(format!("delay: invalid option -- '{flag}'\n{USAGE}"));
                }
                help = true;
            }
            continue;
        }
        if millis.is_some() {
            return Err(format!("delay: extra operand '{arg}'\n{USAGE}"));
        }
        millis = Some(parse_millis(arg)?);
    }
    if help {
        return Ok(Options::Help);
    }
    match millis {
        Some(millis) => Ok(Options::Delay { millis }),
        None => Err(format!("delay: missing operand\n{USAGE}")),
    }
}

fn delay(millis: u64) {
    if millis == 0 {
        return;
    }
    std::thread::sleep(Duration::from_millis(millis));
}

fn run(args: &[String]) -> i32 {
    match parse_args(args) {
        Ok(Options::Help) => {
            let _ = writeln!(io::stdout(), "{USAGE}");
            0
        }
        Ok(Options::Delay { millis }) => {
            delay(millis);
            0
        }
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            1
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_missing_operand() {
        let args: [String; 0] = [];
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("missing operand"));
        assert!(err.contains(USAGE));
    }

    #[test]
    fn parse_args_millis() {
        let args = ["250".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Options::Delay { millis: 250 });
    }

    #[test]
    fn parse_args_zero() {
        let args = ["0".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Options::Delay { millis: 0 });
    }

    #[test]
    fn parse_args_after_double_dash() {
        let args = ["--".to_string(), "10".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Options::Delay { millis: 10 });
    }

    #[test]
    fn parse_args_help() {
        let args = ["--help".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Options::Help);
        let args = ["-h".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Options::Help);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_invalid_number() {
        let args = ["1.5".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid number"));
        let args = ["ms".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid number"));
        let args = ["-1".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_extra_operand() {
        let args = ["10".to_string(), "20".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn delay_zero_returns() {
        delay(0);
    }
}
