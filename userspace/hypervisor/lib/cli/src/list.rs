// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::platform::PlatformServices;
use anyhow::{Error, anyhow};
use fidl_fuchsia_virtualization::{GuestListEntry, GuestManagerProxy, GuestStatus, KernelType};
use guest_cli_args as arguments;
use prettytable::{Table, row};
use std::fmt;

fn guest_status_to_string(status: GuestStatus) -> &'static str {
    match status {
        GuestStatus::NotStarted => "Not started",
        GuestStatus::Starting => "Starting",
        GuestStatus::Running => "Running",
        GuestStatus::Stopping => "Stopping",
        GuestStatus::Stopped => "Stopped",
        GuestStatus::VmmUnexpectedTermination => "VMM Unexpectedly Terminated",
    }
}

fn uptime_to_string(uptime_nanos: Option<i64>) -> String {
    match uptime_nanos {
        Some(uptime) => {
            if uptime < 0 {
                "Invalid negative uptime!".to_string()
            } else {
                let uptime = std::time::Duration::from_nanos(uptime as u64);
                let seconds = uptime.as_secs() % 60;
                let minutes = (uptime.as_secs() / 60) % 60;
                let hours = uptime.as_secs() / 3600;
                format!("{:0>2}:{:0>2}:{:0>2} HH:MM:SS", hours, minutes, seconds)
            }
        }
        None => "--:--:-- HH:MM:SS".to_string(),
    }
}

fn kernel_type_to_string(kernel_type: KernelType) -> &'static str {
    match kernel_type {
        KernelType::Zircon => "Zircon",
        KernelType::Linux => "Linux",
        KernelType::Rtos => "Rtos",
    }
}

fn format_memory_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB && bytes % GIB == 0 {
        format!("{} GiB ({} bytes)", bytes / GIB, bytes)
    } else if bytes >= MIB && bytes % MIB == 0 {
        format!("{} MiB ({} bytes)", bytes / MIB, bytes)
    } else if bytes >= KIB && bytes % KIB == 0 {
        format!("{} KiB ({} bytes)", bytes / KIB, bytes)
    } else if bytes >= GIB {
        format!("{:.2} GiB ({} bytes)", bytes as f64 / GIB as f64, bytes)
    } else if bytes >= MIB {
        format!("{:.2} MiB ({} bytes)", bytes as f64 / MIB as f64, bytes)
    } else {
        format!("{} bytes", bytes)
    }
}

fn format_mac_address(octets: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        octets[0], octets[1], octets[2], octets[3], octets[4], octets[5]
    )
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct GuestDetails {
    pub vmid: String,
    pub package_url: String,
    pub status: String,
    pub uptime_nanos: i64,
    pub stop_reason: Option<String>,
    pub kernel_type: Option<String>,
    pub cmdline: Option<String>,
    pub cmdline_add: Vec<String>,
    pub cpu_count: Option<u8>,
    pub memory_bytes: Option<u64>,
    pub device_counts: Vec<(String, u32)>,
    pub networks: Vec<String>,
    pub problems: Vec<String>,
}

fn device_names(device_counts: &[(String, u32)], active: bool) -> Vec<String> {
    let names: Vec<String> = device_counts
        .iter()
        .filter(|(_, count)| if active { *count > 0 } else { *count == 0 })
        .map(|(device, count)| {
            if *count > 1 {
                format!("{device} ({count} devices)")
            } else {
                device.clone()
            }
        })
        .collect();
    if names.is_empty() { vec!["None".to_string()] } else { names }
}

fn write_aligned_fields(
    f: &mut fmt::Formatter<'_>,
    fields: &[(String, Vec<String>)],
) -> fmt::Result {
    let width = fields.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    for (label, values) in fields {
        let mut values = values.iter();
        let first = values.next().map(String::as_str).unwrap_or("");
        writeln!(f, "{label:<width$}  {first}")?;
        for value in values {
            writeln!(f, "{:width$}  {value}", "")?;
        }
    }
    Ok(())
}

