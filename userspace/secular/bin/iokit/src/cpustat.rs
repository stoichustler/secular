// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Print CPU identity, sampled load, and scheduler pressure.
//!
//! Basic identity comes from the vDSO (`zx_system_get_num_cpus`, ticks,
//! page size). Per-CPU idle/busy and counters come from
//! `ZX_INFO_CPU_STATS` via `fuchsia.kernel.InfoResource`.
//!
//! Load is scheduler-normalized busy time over the sample interval (same
//! definition as `kstats`). Busy is wall time that was not idle. Hints
//! flag saturation, idle cores next to a hot core, and interrupt-bound
//! work — the usual CPU-capacity limits on this console image.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "Usage: cpustat [-i millis] [-n count] [-q]";
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const NANOS_PER_MILLI: u64 = 1_000_000;
const DEFAULT_INTERVAL_MILLIS: u64 = 1000;
const DEFAULT_SAMPLES: u64 = 1;
const SATURATED_BP: i64 = 9000;
const IDLE_BP: i64 = 1000;
const HIGH_CS_PER_SEC: u64 = 10_000;
const HIGH_IRQ_PER_SEC: u64 = 1_000;
const ZX_INFO_CPU_STATS_FLAG_ONLINE: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Options {
    interval_millis: u64,
    samples: u64,
    quiet: bool,
}

impl Options {
    fn new() -> Self {
        Self { interval_millis: DEFAULT_INTERVAL_MILLIS, samples: DEFAULT_SAMPLES, quiet: false }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    cpu_count: u32,
    ticks_per_second: i64,
    page_size: u32,
    version: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CpuSample {
    cpu_number: u32,
    flags: u32,
    idle_time: i64,
    normalized_busy_time: i64,
    reschedules: u64,
    context_switches: u64,
    irq_preempts: u64,
    preempts: u64,
    yields: u64,
    ints: u64,
    timer_ints: u64,
    timers: u64,
    syscalls: u64,
    reschedule_ipis: u64,
    generic_ipis: u64,
}

impl CpuSample {
    fn online(&self) -> bool {
        self.flags & ZX_INFO_CPU_STATS_FLAG_ONLINE != 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CpuDelta {
    /// `cpu` column: logical CPU index from `ZX_INFO_CPU_STATS`.
    cpu_number: u32,
    /// `state` column: `ZX_INFO_CPU_STATS_FLAG_ONLINE` at the end of the sample.
    online: bool,
    idle_delta: i64,
    /// `busy` column: wall time in the interval that was not idle.
    busy_delta: i64,
    /// `load` column: scheduler-normalized busy time (same as `kstats` load).
    load_delta: i64,
    reschedules: u64,
    /// `cs` column: thread context switches.
    context_switches: u64,
    irq_preempts: u64,
    /// `pmpt` column: preemptions (higher-priority work interrupted this CPU).
    preempts: u64,
    /// `yld` column: voluntary yields.
    yields: u64,
    /// `irq` column: hardware interrupts excluding timer IRQs.
    ints: u64,
    timer_ints: u64,
    timers: u64,
    /// `sysc` column: syscalls handled on this CPU.
    syscalls: u64,
    /// Folded into `ipi` with `generic_ipis`: scheduler reschedule IPIs.
    reschedule_ipis: u64,
    /// Folded into `ipi` with `reschedule_ipis`: other inter-processor interrupts.
    generic_ipis: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Report {
    identity: Identity,
    interval_nanos: i64,
    deltas: Vec<CpuDelta>,
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
        return Err(format!("cpustat: option requires an argument -- '{flag}'\n{USAGE}"));
    }
    let value = args[*index].clone();
    *index += 1;
    Ok(value)
}

/// Parses operands after `argv[0]`.
///
/// `-i` is the sample interval in milliseconds. `-n` is how many samples to
/// print after the baseline (`0` repeats until interrupted). `-q` skips hints.
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
                return Err(format!("cpustat: invalid option -- '-'\n{USAGE}"));
            }
            let mut flag_index = 0;
            while flag_index < flags.len() {
                let flag = flags[flag_index];
                match flag {
                    'q' => options.quiet = true,
                    'i' | 'n' => {
                        let value = take_flag_value(&flags, flag_index, args, &mut index, flag)?;
                        let count = parse_count(&value)
                            .map_err(|_| format!("cpustat: invalid number '{value}'\n{USAGE}"))?;
                        if flag == 'i' {
                            if count == 0 {
                                return Err(format!(
                                    "cpustat: interval must be greater than 0\n{USAGE}"
                                ));
                            }
                            options.interval_millis = count;
                        } else {
                            options.samples = count;
                        }
                        break;
                    }
                    _ => return Err(format!("cpustat: invalid option -- '{flag}'\n{USAGE}")),
                }
                flag_index += 1;
            }
            continue;
        }
        return Err(format!("cpustat: extra operand '{arg}'\n{USAGE}"));
    }
    Ok(options)
}

