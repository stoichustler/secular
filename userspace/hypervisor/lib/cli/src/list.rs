// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::platform::PlatformServices;
use anyhow::{Error, anyhow};
use fidl_fuchsia_virtualization::{GuestListEntry, GuestManagerProxy, GuestStatus};
use guest_cli_args as arguments;
use prettytable::format::consts::FORMAT_CLEAN;
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

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct GuestDetails {
    pub vmid: String,
    pub package_url: String,
    pub status: String,
    pub uptime_nanos: i64,
    pub stop_reason: Option<String>,
    pub cpu_count: Option<u8>,
    pub memory_bytes: Option<u64>,
    pub device_counts: Vec<(String, u32)>,
    pub problems: Vec<String>,
}

impl fmt::Display for GuestDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut table = Table::new();
        table.set_format(*FORMAT_CLEAN);
        table.add_row(row!["Guest vmid:", self.vmid]);
        table.add_row(row!["Guest package:", self.package_url]);
        table.add_row(row!["Guest status:", self.status]);
        table.add_row(row!["Guest uptime:", uptime_to_string(Some(self.uptime_nanos))]);

        if self.status == "Not started" {
            write!(f, "{}", table)?;
            return Ok(());
        }

        table.add_empty_row();

        if let Some(stop_reason) = &self.stop_reason {
            table.add_row(row!["Stop reason:", stop_reason]);
        }
        if let Some(cpu_count) = self.cpu_count {
            table.add_row(row!["CPU count:", cpu_count]);
        }
        if let Some(memory_bytes) = self.memory_bytes {
            let gib = f64::trunc(memory_bytes as f64 / (1 << 30) as f64) * 100.0 / 100.0;
            table.add_row(row!["Guest memory:", format!("{} GiB ({} bytes)", gib, memory_bytes)]);
        }

        if self.cpu_count.is_some() && self.memory_bytes.is_some() {
            table.add_empty_row();

            let mut active = Table::new();
            active.set_format(*FORMAT_CLEAN);
            let mut inactive = Table::new();
            inactive.set_format(*FORMAT_CLEAN);

            for (device, count) in self.device_counts.iter() {
                if *count == 0 {
                    inactive.add_row(row![device]);
                } else if *count == 1 {
                    active.add_row(row![device]);
                } else {
                    active.add_row(row![format!("{} ({} devices)", device, *count)]);
                }
            }

            if active.len() == 0 {
                active.add_row(row!["None"]);
            }

            if inactive.len() == 0 {
                inactive.add_row(row!["None"]);
            }

            table.add_row(row!["Active devices:", active]);
            table.add_empty_row();
            table.add_row(row!["Inactive devices:", inactive]);
        }
        write!(f, "{}", table)?;

        if !self.problems.is_empty() {
            let mut problem_table = Table::new();
            problem_table.set_format(*FORMAT_CLEAN);
            problem_table.add_empty_row();
            problem_table.add_row(row![
                format!(
                    "{} problem{} detected:",
                    self.problems.len(),
                    if self.problems.len() > 1 { "s" } else { "" }
                ),
                " "
            ]);
            for problem in self.problems.iter() {
                problem_table.add_row(row![format!("* {}", problem), " "]);
            }
            write!(f, "{}", problem_table)?;
        }
        return Ok(());
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
    } else {
        if let Some(config) = guest_info.guest_descriptor {
            details.cpu_count = config.num_cpus;
            details.memory_bytes = config.guest_memory;

            let add_to_table =
                |device: &str, is_active: Option<bool>, table: &mut Vec<(String, u32)>| -> () {
                    let count = is_active.map(|b| b as u32).unwrap_or(0);
                    table.push((device.to_string(), count));
                };

            add_to_table("balloon", config.balloon, &mut details.device_counts);
            add_to_table("console", config.console, &mut details.device_counts);
            add_to_table("rng", config.rng, &mut details.device_counts);
            add_to_table("vsock", config.vsock, &mut details.device_counts);
            add_to_table("mem", config.mem, &mut details.device_counts);
        }
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

fn list_entry_to_overview(entry: GuestListEntry) -> GuestOverview {
    GuestOverview {
        name: format!("vmid={}", entry.vmid.unwrap_or(0)),
        status: guest_status_to_string(
            entry.guest_status.expect("guest status should always be set"),
        )
        .to_string(),
        uptime_nanos: entry.uptime,
    }
}

async fn get_environment_summary(manager: GuestManagerProxy) -> Result<GuestSummary, Error> {
    let guests = manager.list().await.map_err(|err| anyhow!("Failed to list guests: {}", err))?;
    Ok(GuestSummary { guests: guests.into_iter().map(list_entry_to_overview).collect() })
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
        GuestDescriptor, GuestError, GuestInfo, GuestListEntry, GuestManagerMarker,
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
        let expected = concat!(
            " Guest vmid:     vmid=0 \n",
            " Guest package:  fuchsia-boot:///guest_manager#meta/secular_guest_pkg.cm \n",
            " Guest status:   Stopped \n",
            " Guest uptime:   00:00:05 HH:MM:SS \n",
            "                  \n",
            " Stop reason:    Clean shutdown \n",
        );

        assert_eq!(actual, expected);
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
        let expected = concat!(
            " Guest vmid:     vmid=1 \n",
            " Guest package:  fuchsia-boot:///guest_manager#meta/secular_guest_pkg.cm \n",
            " Guest status:   Stopped \n",
            " Guest uptime:   00:01:05 HH:MM:SS \n",
            "                  \n",
            " Stop reason:    InternalError \n",
        );

        assert_eq!(actual, expected);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn get_detailed_info_running_guest() {
        let manager = serve_mock_manager(Some(GuestInfo {
            guest_status: Some(GuestStatus::Running),
            uptime: Some(std::time::Duration::from_secs(125 * 60).as_nanos() as i64),
            guest_descriptor: Some(GuestDescriptor {
                num_cpus: Some(4),
                guest_memory: Some(1073741824),
                balloon: Some(true),
                console: Some(true),
                rng: Some(true),
                vsock: Some(true),
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
        let expected = concat!(
            " Guest vmid:        vmid=0 \n",
            " Guest package:     fuchsia-boot:///guest_manager#meta/secular_guest_pkg.cm \n",
            " Guest status:      Running \n",
            " Guest uptime:      02:05:00 HH:MM:SS \n",
            "                     \n",
            " CPU count:         4 \n",
            " Guest memory:      1 GiB (1073741824 bytes) \n",
            "                     \n",
            " Active devices:     balloon  \n",
            "                     console  \n",
            "                     rng  \n",
            "                     vsock  \n",
            "                     \n",
            " Inactive devices:   mem  \n",
            "                                                        \n",
            " 2 problems detected:                                    \n",
            " * Host is experiencing heavy memory pressure            \n",
            " * No bridge between guest and host network interaces    \n",
        );

        assert_eq!(actual, expected);
    }
}
