// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/virtualization/bin/guest_manager/guest_manager.h"

#include <fuchsia/net/interfaces/cpp/fidl.h>
#include <fuchsia/virtualization/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <unordered_set>

#include <src/lib/files/file.h>

#include "src/virtualization/bin/guest_manager/memory_pressure_handler.h"

namespace {

uint64_t GetDefaultGuestMemory() {
  const uint64_t host_memory = zx_system_get_physmem();
  const uint64_t max_reserved_host_memory = 3 * (1ul << 30);  // 3 GiB.

  // Reserve half the host memory up to 2 GiB, and allow the rest to be used by the guest.
  return host_memory - std::min(host_memory / 2, max_reserved_host_memory);
}

uint8_t GetDefaultNumCpus() {
  return static_cast<uint8_t>(std::min<uint32_t>(zx_system_get_num_cpus(), UINT8_MAX));
}

}  // namespace

using ::fuchsia::virtualization::GuestConfig;
using ::fuchsia::virtualization::GuestDescriptor;
using ::fuchsia::virtualization::GuestError;
using ::fuchsia::virtualization::GuestLifecycle_Create_Result;
using ::fuchsia::virtualization::GuestLifecycle_Run_Result;
using ::fuchsia::virtualization::GuestManagerError;
using ::fuchsia::virtualization::GuestStatus;

GuestManager::GuestManager(async_dispatcher_t* dispatcher, sys::ComponentContext* context,
                           std::string config_pkg_dir_path, std::string config_path)
    : dispatcher_(dispatcher),
      context_(context),
      config_pkg_dir_path_(std::move(config_pkg_dir_path)),
      config_path_(std::move(config_path)) {
  context_->outgoing()->AddPublicService(manager_bindings_.GetHandler(this));
}

fit::result<GuestManagerError, GuestConfig> GuestManager::GetDefaultGuestConfig() {
  // Reads guest config from the guest_pkg child (data/guest.cfg).
  const std::string config_path = config_pkg_dir_path_ + config_path_;
  auto open_at = [&](const std::string& path, fidl::InterfaceRequest<fuchsia::io::File> file) {
    return fdio_open3((config_pkg_dir_path_ + path).c_str(),
                      static_cast<uint64_t>(fuchsia::io::PERM_READABLE),
                      file.TakeChannel().release());
  };

  std::string content;
  bool readFileSuccess = files::ReadFileToString(config_path, &content);
  if (!readFileSuccess) {
    FX_LOGS(ERROR) << "Failed to read guest configuration " << config_path;
    return fit::error(GuestManagerError::BAD_CONFIG);
  }
  auto config = guest_config::ParseConfig(content, std::move(open_at));
  if (config.is_error()) {
    FX_PLOGS(ERROR, config.error_value()) << "Failed to parse guest configuration " << config_path;
    return fit::error(GuestManagerError::BAD_CONFIG);
  }

  return fit::ok(std::move(*config));
}

bool GuestManager::GuestInstance::is_started() const {
  switch (state) {
    case GuestStatus::STARTING:
    case GuestStatus::RUNNING:
    case GuestStatus::STOPPING:
      return true;
    default:
      return false;
  }
}

GuestManager::GuestInstance& GuestManager::GetOrCreateInstance(uint32_t vmid) {
  auto& instance = guests_[vmid];
  if (!instance) {
    instance = std::make_unique<GuestInstance>();
    instance->vmid = vmid;
  }
  return *instance;
}

GuestManager::GuestInstance* GuestManager::FindInstance(uint32_t vmid) {
  auto it = guests_.find(vmid);
  return it == guests_.end() ? nullptr : it->second.get();
}

const GuestManager::GuestInstance* GuestManager::FindInstance(uint32_t vmid) const {
  auto it = guests_.find(vmid);
  return it == guests_.end() ? nullptr : it->second.get();
}