fn saturating_delta_i64(now: i64, then: i64) -> i64 {
    now.saturating_sub(then).max(0)
}

fn saturating_delta_u64(now: u64, then: u64) -> u64 {
    now.saturating_sub(then)
}

fn diff_cpu(now: &CpuSample, then: &CpuSample, interval_nanos: i64) -> CpuDelta {
    let idle_delta = saturating_delta_i64(now.idle_time, then.idle_time);
    let busy_delta = (interval_nanos - idle_delta).clamp(0, interval_nanos.max(0));
    let load_delta = saturating_delta_i64(now.normalized_busy_time, then.normalized_busy_time)
        .min(interval_nanos.max(0));
    CpuDelta {
        cpu_number: now.cpu_number,
        online: now.online(),
        idle_delta,
        busy_delta,
        load_delta,
        reschedules: saturating_delta_u64(now.reschedules, then.reschedules),
        context_switches: saturating_delta_u64(now.context_switches, then.context_switches),
        irq_preempts: saturating_delta_u64(now.irq_preempts, then.irq_preempts),
        preempts: saturating_delta_u64(now.preempts, then.preempts),
        yields: saturating_delta_u64(now.yields, then.yields),
        ints: saturating_delta_u64(now.ints, then.ints),
        timer_ints: saturating_delta_u64(now.timer_ints, then.timer_ints),
        timers: saturating_delta_u64(now.timers, then.timers),
        syscalls: saturating_delta_u64(now.syscalls, then.syscalls),
        reschedule_ipis: saturating_delta_u64(now.reschedule_ipis, then.reschedule_ipis),
        generic_ipis: saturating_delta_u64(now.generic_ipis, then.generic_ipis),
    }
}

fn basis_points(numer: i64, denom: i64) -> i64 {
    if denom <= 0 {
        0
    } else {
        numer.saturating_mul(10000).saturating_div(denom).clamp(0, 10000)
    }
}

fn format_percent(numer: i64, denom: i64) -> String {
    if denom <= 0 {
        return "--".to_string();
    }
    let bp = basis_points(numer, denom);
    format!("{}.{:02}%", bp / 100, bp % 100)
}

fn format_duration_millis(millis: u64) -> String {
    let seconds = millis / 1000;
    let frac = millis % 1000;
    format!("{seconds}.{frac:03}s")
}

fn per_second(count: u64, interval_nanos: i64) -> u64 {
    if interval_nanos <= 0 {
        0
    } else {
        ((count as u128) * (NANOS_PER_SECOND as u128) / (interval_nanos as u128)) as u64
    }
}

fn write_field<W: Write>(writer: &mut W, label: &str, value: &str) -> io::Result<()> {
    writeln!(writer, "{label:<12} {value}")
}

fn write_cpu_row<W: Write>(writer: &mut W, delta: &CpuDelta, interval_nanos: i64) -> io::Result<()> {
    let state = if delta.online { "online" } else { "offline" };
    writeln!(
        writer,
        "{:>3}  {state:<7}  {:>7}  {:>7}  {:>8}  {:>5}  {:>5}  {:>8}  {:>8}  {:>6}",
        delta.cpu_number,                                      // cpu
        format_percent(delta.load_delta, interval_nanos),      // load
        format_percent(delta.busy_delta, interval_nanos),      // busy
        delta.context_switches,                                // cs
        delta.yields,                                          // yld
        delta.preempts,                                        // pmpt
        delta.syscalls,                                        // sysc
        delta.ints,                                            // irq
        delta.reschedule_ipis.saturating_add(delta.generic_ipis), // ipi
    )
}

