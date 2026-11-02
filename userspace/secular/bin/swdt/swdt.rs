// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Background userspace software watchdog.
//!
//! `swdt` runs as a bootstrap component. Every 15s it pets itself and pings
//! `guest_manager` over `fuchsia.virtualization.GuestManager`. It does not
//! take `fuchsia.kernel.RootJob` and is not installed in `/boot/bin`.

use std::io::{self, Write};
use std::process::ExitCode;
#[cfg(target_os = "fuchsia")]
use std::time::Duration;

const USAGE: &str = "Usage: swdt [-p seconds] [-t seconds] [-n count]";

const DEFAULT_PERIOD_SECONDS: u64 = 15;
const DEFAULT_TIMEOUT_SECONDS: u64 = 45;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Options {
    period_seconds: u64,
    timeout_seconds: u64,
    cycle_count: u64,
}

impl Options {
    fn new() -> Self {
        Self {
            period_seconds: DEFAULT_PERIOD_SECONDS,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
            cycle_count: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestWatch {
    timeout_nanos: u128,
    start_nanos: u128,
    last_ok_nanos: Option<u128>,
    hung: bool,
}

impl GuestWatch {
    fn new(timeout_seconds: u64, start_nanos: u128) -> Self {
        Self {
            timeout_nanos: seconds_to_nanos(timeout_seconds),
            start_nanos,
            last_ok_nanos: None,
            hung: false,
        }
    }

    /// Updates hang state from one ping. `ping_ok` is true when `guest_manager`
    /// answered before the ping deadline.
    fn tick(&mut self, now_nanos: u128, ping_ok: bool) -> bool {
        if ping_ok {
            self.last_ok_nanos = Some(now_nanos);
            self.hung = false;
            return false;
        }
        let since = self.last_ok_nanos.unwrap_or(self.start_nanos);
        if now_nanos.saturating_sub(since) >= self.timeout_nanos {
            self.hung = true;
        }
        self.hung
    }
}

fn seconds_to_nanos(seconds: u64) -> u128 {
    u128::from(seconds).saturating_mul(1_000_000_000)
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
        return Err(format!("swdt: option requires an argument -- '{flag}'\n{USAGE}"));
    }
    let value = args[*index].clone();
    *index += 1;
    Ok(value)
}

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
                return Err(format!("swdt: invalid option -- '-'\n{USAGE}"));
            }
            let flag = flags[0];
            match flag {
                'h' => return Err(USAGE.to_string()),
                'p' | 't' | 'n' => {
                    let value = take_flag_value(&flags, 0, args, &mut index, flag)?;
                    let count = parse_count(&value)
                        .map_err(|_| format!("swdt: invalid number '{value}'\n{USAGE}"))?;
                    match flag {
                        'p' => {
                            if count == 0 {
                                return Err(format!(
                                    "swdt: period must be greater than 0\n{USAGE}"
                                ));
                            }
                            options.period_seconds = count;
                        }
                        't' => {
                            if count == 0 {
                                return Err(format!(
                                    "swdt: timeout must be greater than 0\n{USAGE}"
                                ));
                            }
                            options.timeout_seconds = count;
                        }
                        _ => options.cycle_count = count,
                    }
                }
                _ => return Err(format!("swdt: invalid option -- '{flag}'\n{USAGE}")),
            }
            continue;
        }
        return Err(format!("swdt: extra operand '{arg}'\n{USAGE}"));
    }
    Ok(options)
}

fn format_kick_line(kick_count: u64) -> String {
    format!("kick SECULAR count: {kick_count}")
}

fn format_hang_line() -> String {
    "oops, SECULAR hang!!!".to_string()
}

fn collect_daemon_lines(kick_count: u64, hung: bool) -> Vec<String> {
    let mut lines = vec![format_kick_line(kick_count)];
    if hung {
        lines.push(format_hang_line());
    }
    lines
}

#[cfg(target_os = "fuchsia")]
fn write_lines<W: io::Write>(writer: &mut W, lines: &[String]) -> io::Result<()> {
    for line in lines {
        writeln!(writer, "{line}")?;
    }
    writer.flush()
}