// |fuchsia::virtualization::GuestManager|
void GuestManager::Launch(uint32_t vmid, GuestConfig user_config,
                          fidl::InterfaceRequest<fuchsia::virtualization::Guest> controller,
                          LaunchCallback callback) {
  GuestInstance& instance = GetOrCreateInstance(vmid);
  if (instance.is_started()) {
    callback(fpromise::error(GuestManagerError::ALREADY_RUNNING));
    return;
  }

  if (!instance.lifecycle.is_bound()) {
    // Opening the lifecycle channel will start the VMM component, and closing the channel will
    // destroy the VMM component.
    //
    // This error handler is only invoked if the server side of the channel is closed, not if the
    // guest manager closes the channel to destroy the VMM component. As the VMM component will
    // never intentionally close the channel, this channel closing means that the component has
    // terminated unexpectedly.
    context_->svc()->Connect(instance.lifecycle.NewRequest());
    instance.lifecycle.set_error_handler([this, vmid](zx_status_t) {
      GuestInstance* crashed = FindInstance(vmid);
      if (crashed == nullptr) {
        return;
      }
      crashed->state = GuestStatus::VMM_UNEXPECTED_TERMINATION;
      OnGuestStopped();
    });
  }

  auto default_config = GetDefaultGuestConfig();
  if (default_config.is_error()) {
    callback(fpromise::error(default_config.error_value()));
    return;
  }

  // Use the static config as a base, but apply the user config as an override.
  GuestConfig merged_cfg;
  merged_cfg = guest_config::MergeConfigs(std::move(*default_config), std::move(user_config));
  if (!merged_cfg.has_guest_memory()) {
    merged_cfg.set_guest_memory(GetDefaultGuestMemory());
  }
  if (!merged_cfg.has_cpus()) {
    merged_cfg.set_cpus(GetDefaultNumCpus());
  }

  // TODO(b/410037697): Re-enable networking.
  if (merged_cfg.has_default_net() && merged_cfg.default_net()) {
    FX_LOGS(ERROR) << "b/410037697: Networking is not currently supported for guests.";
    callback(fpromise::error(GuestManagerError::BAD_CONFIG));
    return;
  }

  // Merge the command-line additions into the main kernel command-line field.
  for (auto& cmdline : *merged_cfg.mutable_cmdline_add()) {
    merged_cfg.mutable_cmdline()->append(" " + cmdline);
  }
  merged_cfg.clear_cmdline_add();
  if (!merged_cfg.has_cmdline()) {
    merged_cfg.set_cmdline("");
  }

  // If there are any initial vsock listeners, they must be bound to unique host ports.
  if (merged_cfg.has_vsock_listeners() && merged_cfg.vsock_listeners().size() > 1) {
    std::unordered_set<uint32_t> ports;
    for (auto& listener : merged_cfg.vsock_listeners()) {
      if (!ports.insert(listener.port).second) {
        callback(fpromise::error(GuestManagerError::BAD_CONFIG));
        return;
      }
    }
  }

  instance.start_time = zx::clock::get_monotonic();
  instance.state = GuestStatus::STARTING;
  instance.last_error = std::nullopt;
  SnapshotConfig(merged_cfg, &instance.guest_descriptor);
  bool balloon_enabled = merged_cfg.has_virtio_balloon();
  instance.lifecycle->Create(
      std::move(merged_cfg),
      [this, vmid, callback = std::move(callback), balloon_enabled](
          GuestLifecycle_Create_Result result) mutable {
        this->HandleCreateResult(vmid, std::move(result), balloon_enabled, std::move(callback));
      });
  instance.lifecycle->Bind(std::move(controller));
}

void GuestManager::HandleCreateResult(uint32_t vmid, GuestLifecycle_Create_Result result,
                                      bool balloon_enabled, LaunchCallback callback) {
  GuestInstance* instance = FindInstance(vmid);
  if (instance == nullptr) {
    FX_LOGS(ERROR) << "Guest Create failed for vmid=" << vmid << ": instance disappeared";
    callback(fpromise::error(GuestManagerError::START_FAILURE));
    return;
  }
  if (result.is_err()) {
    FX_LOGS(ERROR) << "Guest Create failed for vmid=" << vmid
                   << " GuestError=" << static_cast<uint32_t>(result.err());
    HandleGuestStopped(vmid, fit::error(result.err()));
    callback(fpromise::error(GuestManagerError::START_FAILURE));
  } else {
    instance->state = GuestStatus::RUNNING;
    instance->lifecycle->Run([this, vmid](GuestLifecycle_Run_Result result) {
      this->HandleRunResult(vmid, std::move(result));
    });
    if (balloon_enabled) {
      instance->memory_pressure_handler = std::make_unique<MemoryPressureHandler>(dispatcher_);
      zx_status_t status = instance->memory_pressure_handler->Start(context_);
      if (status != ZX_OK) {
        FX_PLOGS(ERROR, status) << "Failed to start memory pressure handler";
      }
    }
    OnGuestLaunched();
    callback(fpromise::ok());
  }
}