fn write_total_row<W: Write>(
    writer: &mut W,
    deltas: &[CpuDelta],
    interval_nanos: i64,
) -> io::Result<()> {
    let mut total = CpuDelta { cpu_number: 0, online: true, ..CpuDelta::default() };
    let mut load = 0i64;
    let mut busy = 0i64;
    for delta in deltas {
        load = load.saturating_add(delta.load_delta);
        busy = busy.saturating_add(delta.busy_delta);
        total.context_switches = total.context_switches.saturating_add(delta.context_switches);
        total.yields = total.yields.saturating_add(delta.yields);
        total.preempts = total.preempts.saturating_add(delta.preempts);
        total.syscalls = total.syscalls.saturating_add(delta.syscalls);
        total.ints = total.ints.saturating_add(delta.ints);
        total.reschedule_ipis = total.reschedule_ipis.saturating_add(delta.reschedule_ipis);
        total.generic_ipis = total.generic_ipis.saturating_add(delta.generic_ipis);
    }
    let n = i64::try_from(deltas.len().max(1)).unwrap_or(1);
    total.load_delta = load / n;
    total.busy_delta = busy / n;
    writeln!(
        writer,
        "all  {:<7}  {:>7}  {:>7}  {:>8}  {:>5}  {:>5}  {:>8}  {:>8}  {:>6}",
        "",
        format_percent(total.load_delta, interval_nanos),
        format_percent(total.busy_delta, interval_nanos),
        total.context_switches,
        total.yields,
        total.preempts,
        total.syscalls,
        total.ints,
        total.reschedule_ipis.saturating_add(total.generic_ipis),
    )
}

fn hints(deltas: &[CpuDelta], interval_nanos: i64) -> Vec<String> {
    let mut out = Vec::new();
    if deltas.is_empty() || interval_nanos <= 0 {
        return out;
    }

    let online: Vec<&CpuDelta> = deltas.iter().filter(|cpu| cpu.online).collect();
    let offline = deltas.len() - online.len();
    if offline > 0 {
        out.push(format!(
            "{offline} CPU(s) offline — hardware capacity is reduced (hotplug, parking, or boot)"
        ));
    }
    if online.is_empty() {
        return out;
    }

    let loads: Vec<(u32, i64)> = online
        .iter()
        .map(|cpu| (cpu.cpu_number, basis_points(cpu.load_delta, interval_nanos)))
        .collect();
    let max = loads.iter().max_by_key(|(_, bp)| *bp).copied().unwrap_or((0, 0));
    let min = loads.iter().min_by_key(|(_, bp)| *bp).copied().unwrap_or((0, 0));
    let saturated: Vec<u32> =
        loads.iter().filter(|(_, bp)| *bp >= SATURATED_BP).map(|(id, _)| *id).collect();

    if saturated.len() == online.len() {
        out.push(format!(
            "all {} online CPU(s) at or above 90% load — CPU-bound; this is a compute capacity limit",
            online.len()
        ));
    } else if !saturated.is_empty() && min.1 <= IDLE_BP {
        let hot = loads.iter().find(|(id, _)| *id == saturated[0]).map(|(_, bp)| *bp).unwrap_or(max.1);
        out.push(format!(
            "cpu {} at {} while cpu {} is idle — not enough parallelism or affinity pinning",
            saturated[0],
            format_percent(hot, 10000),
            min.0
        ));
    } else if max.1 >= SATURATED_BP {
        out.push(format!(
            "cpu {} load {} — that core is saturated",
            max.0,
            format_percent(max.1, 10000)
        ));
    }

    for cpu in &online {
        let irq_s = per_second(cpu.ints, interval_nanos);
        let sysc_s = per_second(cpu.syscalls, interval_nanos);
        if irq_s >= HIGH_IRQ_PER_SEC && cpu.ints > cpu.syscalls.saturating_mul(2) {
            out.push(format!(
                "cpu {} interrupt-heavy ({} irq/s vs {} sysc/s) — check timers, virtio, or serial",
                cpu.cpu_number, irq_s, sysc_s
            ));
        }
        let cs_s = per_second(cpu.context_switches, interval_nanos);
        if cs_s >= HIGH_CS_PER_SEC && basis_points(cpu.load_delta, interval_nanos) < SATURATED_BP {
            out.push(format!(
                "cpu {} {} context switches/s at modest load — wakeup/contention, not raw ALU",
                cpu.cpu_number, cs_s
            ));
        }
        let ipi = cpu.reschedule_ipis.saturating_add(cpu.generic_ipis);
        if per_second(ipi, interval_nanos) >= HIGH_IRQ_PER_SEC {
            out.push(format!(
                "cpu {} high IPI rate — cross-CPU reschedule pressure",
                cpu.cpu_number
            ));
        }
    }
    out
}