impl fmt::Display for GuestDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fields = vec![
            ("Guest vmid:".to_string(), vec![self.vmid.clone()]),
            ("Guest package:".to_string(), vec![self.package_url.clone()]),
            ("Guest status:".to_string(), vec![self.status.clone()]),
            ("Guest uptime:".to_string(), vec![uptime_to_string(Some(self.uptime_nanos))]),
        ];

        if self.status != "Not started" {
            if let Some(stop_reason) = &self.stop_reason {
                fields.push(("Stop reason:".to_string(), vec![stop_reason.clone()]));
            }
            if let Some(kernel_type) = &self.kernel_type {
                fields.push(("Kernel type:".to_string(), vec![kernel_type.clone()]));
            }
            if let Some(cmdline) = &self.cmdline {
                fields.push(("Kernel cmdline:".to_string(), vec![cmdline.clone()]));
            }
            if !self.cmdline_add.is_empty() {
                fields.push(("Cmdline extras:".to_string(), vec![self.cmdline_add.join(" ")]));
            }
            if let Some(cpu_count) = self.cpu_count {
                fields.push(("CPU count:".to_string(), vec![cpu_count.to_string()]));
            }
            if let Some(memory_bytes) = self.memory_bytes {
                fields.push(("Guest memory:".to_string(), vec![format_memory_bytes(memory_bytes)]));
            }
            if !self.device_counts.is_empty() {
                fields.push(("Active devices:".to_string(), device_names(&self.device_counts, true)));
                fields.push((
                    "Inactive devices:".to_string(),
                    device_names(&self.device_counts, false),
                ));
            }
            if !self.device_counts.is_empty() || !self.networks.is_empty() || self.cpu_count.is_some()
            {
                let networks = if self.networks.is_empty() {
                    vec!["None".to_string()]
                } else {
                    self.networks.clone()
                };
                fields.push(("Networks:".to_string(), networks));
            }
        }

        write_aligned_fields(f, &fields)?;

        if !self.problems.is_empty() {
            writeln!(f)?;
            writeln!(
                f,
                "{} problem{} detected:",
                self.problems.len(),
                if self.problems.len() > 1 { "s" } else { "" }
            )?;
            for problem in &self.problems {
                writeln!(f, "* {problem}")?;
            }
        }
        Ok(())
    }
}

