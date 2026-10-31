// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Print a file in canonical hexadecimal and ASCII.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::process::ExitCode;

const STDIN_OPERAND: &str = "-";
const USAGE: &str = "Usage: hexdump [-C] [-n length] [-s offset] [file...]";
const BYTES_PER_LINE: usize = 16;
const MIDDLE_SPLIT: usize = 8;
const READ_BUFFER_SIZE: usize = 4096;
const OFFSET_WIDTH_32: usize = 8;
const OFFSET_WIDTH_64: usize = 16;
const OFFSET_32_LIMIT: u64 = 0xffff_ffff;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Options {
    skip_bytes: u64,
    max_bytes: Option<u64>,
}

/// Parses a decimal or `0x`-prefixed hexadecimal count.
fn parse_count(value: &str) -> Result<u64, ()> {
    let (digits, radix) =
        if let Some(rest) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
            (rest, 16)
        } else {
            (value, 10)
        };
    u64::from_str_radix(digits, radix).map_err(|_| ())
}

/// Parses operands after `argv[0]`. `-C` is accepted for compatibility.
fn parse_args(args: &[String]) -> Result<(Options, Vec<&str>), String> {
    let mut options = Options::default();
    let mut operands = Vec::new();
    let mut options_ended = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != STDIN_OPERAND {
            let flags: Vec<char> = arg.chars().skip(1).collect();
            if flags.is_empty() {
                return Err(format!("hexdump: invalid option -- '-'\n{USAGE}"));
            }
            let mut flag_index = 0;
            while flag_index < flags.len() {
                match flags[flag_index] {
                    'C' => {}
                    'n' | 's' => {
                        let flag = flags[flag_index];
                        let attached: String = flags[flag_index + 1..].iter().collect();
                        let value = if attached.is_empty() {
                            if index >= args.len() {
                                return Err(format!(
                                    "hexdump: option requires an argument -- '{flag}'\n{USAGE}"
                                ));
                            }
                            let next = args[index].clone();
                            index += 1;
                            next
                        } else {
                            attached
                        };
                        let count = parse_count(&value)
                            .map_err(|_| format!("hexdump: invalid number '{value}'\n{USAGE}"))?;
                        if flag == 'n' {
                            options.max_bytes = Some(count);
                        } else {
                            options.skip_bytes = count;
                        }
                        break;
                    }
                    flag => {
                        return Err(format!("hexdump: invalid option -- '{flag}'\n{USAGE}"));
                    }
                }
                flag_index += 1;
            }
            continue;
        }
        operands.push(arg.as_str());
    }
    Ok((options, operands))
}

fn offset_width(offset: u64) -> usize {
    if offset > OFFSET_32_LIMIT { OFFSET_WIDTH_64 } else { OFFSET_WIDTH_32 }
}

fn ascii_char(byte: u8) -> char {
    if byte.is_ascii_graphic() || byte == b' ' { byte as char } else { '.' }
}

/// Formats one canonical `hexdump -C` line, including the trailing newline.
fn format_canonical_line(offset: u64, data: &[u8]) -> String {
    debug_assert!(data.len() <= BYTES_PER_LINE);
    let width = offset_width(offset);
    let mut line = format!("{offset:0width$x}  ");
    for index in 0..BYTES_PER_LINE {
        if index == MIDDLE_SPLIT {
            line.push(' ');
        }
        if index < data.len() {
            line.push_str(&format!("{:02x} ", data[index]));
        } else {
            line.push_str("   ");
        }
    }
    line.push(' ');
    line.push('|');
    for &byte in data {
        line.push(ascii_char(byte));
    }
    line.push('|');
    line.push('\n');
    line
}

fn format_final_offset(offset: u64) -> String {
    let width = offset_width(offset);
    format!("{offset:0width$x}\n")
}

fn skip_reader<R: Read>(reader: &mut R, mut remaining: u64) -> io::Result<()> {
    let mut buffer = [0u8; READ_BUFFER_SIZE];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..want])?;
        if read == 0 {
            return Ok(());
        }
        remaining -= read as u64;
    }
    Ok(())
}

fn skip_input<R: Read + Seek>(reader: &mut R, skip_bytes: u64) -> io::Result<()> {
    if skip_bytes == 0 {
        return Ok(());
    }
    match reader.seek(SeekFrom::Start(skip_bytes)) {
        Ok(_) => Ok(()),
        Err(err)
            if err.kind() == ErrorKind::NotSeekable || err.kind() == ErrorKind::Unsupported =>
        {
            skip_reader(reader, skip_bytes)
        }
        Err(err) => Err(err),
    }
}