fn write_report<W: Write>(mut writer: W, report: &Report, quiet: bool) -> io::Result<()> {
    let online = report.deltas.iter().filter(|cpu| cpu.online).count();
    let offline = report.deltas.len().saturating_sub(online);
    write_field(
        &mut writer,
        "cpus",
        &format!("{}  (online {online}, offline {offline})", report.identity.cpu_count),
    )?;
    write_field(&mut writer, "ticks/s", &report.identity.ticks_per_second.to_string())?;
    write_field(&mut writer, "page-size", &report.identity.page_size.to_string())?;
    write_field(
        &mut writer,
        "interval",
        &format_duration_millis((report.interval_nanos.max(0) as u64) / NANOS_PER_MILLI),
    )?;
    write_field(&mut writer, "version", &report.identity.version)?;
    writeln!(writer)?;
    // Per-CPU sample over `interval`. Columns:
    //   cpu   logical CPU index
    //   state online if ZX_INFO_CPU_STATS_FLAG_ONLINE, else offline
    //   load  scheduler-normalized busy / interval (kstats load; capacity used)
    //   busy  (interval - idle) / interval (wall time not idle)
    //   cs    context switches
    //   yld   voluntary yields
    //   pmpt  preemptions
    //   sysc  syscalls
    //   irq   hardware interrupts minus timer IRQs
    //   ipi   reschedule_ipis + generic_ipis
    writeln!(
        writer,
        "cpu   state      load     busy        cs    yld   pmpt      sysc       irq     ipi"
    )?;
    for delta in &report.deltas {
        write_cpu_row(&mut writer, delta, report.interval_nanos)?;
    }
    write_total_row(&mut writer, &report.deltas, report.interval_nanos)?;
    if !quiet {
        let notes = hints(&report.deltas, report.interval_nanos);
        if !notes.is_empty() {
            writeln!(writer)?;
            writeln!(writer, "hints:")?;
            for note in notes {
                writeln!(writer, "  {note}")?;
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "fuchsia")]
fn take_identity() -> Identity {
    // SAFETY: These vDSO syscalls have no preconditions.
    let cpu_count = unsafe { zx_sys::zx_system_get_num_cpus() };
    let ticks_per_second = unsafe { zx_sys::zx_ticks_per_second() };
    let page_size = unsafe { zx_sys::zx_system_get_page_size() };
    let version_view = unsafe { zx_sys::zx_system_get_version_string() };
    let version = if version_view.c_str.is_null() {
        String::new()
    } else {
        // SAFETY: Kernel returns a static NUL-terminated UTF-8 string.
        let bytes = unsafe { std::slice::from_raw_parts(version_view.c_str, version_view.length) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    Identity { cpu_count, ticks_per_second, page_size, version }
}

#[cfg(not(target_os = "fuchsia"))]
fn take_identity() -> Identity {
    Identity { cpu_count: 0, ticks_per_second: 0, page_size: 0, version: "host".to_string() }
}

#[cfg(target_os = "fuchsia")]
fn cpu_sample_from_kernel(stats: &zx::PerCpuStats) -> CpuSample {
    CpuSample {
        cpu_number: stats.cpu_number,
        flags: stats.flags,
        idle_time: stats.idle_time,
        normalized_busy_time: stats.normalized_busy_time,
        reschedules: stats.reschedules,
        context_switches: stats.context_switches,
        irq_preempts: stats.irq_preempts,
        preempts: stats.preempts,
        yields: stats.yields,
        ints: stats.ints,
        timer_ints: stats.timer_ints,
        timers: stats.timers,
        syscalls: stats.syscalls,
        reschedule_ipis: stats.reschedule_ipis,
        generic_ipis: stats.generic_ipis,
    }
}

#[cfg(target_os = "fuchsia")]
fn fetch_info_resource() -> Result<zx::Resource, String> {
    fuchsia_async::LocalExecutorBuilder::new().build().run_singlethreaded(async {
        let proxy = fuchsia_component::client::connect_to_protocol::<
            fidl_fuchsia_kernel::InfoResourceMarker,
        >()
        .map_err(|err| {
            format!("cpustat: cannot connect to fuchsia.kernel.InfoResource: {err}")
        })?;
        proxy
            .get()
            .await
            .map_err(|err| format!("cpustat: InfoResource.Get failed: {err}"))
    })
}

#[cfg(target_os = "fuchsia")]
fn take_cpu_samples(resource: &zx::Resource) -> Result<Vec<CpuSample>, String> {
    resource
        .cpu_stats()
        .map(|stats| stats.iter().map(cpu_sample_from_kernel).collect())
        .map_err(|err| format!("cpustat: ZX_INFO_CPU_STATS failed: {err}"))
}

fn sleep_interval(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}

fn build_report(
    identity: Identity,
    before: &[CpuSample],
    after: &[CpuSample],
    interval_nanos: i64,
) -> Report {
    let mut deltas = Vec::new();
    let n = before.len().min(after.len());
    for index in 0..n {
        deltas.push(diff_cpu(&after[index], &before[index], interval_nanos));
    }
    Report { identity, interval_nanos, deltas }
}

#[cfg(target_os = "fuchsia")]
fn run_samples(options: Options) -> Result<(), String> {
    let identity = take_identity();
    let resource = fetch_info_resource()?;
    let interval_nanos = i64::try_from(options.interval_millis.saturating_mul(NANOS_PER_MILLI))
        .unwrap_or(i64::MAX);
    let mut previous = take_cpu_samples(&resource)?;
    let mut remaining = options.samples;
    let forever = remaining == 0;
    loop {
        sleep_interval(options.interval_millis);
        let current = take_cpu_samples(&resource)?;
        let report = build_report(identity.clone(), &previous, &current, interval_nanos);
        let mut stdout = io::stdout().lock();
        write_report(&mut stdout, &report, options.quiet)
            .map_err(|err| format!("cpustat: {err}"))?;
        if !forever {
            remaining = remaining.saturating_sub(1);
            if remaining == 0 {
                break;
            }
            writeln!(stdout).map_err(|err| format!("cpustat: {err}"))?;
        } else {
            writeln!(stdout).map_err(|err| format!("cpustat: {err}"))?;
        }
        previous = current;
    }
    Ok(())
}

#[cfg(not(target_os = "fuchsia"))]
fn run_samples(options: Options) -> Result<(), String> {
    let identity = take_identity();
    let interval_nanos = i64::try_from(options.interval_millis.saturating_mul(NANOS_PER_MILLI))
        .unwrap_or(i64::MAX);
    let report = Report { identity, interval_nanos, deltas: Vec::new() };
    let mut stdout = io::stdout().lock();
    write_report(&mut stdout, &report, options.quiet).map_err(|err| format!("cpustat: {err}"))?;
    let _ = options.samples;
    Ok(())
}

fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };
    if let Err(message) = run_samples(options) {
        let _ = writeln!(io::stderr(), "{message}");
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

    fn sample(cpu: u32, online: bool, idle: i64, load: i64) -> CpuSample {
        CpuSample {
            cpu_number: cpu,
            flags: if online { ZX_INFO_CPU_STATS_FLAG_ONLINE } else { 0 },
            idle_time: idle,
            normalized_busy_time: load,
            ..CpuSample::default()
        }
    }

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert_eq!(parse_args(&args).unwrap(), Options::new());
    }

    #[test]
    fn parse_args_interval_samples_quiet() {
        let args = ["-i250".to_string(), "-n".to_string(), "3".to_string(), "-q".to_string()];
        let options = parse_args(&args).unwrap();
        assert_eq!(options.interval_millis, 250);
        assert_eq!(options.samples, 3);
        assert!(options.quiet);
    }

    #[test]
    fn parse_args_rejects_zero_interval() {
        let args = ["-i".to_string(), "0".to_string()];
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
    fn format_percent_clamps() {
        assert_eq!(format_percent(0, 1_000_000_000), "0.00%");
        assert_eq!(format_percent(250_000_000, 1_000_000_000), "25.00%");
        assert_eq!(format_percent(1_000_000_000, 1_000_000_000), "100.00%");
        assert_eq!(format_percent(1, 0), "--");
    }

    #[test]
    fn diff_cpu_uses_idle_and_normalized_busy() {
        let then = sample(0, true, 1_000, 500);
        let now = sample(0, true, 1_000 + 250_000_000, 500 + 750_000_000);
        let delta = diff_cpu(&now, &then, 1_000_000_000);
        assert_eq!(delta.idle_delta, 250_000_000);
        assert_eq!(delta.busy_delta, 750_000_000);
        assert_eq!(delta.load_delta, 750_000_000);
        assert_eq!(format_percent(delta.load_delta, 1_000_000_000), "75.00%");
    }

    #[test]
    fn hints_all_saturated() {
        let interval = 1_000_000_000;
        let deltas = vec![
            CpuDelta { cpu_number: 0, online: true, load_delta: 950_000_000, ..CpuDelta::default() },
            CpuDelta { cpu_number: 1, online: true, load_delta: 920_000_000, ..CpuDelta::default() },
        ];
        let notes = hints(&deltas, interval);
        assert!(notes.iter().any(|note| note.contains("CPU-bound")));
    }

    #[test]
    fn hints_imbalance() {
        let interval = 1_000_000_000;
        let deltas = vec![
            CpuDelta { cpu_number: 0, online: true, load_delta: 950_000_000, ..CpuDelta::default() },
            CpuDelta { cpu_number: 1, online: true, load_delta: 10_000_000, ..CpuDelta::default() },
        ];
        let notes = hints(&deltas, interval);
        assert!(notes.iter().any(|note| note.contains("parallelism")));
    }

    #[test]
    fn hints_interrupt_heavy() {
        let interval = 1_000_000_000;
        let deltas = vec![CpuDelta {
            cpu_number: 0,
            online: true,
            load_delta: 200_000_000,
            ints: 20_000,
            syscalls: 100,
            ..CpuDelta::default()
        }];
        let notes = hints(&deltas, interval);
        assert!(notes.iter().any(|note| note.contains("interrupt-heavy")));
    }

    #[test]
    fn write_report_lists_identity_and_table() {
        let report = Report {
            identity: Identity {
                cpu_count: 2,
                ticks_per_second: 24_000_000,
                page_size: 4096,
                version: "git:test".to_string(),
            },
            interval_nanos: 1_000_000_000,
            deltas: vec![CpuDelta {
                cpu_number: 0,
                online: true,
                load_delta: 250_000_000,
                busy_delta: 300_000_000,
                context_switches: 10,
                syscalls: 40,
                ints: 5,
                ..CpuDelta::default()
            }],
        };
        let mut output = Vec::new();
        write_report(&mut output, &report, true).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("cpus         2  (online 1, offline 0)"));
        assert!(text.contains("ticks/s      24000000"));
        assert!(text.contains("interval     1.000s"));
        assert!(text.contains("  0  online"));
        assert!(text.contains("25.00%"));
        assert!(!text.contains("hints:"));
    }
}