void GuestManager::HandleRunResult(uint32_t vmid, GuestLifecycle_Run_Result result) {
  if (result.is_response()) {
    HandleGuestStopped(vmid, fit::ok());
  } else {
    HandleGuestStopped(vmid, fit::error(result.err()));
  }
}

void GuestManager::HandleGuestStopped(uint32_t vmid, fit::result<GuestError> err) {
  GuestInstance* instance = FindInstance(vmid);
  if (instance == nullptr) {
    return;
  }
  if (err.is_ok()) {
    instance->last_error = std::nullopt;
  } else {
    instance->last_error = err.error_value();
  }

  instance->stop_time = zx::clock::get_monotonic();
  instance->state = GuestStatus::STOPPED;

  for (auto& pending_force_shutdown : instance->pending_force_shutdowns) {
    auto callback = std::move(pending_force_shutdown);
    callback();
  }
  instance->pending_force_shutdowns.clear();

  OnGuestStopped();
}

void GuestManager::ForceShutdown(uint32_t vmid, ForceShutdownCallback callback) {
  GuestInstance* instance = FindInstance(vmid);
  if (instance == nullptr || !instance->lifecycle.is_bound() || !instance->is_started()) {
    // VMM component isn't running.
    callback();
    return;
  }

  instance->state = GuestStatus::STOPPING;
  instance->pending_force_shutdowns.push_back(std::move(callback));
  instance->lifecycle->Stop([]() {});
}

GuestNetworkState GuestManager::QueryGuestNetworkState(uint32_t vmid) {
  GuestInstance* instance = FindInstance(vmid);
  if (instance == nullptr) {
    return GuestNetworkState::NO_NETWORK_DEVICE;
  }
  return QueryGuestNetworkState(instance->guest_descriptor);
}

GuestNetworkState GuestManager::QueryGuestNetworkState(
    const fuchsia::virtualization::GuestDescriptor& guest_descriptor) {
  if (!guest_descriptor.has_networks() || guest_descriptor.networks().empty()) {
    return GuestNetworkState::NO_NETWORK_DEVICE;
  }

  ::fuchsia::net::interfaces::StateSyncPtr state;
  zx_status_t status = context_->svc()->Connect(state.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to connect to network interface service";
    return GuestNetworkState::FAILED_TO_QUERY;
  }

  ::fuchsia::net::interfaces::WatcherSyncPtr watcher;
  status = state->GetWatcher(::fuchsia::net::interfaces::WatcherOptions(), watcher.NewRequest());
  if (status != ZX_OK || !watcher.is_bound()) {
    FX_PLOGS(ERROR, status) << "Failed to bind to network watcher service";
    return GuestNetworkState::FAILED_TO_QUERY;
  }

  bool has_bridge = false, has_ethernet = false, has_wlan = false;
  uint32_t num_virtual = 0;
  ::fuchsia::net::interfaces::Event event;
  do {
    status = watcher->Watch(&event);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to watch for interface event";
      return GuestNetworkState::FAILED_TO_QUERY;
    }

    if (!event.is_existing()) {
      // Only care about existing interfaces at the moment of this query.
      continue;
    }

    if (!event.existing().has_port_class() || !event.existing().port_class().is_device()) {
      // Ignore loopback interfaces.
      continue;
    }

    if (!event.existing().has_online() || !event.existing().online()) {
      // Only consider enabled interfaces.
      continue;
    }

    switch (event.existing().port_class().device()) {
      case ::fuchsia::hardware::network::PortClass::VIRTUAL:
        num_virtual++;
        break;
      case ::fuchsia::hardware::network::PortClass::ETHERNET:
        has_ethernet = true;
        break;
      case ::fuchsia::hardware::network::PortClass::WLAN_CLIENT:
        has_wlan = true;
        break;
      case ::fuchsia::hardware::network::PortClass::BRIDGE:
        has_bridge = true;
        break;
      case ::fuchsia::hardware::network::PortClass::PPP:
      case ::fuchsia::hardware::network::PortClass::WLAN_AP:
      default:
        // Ignore.
        break;
    }
  } while (!event.is_idle());

  if (!has_ethernet && !has_wlan) {
    // No usable host networking, so there won't be any functional guest networking.
    return GuestNetworkState::NO_HOST_NETWORKING;
  }

  if (num_virtual < guest_descriptor.networks().size()) {
    // Something went wrong during virtio-net device initialization, and there are fewer virtual
    // interfaces than there should be. This is an unlikely state as virtual interfaces may be
    // non-functional, but they should at least be present.
    return GuestNetworkState::MISSING_VIRTUAL_INTERFACES;
  }

  // See if a bridge is expected from the guest network configurations.
  bool expected_bridge =
      std::any_of(guest_descriptor.networks().begin(), guest_descriptor.networks().end(),
                  [](auto& spec) { return spec.enable_bridge; });

  if (expected_bridge && !has_bridge) {
    // A bridge was expected from the guest network configurations, but none are present.
    if (has_wlan && !has_ethernet) {
      // There's no ethernet interface to bridge against, but there is a WLAN interface. Bridging
      // against WLAN isn't supported, so the user needs to disconnect from WiFi and connect
      // ethernet.
      return GuestNetworkState::ATTEMPTED_TO_BRIDGE_WITH_WLAN;
    }

    // Possibly a transient state where a bridge is still being created.
    return GuestNetworkState::NO_BRIDGE_CREATED;
  }

  // The host and guest are likely correctly configured for guest networking.
  return GuestNetworkState::OK;
}

