// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! In-process `fuchsia.hardware.tee.DeviceConnector` for Secular's Trusty BL32.
//!
//! Fuchsia's tee_manager talks to an OP-TEE driver at `/dev/class/tee`. Secular
//! has no OP-TEE driver; this module answers DeviceInfo from the Trusty SPD
//! UUID SMC. Application sessions are not supported until a TA IPC path exists.

use anyhow::{Context as _, Error, format_err};
use fidl_fuchsia_hardware_tee::{
    DeviceConnectorMarker, DeviceConnectorProxy, DeviceConnectorRequest,
};
use fidl_fuchsia_kernel as fkernel;
use fidl_fuchsia_tee as fuchsia_tee;
use fuchsia_async as fasync;
use futures::TryStreamExt as _;

/// SMCCC Trusted OS UID query (fast, SMC32, entity 63, function 0xFF01).
const SMC_FC_GET_UUID: u32 = 0xbf00ff01;
const SMC_UNK: u64 = 0xffff_ffff;

pub async fn spawn_trusty_device_connector() -> Result<DeviceConnectorProxy, Error> {
    let os_info = query_trusty_os_info().await?;
    log::info!(
        "Trusty OS uuid {:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        os_info.uuid.as_ref().map(|u| u.time_low).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.time_mid).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.time_hi_and_version).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[0]).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[1]).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[2]).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[3]).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[4]).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[5]).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[6]).unwrap_or_default(),
        os_info.uuid.as_ref().map(|u| u.clock_seq_and_node[7]).unwrap_or_default(),
    );

    let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<DeviceConnectorMarker>();
    fasync::Task::local(async move {
        if let Err(e) = serve_device_connector(os_info, stream).await {
            log::error!("Trusty DeviceConnector exited: {e:?}");
        }
    })
    .detach();
    Ok(proxy)
}

async fn query_trusty_os_info() -> Result<fuchsia_tee::OsInfo, Error> {
    let proxy = fuchsia_component::client::connect_to_protocol::<fkernel::SmcResourceMarker>()
        .context("connect fuchsia.kernel.SmcResource")?;
    let resource = proxy.get().await.context("SmcResource.Get")?;

    let mut params = zx::sys::zx_smc_parameters_t::default();
    params.func_id = SMC_FC_GET_UUID;
    let mut result = zx::sys::zx_smc_result_t::default();
    zx::ok(unsafe { zx::sys::zx_smc_call(resource.raw_handle(), &params, &mut result) })
        .context("zx_smc_call GET_UUID")?;
    if result.arg0 == SMC_UNK {
        return Err(format_err!("Trusty GET_UUID returned SMC_UNK"));
    }

    Ok(fuchsia_tee::OsInfo {
        uuid: Some(smccc_uuid_to_tee_uuid(
            result.arg0 as u32,
            result.arg1 as u32,
            result.arg2 as u32,
            result.arg3 as u32,
        )),
        is_global_platform_compliant: Some(false),
        ..Default::default()
    })
}

/// SMCCC UUID words store RFC-4122 bytes with byte 0 in the low 8 bits of each word.
pub fn smccc_uuid_to_tee_uuid(w0: u32, w1: u32, w2: u32, w3: u32) -> fuchsia_tee::Uuid {
    let b = [
        w0 as u8,
        (w0 >> 8) as u8,
        (w0 >> 16) as u8,
        (w0 >> 24) as u8,
        w1 as u8,
        (w1 >> 8) as u8,
        (w1 >> 16) as u8,
        (w1 >> 24) as u8,
        w2 as u8,
        (w2 >> 8) as u8,
        (w2 >> 16) as u8,
        (w2 >> 24) as u8,
        w3 as u8,
        (w3 >> 8) as u8,
        (w3 >> 16) as u8,
        (w3 >> 24) as u8,
    ];
    fuchsia_tee::Uuid {
        time_low: u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        time_mid: u16::from_be_bytes([b[4], b[5]]),
        time_hi_and_version: u16::from_be_bytes([b[6], b[7]]),
        clock_seq_and_node: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
    }
}

async fn serve_device_connector(
    os_info: fuchsia_tee::OsInfo,
    mut stream: fidl_fuchsia_hardware_tee::DeviceConnectorRequestStream,
) -> Result<(), Error> {
    while let Some(request) = stream.try_next().await.context("DeviceConnector stream")? {
        match request {
            DeviceConnectorRequest::ConnectToDeviceInfo { device_info_request, control_handle: _ } => {
                let os_info = os_info.clone();
                fasync::Task::local(async move {
                    let stream = device_info_request.into_stream();
                    let _ = serve_device_info(os_info, stream).await;
                })
                .detach();
            }
            DeviceConnectorRequest::ConnectToApplication {
                application_uuid: _,
                service_provider,
                application_request,
                control_handle: _,
            } => {
                drop(service_provider);
                let _ = application_request.close_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
        }
    }
    Ok(())
}

async fn serve_device_info(
    os_info: fuchsia_tee::OsInfo,
    mut stream: fuchsia_tee::DeviceInfoRequestStream,
) -> Result<(), Error> {
    while let Some(request) = stream.try_next().await.context("DeviceInfo stream")? {
        match request {
            fuchsia_tee::DeviceInfoRequest::GetOsInfo { responder } => {
                responder.send(&os_info).context("GetOsInfo reply")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusty_spd_uuid_words() {
        // ktee packed words for 40ee25f0-a2bc-304c-8c4c-a173c57d8af1
        let uuid = smccc_uuid_to_tee_uuid(0xf025ee40, 0x4c30bca2, 0x73a14c8c, 0xf18a7dc5);
        assert_eq!(uuid.time_low, 0x40ee25f0);
        assert_eq!(uuid.time_mid, 0xa2bc);
        assert_eq!(uuid.time_hi_and_version, 0x304c);
        assert_eq!(
            uuid.clock_seq_and_node,
            [0x8c, 0x4c, 0xa1, 0x73, 0xc5, 0x7d, 0x8a, 0xf1]
        );
    }
}