#[cfg(target_os = "fuchsia")]
async fn ping_guest_manager(timeout: Duration) -> bool {
    use fuchsia_async::{DurationExt, TimeoutExt};

    let Ok(proxy) = fuchsia_component::client::connect_to_protocol::<
        fidl_fuchsia_virtualization::GuestManagerMarker,
    >() else {
        return false;
    };
    let ping = async { proxy.list().await.map(|_| true).unwrap_or(false) };
    ping.on_timeout(timeout.after_now(), || false).await
}

#[cfg(target_os = "fuchsia")]
async fn run_daemon(options: Options) -> i32 {
    use fuchsia_async::{DurationExt, Timer};

    let epoch = std::time::Instant::now();
    let mut watch = GuestWatch::new(options.timeout_seconds, 0);
    let mut remaining = options.cycle_count;
    let forever = remaining == 0;
    let mut kick_count: u64 = 0;
    let mut last_hung;
    let period = Duration::from_secs(options.period_seconds);
    let ping_timeout = Duration::from_secs(options.period_seconds.min(options.timeout_seconds));

    loop {
        let cycle_start = std::time::Instant::now();
        let ping_ok = ping_guest_manager(ping_timeout).await;
        let hung = watch.tick(epoch.elapsed().as_nanos(), ping_ok);
        last_hung = hung;
        kick_count = kick_count.saturating_add(1);
        let lines = collect_daemon_lines(kick_count, hung);
        let mut stdout = io::stdout().lock();
        if write_lines(&mut stdout, &lines).is_err() {
            return 1;
        }
        if !forever {
            remaining = remaining.saturating_sub(1);
            if remaining == 0 {
                break;
            }
        }
        if let Some(sleep_for) = period.checked_sub(cycle_start.elapsed()) {
            Timer::new(sleep_for.after_now()).await;
        }
    }

    if last_hung { 1 } else { 0 }
}

fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };
    #[cfg(target_os = "fuchsia")]
    {
        fuchsia_async::LocalExecutorBuilder::new().build().run_singlethreaded(async {
            run_daemon(options).await
        })
    }
    #[cfg(not(target_os = "fuchsia"))]
    {
        // Host unit tests cover parse/format; the daemon loop is Fuchsia-only.
        let _ = options;
        0
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
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert_eq!(parse_args(&args).unwrap(), Options::new());
    }

    #[test]
    fn parse_args_period_timeout_cycles() {
        let args = ["-p".to_string(), "15".to_string(), "-t30".to_string(), "-n2".to_string()];
        let options = parse_args(&args).unwrap();
        assert_eq!(options.period_seconds, 15);
        assert_eq!(options.timeout_seconds, 30);
        assert_eq!(options.cycle_count, 2);
    }

    #[test]
    fn parse_args_rejects_zero_period() {
        let args = ["-p".to_string(), "0".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("period must be greater than 0"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_operand() {
        let args = ["guest_manager".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn guest_watch_waits_for_timeout_before_hang() {
        let mut watch = GuestWatch::new(45, 0);
        assert!(!watch.tick(0, false));
        assert!(!watch.tick(seconds_to_nanos(44), false));
        assert!(watch.tick(seconds_to_nanos(45), false));
    }

    #[test]
    fn guest_watch_recovers_after_successful_ping() {
        let mut watch = GuestWatch::new(45, 0);
        assert!(watch.tick(seconds_to_nanos(45), false));
        assert!(!watch.tick(seconds_to_nanos(46), true));
    }

    #[test]
    fn guest_watch_ok_ping_resets_stall() {
        let mut watch = GuestWatch::new(45, 0);
        assert!(!watch.tick(0, false));
        assert!(!watch.tick(seconds_to_nanos(30), true));
        assert!(!watch.tick(seconds_to_nanos(60), false));
        assert!(watch.tick(seconds_to_nanos(75), false));
    }

    #[test]
    fn format_kick_line_includes_count() {
        assert_eq!(format_kick_line(1), "kick SECULAR count: 1");
        assert_eq!(format_kick_line(42), "kick SECULAR count: 42");
    }

    #[test]
    fn format_hang_line_is_oops() {
        assert_eq!(format_hang_line(), "oops, SECULAR hang!!!");
    }

    #[test]
    fn collect_daemon_lines_kick_then_hang() {
        assert_eq!(collect_daemon_lines(3, false), vec!["kick SECULAR count: 3".to_string()]);
        assert_eq!(
            collect_daemon_lines(3, true),
            vec!["kick SECULAR count: 3".to_string(), "oops, SECULAR hang!!!".to_string()]
        );
    }
}