// Static.
std::string GuestManager::GuestNetworkStateToStringExplanation(GuestNetworkState state) {
  switch (state) {
    case GuestNetworkState::OK:
      return "Guest network likely configured correctly; "
             "check host connectivity if suspected network failure";
    case GuestNetworkState::NO_NETWORK_DEVICE:
      return "Guest not configured with a network device; "
             "check guest configuration if networking is required";
    case GuestNetworkState::FAILED_TO_QUERY:
      return "Failed to query guest network status";
    case GuestNetworkState::NO_HOST_NETWORKING:
      return "Host has no network interfaces; guest networking will not work";
    case GuestNetworkState::MISSING_VIRTUAL_INTERFACES:
      return "Fewer than expected virtual interfaces; guest failed network device startup";
    case GuestNetworkState::NO_BRIDGE_CREATED:
      return "No bridge between guest and host network interaces; "
             "this may be transient so retrying is recommended";
    case GuestNetworkState::ATTEMPTED_TO_BRIDGE_WITH_WLAN:
      return "Cannot create bridged guest network when host is using WiFi; "
             "disconnect WiFi and connect via ethernet";
  }
}

void GuestManager::Connect(uint32_t vmid,
                           fidl::InterfaceRequest<fuchsia::virtualization::Guest> controller,
                           fuchsia::virtualization::GuestManager::ConnectCallback callback) {
  GuestInstance* instance = FindInstance(vmid);
  if (instance == nullptr) {
    FX_LOGS(ERROR) << "Failed to connect to guest vmid=" << vmid << ". Unknown vmid";
    callback(fpromise::error(GuestManagerError::UNKNOWN_VMID));
    return;
  }
  if (instance->is_started()) {
    instance->lifecycle->Bind(std::move(controller));
    callback(fpromise::ok());
  } else {
    FX_LOGS(ERROR) << "Failed to connect to guest vmid=" << vmid << ". Guest is not running";
    callback(fpromise::error(GuestManagerError::NOT_RUNNING));
  }
}

std::vector<std::string> GuestManager::CheckForProblems(
    const fuchsia::virtualization::GuestDescriptor& guest_descriptor,
    MemoryPressureHandler* memory_pressure_handler) {
  std::vector<std::string> problems;

  GuestNetworkState network_state = QueryGuestNetworkState(guest_descriptor);
  switch (network_state) {
    case GuestNetworkState::OK:
    case GuestNetworkState::NO_NETWORK_DEVICE:
      // Do nothing.
      break;
    case GuestNetworkState::FAILED_TO_QUERY:
    case GuestNetworkState::NO_HOST_NETWORKING:
    case GuestNetworkState::MISSING_VIRTUAL_INTERFACES:
    case GuestNetworkState::NO_BRIDGE_CREATED:
    case GuestNetworkState::ATTEMPTED_TO_BRIDGE_WITH_WLAN:
      problems.push_back(GuestNetworkStateToStringExplanation(network_state));
      break;
  }

  if (memory_pressure_handler != nullptr) {
    switch (memory_pressure_handler->get_latest_memory_pressure_event()) {
      case fuchsia_memorypressure::Level::kNormal:
        // Do nothing.
        break;
      case fuchsia_memorypressure::Level::kWarning:
        problems.push_back("Host is experiencing moderate memory pressure");
        break;
      case fuchsia_memorypressure::Level::kCritical:
        problems.push_back("Host is experiencing severe memory pressure");
        break;
    }
  }

  return problems;
}

