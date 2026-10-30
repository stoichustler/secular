// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Clear the console screen.

use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "Usage: clear [-x]";

/// Home cursor, erase the display, and erase the scrollback (`\x1b[H\x1b[2J\x1b[3J`).
const CLEAR: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";

/// Home cursor and erase the display, keeping scrollback (`\x1b[H\x1b[2J`).
const CLEAR_KEEP_SCROLLBACK: &[u8] = b"\x1b[H\x1b[2J";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Options {
    keep_scrollback: bool,
}

/// Parses operands after `argv[0]`. `-x` keeps the scrollback buffer.
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
                if flag != 'x' {
                    return Err(format!("clear: invalid option -- '{flag}'\n{USAGE}"));
                }
                options.keep_scrollback = true;
            }
            continue;
        }
        return Err(format!("clear: extra operand '{arg}'\n{USAGE}"));
    }
    Ok(options)
}

fn sequence(options: Options) -> &'static [u8] {
    if options.keep_scrollback { CLEAR_KEEP_SCROLLBACK } else { CLEAR }
}

fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let mut stdout = io::stdout().lock();
    if let Err(err) = stdout.write_all(sequence(options)).and_then(|_| stdout.flush()) {
        let _ = writeln!(io::stderr(), "clear: {err}");
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
        assert_eq!(parse_args(&args).unwrap(), Options::default());
    }

    #[test]
    fn parse_args_keep_scrollback() {
        let args = ["-x".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Options { keep_scrollback: true });
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-z".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_operand() {
        let args = ["tty".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn parse_args_double_dash_still_rejects_operand() {
        let args = ["--".to_string(), "-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn sequence_matches_line_editor_clear_when_keeping_scrollback() {
        assert_eq!(sequence(Options { keep_scrollback: true }), b"\x1b[H\x1b[2J");
    }

    #[test]
    fn sequence_also_erases_scrollback_by_default() {
        assert_eq!(sequence(Options::default()), b"\x1b[H\x1b[2J\x1b[3J");
    }
}