fn dump_reader<R: Read, W: Write>(
    mut reader: R,
    writer: &mut W,
    options: Options,
) -> io::Result<u64> {
    let mut remaining = options.max_bytes;
    let mut offset = options.skip_bytes;
    let mut line = [0u8; BYTES_PER_LINE];
    let mut filled = 0usize;

    loop {
        if remaining == Some(0) {
            break;
        }
        let want = match remaining {
            Some(left) => (BYTES_PER_LINE - filled).min(left as usize),
            None => BYTES_PER_LINE - filled,
        };
        let read = reader.read(&mut line[filled..filled + want])?;
        if read == 0 {
            break;
        }
        filled += read;
        if let Some(left) = remaining.as_mut() {
            *left -= read as u64;
        }
        if filled == BYTES_PER_LINE {
            writer.write_all(format_canonical_line(offset, &line).as_bytes())?;
            offset = offset.saturating_add(BYTES_PER_LINE as u64);
            filled = 0;
        }
    }

    if filled > 0 {
        writer.write_all(format_canonical_line(offset, &line[..filled]).as_bytes())?;
        offset = offset.saturating_add(filled as u64);
    }
    writer.write_all(format_final_offset(offset).as_bytes())?;
    Ok(offset)
}

fn dump_path<W: Write>(path: &str, writer: &mut W, options: Options) -> io::Result<u64> {
    if path == STDIN_OPERAND {
        let mut stdin = io::stdin();
        skip_reader(&mut stdin, options.skip_bytes)?;
        return dump_reader(stdin, writer, options);
    }

    let metadata = std::fs::metadata(path)?;
    if metadata.is_dir() {
        return Err(io::Error::new(ErrorKind::Other, "Is a directory"));
    }

    let mut file = File::open(path)?;
    skip_input(&mut file, options.skip_bytes)?;
    dump_reader(file, writer, options)
}

fn report_error(path: &str, err: &io::Error) {
    let _ = writeln!(io::stderr(), "hexdump: {path}: {err}");
}

fn run(args: &[String]) -> i32 {
    let (options, operands) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let mut stdout = io::stdout().lock();
    let mut status = 0;
    let paths: &[&str] = if operands.is_empty() { &[STDIN_OPERAND] } else { operands.as_slice() };
    for path in paths {
        if let Err(err) = dump_path(path, &mut stdout, options) {
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
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        let (options, operands) = parse_args(&args).unwrap();
        assert_eq!(options, Options::default());
        assert!(operands.is_empty());
    }

    #[test]
    fn parse_args_skip_and_length() {
        let args = ["-s".to_string(), "0x10".to_string(), "-n16".to_string(), "a.bin".to_string()];
        let (options, operands) = parse_args(&args).unwrap();
        assert_eq!(options.skip_bytes, 16);
        assert_eq!(options.max_bytes, Some(16));
        assert_eq!(operands, ["a.bin"]);
    }

    #[test]
    fn parse_args_accepts_canonical_flag() {
        let args = ["-C".to_string(), "-".to_string()];
        let (options, operands) = parse_args(&args).unwrap();
        assert_eq!(options, Options::default());
        assert_eq!(operands, ["-"]);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-z".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_missing_count() {
        let args = ["-n".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("option requires an argument"));
    }

    #[test]
    fn parse_args_double_dash_ends_options() {
        let args = ["--".to_string(), "-n".to_string()];
        assert_eq!(parse_args(&args).unwrap().1, ["-n"]);
    }

    #[test]
    fn format_canonical_line_pads_short_row() {
        let line = format_canonical_line(0, b"ELF");
        assert_eq!(line, "00000000  45 4c 46                                          |ELF|\n");
    }

    #[test]
    fn format_canonical_line_splits_middle() {
        let data = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let line = format_canonical_line(0, data);
        assert_eq!(
            line,
            "00000000  7f 45 4c 46 02 01 01 00  00 00 00 00 00 00 00 00  |.ELF............|\n"
        );
    }

    #[test]
    fn dump_reader_writes_final_offset() {
        let mut output = Vec::new();
        dump_reader(Cursor::new(b"ab"), &mut output, Options::default()).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "00000000  61 62                                             |ab|\n00000002\n"
        );
    }

    #[test]
    fn dump_reader_honors_skip_and_length() {
        let mut output = Vec::new();
        let options = Options { skip_bytes: 2, max_bytes: Some(2) };
        // skip is applied by the caller; dump_reader only limits length and display offset.
        dump_reader(Cursor::new(b"CD"), &mut output, options).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "00000002  43 44                                             |CD|\n00000004\n"
        );
    }

    #[test]
    fn dump_reader_empty_prints_origin() {
        let mut output = Vec::new();
        dump_reader(Cursor::new(b""), &mut output, Options::default()).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "00000000\n");
    }
}