async fn get_detailed_information(
    vmid: arguments::Vmid,
    manager: GuestManagerProxy,
) -> Result<GuestDetails, Error> {
    let guest_info = manager.get_info(vmid.get()).await;
    if let Err(_) = guest_info {
        return Err(anyhow!("Failed to query guest information: {}", vmid));
    }
    let guest_info = guest_info.unwrap();
    let guest_status = guest_info.guest_status.expect("guest status should always be set");

    let mut details: GuestDetails = Default::default();
    details.vmid = vmid.to_string();
    details.package_url = arguments::GUEST_PACKAGE_URL.to_string();
    details.status = guest_status_to_string(guest_status).to_string();
    details.uptime_nanos = guest_info.uptime.unwrap_or(0);

    if guest_status == GuestStatus::NotStarted {
        return Ok(details);
    }

    if guest_status == GuestStatus::Stopped {
        let stop_reason = guest_info
            .stop_error
            .map_or_else(|| "Clean shutdown".to_string(), |err| format!("{:?}", err));
        details.stop_reason = Some(stop_reason);
    }

    if let Some(config) = guest_info.guest_descriptor {
        details.cpu_count = config.num_cpus;
        details.memory_bytes = config.guest_memory;
        details.kernel_type = config.kernel_type.map(kernel_type_to_string).map(str::to_string);
        details.cmdline = config.cmdline.filter(|cmdline| !cmdline.is_empty());
        details.cmdline_add = config.cmdline_add.unwrap_or_default();

        let add_to_table =
            |device: &str, is_active: Option<bool>, table: &mut Vec<(String, u32)>| {
                let count = is_active.map(|active| active as u32).unwrap_or(0);
                table.push((device.to_string(), count));
            };

        add_to_table("console", config.console, &mut details.device_counts);
        add_to_table("rng", config.rng, &mut details.device_counts);
        add_to_table("balloon", config.balloon, &mut details.device_counts);
        add_to_table("vsock", config.vsock, &mut details.device_counts);
        add_to_table("gpu", config.gpu, &mut details.device_counts);
        add_to_table("sound", config.sound, &mut details.device_counts);
        add_to_table("mem", config.mem, &mut details.device_counts);

        details.networks = config
            .networks
            .unwrap_or_default()
            .into_iter()
            .map(|network| {
                format!(
                    "mac {} bridge={}",
                    format_mac_address(&network.mac_address.octets),
                    network.enable_bridge
                )
            })
            .collect();
    }

    if let Some(problems) = guest_info.detected_problems {
        details.problems = problems;
    }
    Ok(details)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct GuestOverview {
    name: String,
    status: String,
    uptime_nanos: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct GuestSummary {
    guests: Vec<GuestOverview>,
}

impl fmt::Display for GuestSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut table = Table::new();
        table.set_titles(row!["VMID", "Status", "Uptime"]);
        for guest in &self.guests {
            table.add_row(row![guest.name, guest.status, uptime_to_string(guest.uptime_nanos)]);
        }
        write!(f, "{}", table)
    }
}

fn list_entry_to_overview(entry: GuestListEntry, stop_reason: Option<String>) -> GuestOverview {
    let status = guest_status_to_string(
        entry.guest_status.expect("guest status should always be set"),
    )
    .to_string();
    let status = match stop_reason {
        Some(reason) => format!("{} ({})", status, reason),
        None => status,
    };
    GuestOverview {
        name: format!("vmid={}", entry.vmid.unwrap_or(0)),
        status,
        uptime_nanos: entry.uptime,
    }
}

async fn get_environment_summary(manager: GuestManagerProxy) -> Result<GuestSummary, Error> {
    let guests = manager.list().await.map_err(|err| anyhow!("Failed to list guests: {}", err))?;
    let mut overviews = Vec::new();
    for entry in guests {
        let vmid = entry.vmid.unwrap_or(0);
        let stop_reason = if entry.guest_status == Some(GuestStatus::Stopped) {
            match manager.get_info(vmid).await {
                Ok(info) => info.stop_error.map(|err| format!("{:?}", err)),
                Err(_) => None,
            }
        } else {
            None
        };
        overviews.push(list_entry_to_overview(entry, stop_reason));
    }
    Ok(GuestSummary { guests: overviews })
}

#[derive(serde::Serialize, serde::Deserialize)]
pub enum GuestList {
    Summary(GuestSummary),
    Details(GuestDetails),
}

impl fmt::Display for GuestList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuestList::Summary(summary) => write!(f, "{}", summary)?,
            GuestList::Details(details) => write!(f, "{}", details)?,
        }
        Ok(())
    }
}

