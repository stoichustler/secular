// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Concatenate files and print on the standard output.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::process::ExitCode;

const STDIN_OPERAND: &str = "-";
const USAGE: &str = "Usage: cat [-u] [file...]";

/// Copies `reader` to `writer`.
fn copy_all<R: Read, W: Write>(mut reader: R, mut writer: W) -> io::Result<()> {
    io::copy(&mut reader, &mut writer)?;
    writer.flush()
}

/// Writes one file (or stdin, when `path` is `-`) to `writer`.
fn cat_path<W: Write>(path: &str, writer: W) -> io::Result<()> {
    if path == STDIN_OPERAND {
        return copy_all(io::stdin(), writer);
    }

    let metadata = std::fs::metadata(path)?;
    if metadata.is_dir() {
        return Err(io::Error::new(ErrorKind::Other, "Is a directory"));
    }

    copy_all(File::open(path)?, writer)
}

/// Parses operands after `argv[0]`. `-u` is accepted and ignored (POSIX).
fn parse_operands(args: &[String]) -> Result<Vec<&str>, String> {
    let mut operands = Vec::new();
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != STDIN_OPERAND {
            for flag in arg.chars().skip(1) {
                if flag != 'u' {
                    return Err(format!("cat: invalid option -- '{flag}'\n{USAGE}"));
                }
            }
            continue;
        }
        operands.push(arg.as_str());
    }
    Ok(operands)
}

fn report_error(path: &str, err: &io::Error) {
    let _ = writeln!(io::stderr(), "cat: {path}: {err}");
}

/// Concatenates `args` (program operands, not including argv[0]) to stdout.
///
/// Returns `0` on success and `1` if any file could not be read.
fn run(args: &[String]) -> i32 {
    let operands = match parse_operands(args) {
        Ok(operands) => operands,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let mut stdout = io::stdout().lock();
    let mut status = 0;
    let paths: &[&str] =
        if operands.is_empty() { &[STDIN_OPERAND] } else { operands.as_slice() };
    for path in paths {
        if let Err(err) = cat_path(path, &mut stdout) {
            report_error(path, &err);
            status = 1;
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
    use std::io::Cursor;

    #[test]
    fn parse_operands_files_and_dash() {
        let args = ["a.txt".to_string(), "-".to_string(), "b.txt".to_string()];
        assert_eq!(parse_operands(&args).unwrap(), ["a.txt", "-", "b.txt"]);
    }

    #[test]
    fn parse_operands_ignores_u() {
        let args = ["-u".to_string(), "file".to_string()];
        assert_eq!(parse_operands(&args).unwrap(), ["file"]);
    }

    #[test]
    fn parse_operands_rejects_unknown_flag() {
        let args = ["-n".to_string()];
        assert!(parse_operands(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_operands_double_dash_ends_options() {
        let args = ["--".to_string(), "-n".to_string()];
        assert_eq!(parse_operands(&args).unwrap(), ["-n"]);
    }

    #[test]
    fn copy_all_writes_bytes() {
        let input = b"hello\nworld";
        let mut output = Vec::new();
        copy_all(Cursor::new(input), &mut output).unwrap();
        assert_eq!(output, input);
    }
}