void GuestManager::GetInfo(uint32_t vmid, GetInfoCallback callback) {
  fuchsia::virtualization::GuestInfo info;
  info.set_vmid(vmid);

  const GuestInstance* instance = FindInstance(vmid);
  if (instance == nullptr) {
    info.set_guest_status(GuestStatus::NOT_STARTED);
    callback(std::move(info));
    return;
  }

  info.set_guest_status(instance->state);

  switch (instance->state) {
    case GuestStatus::STARTING:
    case GuestStatus::RUNNING:
    case GuestStatus::STOPPING: {
      GuestDescriptor descriptor;
      FX_CHECK(instance->guest_descriptor.Clone(&descriptor) == ZX_OK);
      info.set_guest_descriptor(std::move(descriptor));
      info.set_uptime((zx::clock::get_monotonic() - instance->start_time).to_nsecs());
      break;
    }
    case GuestStatus::STOPPED: {
      info.set_uptime((instance->stop_time - instance->start_time).to_nsecs());
      if (instance->last_error.has_value()) {
        info.set_stop_error(instance->last_error.value());
      }
      {
        GuestDescriptor descriptor;
        if (instance->guest_descriptor.Clone(&descriptor) == ZX_OK) {
          info.set_guest_descriptor(std::move(descriptor));
        }
      }
      break;
    }
    case GuestStatus::VMM_UNEXPECTED_TERMINATION:
    case GuestStatus::NOT_STARTED:
      // Do nothing.
      break;
  }

  *info.mutable_detected_problems() =
      CheckForProblems(instance->guest_descriptor, instance->memory_pressure_handler.get());

  callback(std::move(info));
}

void GuestManager::List(ListCallback callback) {
  std::vector<fuchsia::virtualization::GuestListEntry> entries;
  for (const auto& [vmid, instance] : guests_) {
    fuchsia::virtualization::GuestListEntry entry;
    entry.set_vmid(vmid);
    entry.set_guest_status(instance->state);
    switch (instance->state) {
      case GuestStatus::STARTING:
      case GuestStatus::RUNNING:
      case GuestStatus::STOPPING:
        entry.set_uptime((zx::clock::get_monotonic() - instance->start_time).to_nsecs());
        break;
      case GuestStatus::STOPPED:
        entry.set_uptime((instance->stop_time - instance->start_time).to_nsecs());
        break;
      case GuestStatus::VMM_UNEXPECTED_TERMINATION:
      case GuestStatus::NOT_STARTED:
        break;
    }
    entries.push_back(std::move(entry));
  }
  callback(std::move(entries));
}

void GuestManager::SnapshotConfig(const fuchsia::virtualization::GuestConfig& config,
                                  fuchsia::virtualization::GuestDescriptor* guest_descriptor) {
  guest_descriptor->set_num_cpus(config.cpus());
  guest_descriptor->set_guest_memory(config.guest_memory());

  guest_descriptor->set_balloon(config.has_virtio_balloon() && config.virtio_balloon());
  guest_descriptor->set_console(config.has_virtio_console() && config.virtio_console());
  guest_descriptor->set_gpu(config.has_virtio_gpu() && config.virtio_gpu());
  guest_descriptor->set_rng(config.has_virtio_rng() && config.virtio_rng());
  guest_descriptor->set_vsock(config.has_virtio_vsock() && config.virtio_vsock());
  guest_descriptor->set_sound(config.has_virtio_sound() && config.virtio_sound());

  if (config.has_net_devices()) {
    *guest_descriptor->mutable_networks() = config.net_devices();
  }
  // TODO(https://fxbug.dev/42051237): After updating FIDL, this should also return the full
  // MemoryDevice structure.
  guest_descriptor->set_mem(config.has_virtio_mem() && config.virtio_mem());
  if (config.has_kernel_type()) {
    guest_descriptor->set_kernel_type(config.kernel_type());
  }
  if (config.has_cmdline()) {
    guest_descriptor->set_cmdline(config.cmdline());
  }
  if (config.has_cmdline_add()) {
    *guest_descriptor->mutable_cmdline_add() = config.cmdline_add();
  }
}

bool GuestManager::is_guest_started(uint32_t vmid) const {
  const GuestInstance* instance = FindInstance(vmid);
  return instance != nullptr && instance->is_started();
}