pub async fn handle_list<P: PlatformServices>(
    services: &P,
    args: &arguments::list_args::ListArgs,
) -> Result<GuestList, Error> {
    let manager = services.connect_to_manager().await?;
    match args.vmid {
        Some(vmid) => Ok(GuestList::Details(get_detailed_information(vmid, manager).await?)),
        None => Ok(GuestList::Summary(get_environment_summary(manager).await?)),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_virtualization::{
        GuestDescriptor, GuestError, GuestInfo, GuestListEntry, GuestManagerMarker, KernelType,
    };
    use fuchsia_async as fasync;
    use futures::StreamExt;

    fn serve_mock_manager(response: Option<GuestInfo>) -> GuestManagerProxy {
        let (proxy, mut stream) = create_proxy_and_stream::<GuestManagerMarker>();
        fasync::Task::local(async move {
            let (_vmid, responder) = stream
                .next()
                .await
                .expect("mock manager expected a request")
                .unwrap()
                .into_get_info()
                .expect("unexpected call to mock manager");

            if let Some(guest_info) = response {
                responder.send(&guest_info).expect("failed to send mock response");
            } else {
                drop(responder);
            }
        })
        .detach();

        proxy
    }

    fn serve_mock_list(entries: Vec<GuestListEntry>) -> GuestManagerProxy {
        let (proxy, mut stream) = create_proxy_and_stream::<GuestManagerMarker>();
        fasync::Task::local(async move {
            let responder = stream
                .next()
                .await
                .expect("mock manager expected a request")
                .unwrap()
                .into_list()
                .expect("unexpected call to mock manager");
            responder.send(&entries).expect("failed to send mock response");
        })
        .detach();
        proxy
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn negative_uptime() {
        // Note that a negative duration should never happen as we're measuring duration
        // monotonically from a single process.
        let duration = -5;
        let actual = uptime_to_string(Some(duration));
        let expected = "Invalid negative uptime!";

        assert_eq!(actual, expected);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn very_large_uptime() {
        let hours = std::time::Duration::from_secs(123 * 60 * 60);
        let minutes = std::time::Duration::from_secs(45 * 60);
        let seconds = std::time::Duration::from_secs(54);
        let duration = hours + minutes + seconds;

        let actual = uptime_to_string(Some(duration.as_nanos() as i64));
        let expected = "123:45:54 HH:MM:SS";

        assert_eq!(actual, expected);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn summarize_existing_managers() {
        let manager = serve_mock_list(vec![
            GuestListEntry {
                vmid: Some(0),
                guest_status: Some(GuestStatus::Running),
                uptime: Some(std::time::Duration::from_secs(123).as_nanos() as i64),
                ..Default::default()
            },
            GuestListEntry {
                vmid: Some(1),
                guest_status: Some(GuestStatus::Stopped),
                uptime: Some(std::time::Duration::from_secs(5).as_nanos() as i64),
                ..Default::default()
            },
        ]);

        let actual = format!("{}", get_environment_summary(manager).await.unwrap());
        let expected = concat!(
            "+--------+---------+-------------------+\n",
            "| VMID   | Status  | Uptime            |\n",
            "+========+=========+===================+\n",
            "| vmid=0 | Running | 00:02:03 HH:MM:SS |\n",
            "+--------+---------+-------------------+\n",
            "| vmid=1 | Stopped | 00:00:05 HH:MM:SS |\n",
            "+--------+---------+-------------------+\n"
        );

        assert_eq!(actual, expected);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn get_detailed_info_stopped_clean() {
        let manager = serve_mock_manager(Some(GuestInfo {
            guest_status: Some(GuestStatus::Stopped),
            uptime: Some(std::time::Duration::from_secs(5).as_nanos() as i64),
            ..Default::default()
        }));

        let actual = format!(
            "{}",
            get_detailed_information(arguments::Vmid(0), manager).await.unwrap()
        );
        assert!(actual.contains("vmid=0"));
        assert!(actual.contains("Stopped"));
        assert!(actual.contains("00:00:05 HH:MM:SS"));
        assert!(actual.contains("Clean shutdown"));
        assert!(!actual.contains("CPU count:"));
        assert!(!actual.contains("Networks:"));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn get_detailed_info_stopped_guest_failure() {
        let manager = serve_mock_manager(Some(GuestInfo {
            guest_status: Some(GuestStatus::Stopped),
            uptime: Some(std::time::Duration::from_secs(65).as_nanos() as i64),
            stop_error: Some(GuestError::InternalError),
            ..Default::default()
        }));

        let actual = format!(
            "{}",
            get_detailed_information(arguments::Vmid(1), manager).await.unwrap()
        );
        assert!(actual.contains("vmid=1"));
        assert!(actual.contains("Stopped"));
        assert!(actual.contains("00:01:05 HH:MM:SS"));
        assert!(actual.contains("InternalError"));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn get_detailed_info_running_guest() {
        let manager = serve_mock_manager(Some(GuestInfo {
            guest_status: Some(GuestStatus::Running),
            uptime: Some(std::time::Duration::from_secs(125 * 60).as_nanos() as i64),
            guest_descriptor: Some(GuestDescriptor {
                num_cpus: Some(4),
                guest_memory: Some(1073741824),
                kernel_type: Some(KernelType::Rtos),
                cmdline: Some("console=ttyS0".to_string()),
                cmdline_add: Some(vec!["earlycon=pl011".to_string()]),
                balloon: Some(true),
                console: Some(true),
                rng: Some(true),
                vsock: Some(true),
                gpu: Some(false),
                sound: Some(false),
                mem: Some(false),
                ..Default::default()
            }),
            detected_problems: Some(vec![
                "Host is experiencing heavy memory pressure".to_string(),
                "No bridge between guest and host network interaces".to_string(),
            ]),
            ..Default::default()
        }));

        let actual = format!(
            "{}",
            get_detailed_information(arguments::Vmid(0), manager).await.unwrap()
        );
        assert!(actual.contains("vmid=0"));
        assert!(actual.contains("Running"));
        assert!(actual.contains("02:05:00 HH:MM:SS"));
        assert!(actual.contains("Kernel type:"));
        assert!(actual.contains("Rtos"));
        assert!(actual.contains("Kernel cmdline:"));
        assert!(actual.contains("console=ttyS0"));
        assert!(actual.contains("Cmdline extras:"));
        assert!(actual.contains("earlycon=pl011"));
        assert!(actual.contains("CPU count:"));
        assert!(actual.contains("4"));
        assert!(actual.contains("1 GiB (1073741824 bytes)"));
        assert!(actual.contains("console"));
        assert!(actual.contains("balloon"));
        assert!(actual.contains("gpu"));
        assert!(actual.contains("sound"));
        assert!(actual.contains("Networks:"));
        assert!(actual.contains("Host is experiencing heavy memory pressure"));
        assert!(actual.contains("No bridge between guest and host network interaces"));
    }

    #[fuchsia::test]
    fn memory_display_uses_largest_whole_unit() {
        assert_eq!(format_memory_bytes(72 * 1024 * 1024), "72 MiB (75497472 bytes)");
        assert_eq!(format_memory_bytes(1073741824), "1 GiB (1073741824 bytes)");
        assert_eq!(format_memory_bytes(512), "512 bytes");
    }

    #[fuchsia::test]
    fn details_values_share_one_column() {
        let details = GuestDetails {
            vmid: "vmid=0".to_string(),
            package_url: arguments::GUEST_PACKAGE_URL.to_string(),
            status: "Running".to_string(),
            uptime_nanos: 0,
            kernel_type: Some("Rtos".to_string()),
            cpu_count: Some(2),
            memory_bytes: Some(72 * 1024 * 1024),
            device_counts: vec![
                ("console".to_string(), 1),
                ("rng".to_string(), 0),
                ("balloon".to_string(), 0),
                ("vsock".to_string(), 0),
                ("gpu".to_string(), 0),
                ("sound".to_string(), 0),
                ("mem".to_string(), 0),
            ],
            ..Default::default()
        };
        let actual = format!("{details}");
        assert!(actual.contains("guest_pkg.cm"));
        let value_column = "Inactive devices:".len() + 2;
        for line in actual.lines() {
            assert_eq!(&line[value_column - 2..value_column], "  ");
            assert_ne!(line.as_bytes()[value_column], b' ');
        }
    }
}
