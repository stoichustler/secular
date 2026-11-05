// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_VIRTUALIZATION_BIN_GUEST_MANAGER_GUEST_MANAGER_H_
#define SRC_VIRTUALIZATION_BIN_GUEST_MANAGER_GUEST_MANAGER_H_

#include <fuchsia/virtualization/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <map>
#include <memory>

#include "src/virtualization/bin/guest_manager/memory_pressure_handler.h"
#include "src/virtualization/lib/guest_config/guest_config.h"

enum class GuestNetworkState {
  // There are at least enough virtual device interfaces to match the guest configuration, and
  // if there is a bridged configuration, there's at least one bridged interface. This doesn't
  // guarantee working networking, but means that the system state is likely correct.
  OK = 0,

  // This guest wasn't started with a network device, so no networking is expected.
  NO_NETWORK_DEVICE = 1,

  // Failed to query network interfaces. Note that if there's no guest network device, ability to
  // query network interfaces is irrelevant.
  FAILED_TO_QUERY = 2,

  // Host doesn't have a WLAN or ethernet interface, so there's probably no guest networking.
  NO_HOST_NETWORKING = 3,

  // There's at least one missing virtual interface that was expected to be present. Check
  // virtio-net device logs for a failure.
  MISSING_VIRTUAL_INTERFACES = 4,

  // An interface is bridged, there's an ethernet interface to bridge against, but the
  // bridge hasn't been created yet. This might be a transient issue while the bridge is created.
  NO_BRIDGE_CREATED = 5,

  // An interface is bridged, and there's no ethernet to bridge against, but the host is
  // connected to WLAN. This is a common user misconfiguration.
  ATTEMPTED_TO_BRIDGE_WITH_WLAN = 6,
};

class GuestManager : public fuchsia::virtualization::GuestManager {
 public:
  GuestManager(async_dispatcher_t* dispatcher, sys::ComponentContext* context,
               std::string config_pkg_dir_path, std::string config_path);

  GuestManager(async_dispatcher_t* dispatcher, sys::ComponentContext* context)
      : GuestManager(dispatcher, context, "/guest_pkg/", "data/rtos.cfg") {}

  // |fuchsia::virtualization::GuestManager|
  void Launch(uint32_t vmid, fuchsia::virtualization::GuestConfig user_config,
              fidl::InterfaceRequest<fuchsia::virtualization::Guest> controller,
              LaunchCallback callback) override;
  void ForceShutdown(uint32_t vmid, ForceShutdownCallback callback) override;
  void Connect(uint32_t vmid, fidl::InterfaceRequest<fuchsia::virtualization::Guest> controller,
               ConnectCallback) override;
  void GetInfo(uint32_t vmid, GetInfoCallback callback) override;
  void List(ListCallback callback) override;

  // Store a subset of the configuration. This can be queried while the guest is running using
  // the GuestManager::GetInfo FIDL message.
  void SnapshotConfig(const fuchsia::virtualization::GuestConfig& config,
                      fuchsia::virtualization::GuestDescriptor* guest_descriptor);

  // Attempt to query the guest network state by iterating over the host network interfaces.
  GuestNetworkState QueryGuestNetworkState(uint32_t vmid);
  GuestNetworkState QueryGuestNetworkState(
      const fuchsia::virtualization::GuestDescriptor& guest_descriptor);
  static std::string GuestNetworkStateToStringExplanation(GuestNetworkState state);

  // Check for suspected problems with a running guest.
  std::vector<std::string> CheckForProblems(
      const fuchsia::virtualization::GuestDescriptor& guest_descriptor,
      MemoryPressureHandler* memory_pressure_handler);

  // Returns true if any guest identified by vmid is started.
  bool is_guest_started(uint32_t vmid) const;

 protected:
  virtual fit::result<::fuchsia::virtualization::GuestManagerError,
                      ::fuchsia::virtualization::GuestConfig>
  GetDefaultGuestConfig(uint32_t vmid = 0);
  virtual void OnGuestLaunched() {}
  virtual void OnGuestStopped() {}

 private:
  struct GuestInstance {
    uint32_t vmid = 0;
    ::fuchsia::virtualization::GuestLifecyclePtr lifecycle;
    std::optional<fuchsia::virtualization::GuestError> last_error;
    zx::time start_time = zx::time::infinite_past();
    zx::time stop_time = zx::time::infinite_past();
    fuchsia::virtualization::GuestDescriptor guest_descriptor;
    fuchsia::virtualization::GuestStatus state =
        fuchsia::virtualization::GuestStatus::NOT_STARTED;
    std::unique_ptr<MemoryPressureHandler> memory_pressure_handler;
    std::vector<ForceShutdownCallback> pending_force_shutdowns;

    bool is_started() const;
  };

  GuestInstance& GetOrCreateInstance(uint32_t vmid);
  GuestInstance* FindInstance(uint32_t vmid);
  const GuestInstance* FindInstance(uint32_t vmid) const;

  void HandleCreateResult(uint32_t vmid,
                          ::fuchsia::virtualization::GuestLifecycle_Create_Result result,
                          bool balloon_enabled, LaunchCallback callback);
  void HandleRunResult(uint32_t vmid,
                       ::fuchsia::virtualization::GuestLifecycle_Run_Result result);
  void HandleGuestStopped(uint32_t vmid, fit::result<::fuchsia::virtualization::GuestError> err);

  async_dispatcher_t* dispatcher_;
  sys::ComponentContext* context_;
  fidl::BindingSet<fuchsia::virtualization::GuestManager> manager_bindings_;
  std::string config_pkg_dir_path_;
  std::string config_path_;

  std::map<uint32_t, std::unique_ptr<GuestInstance>> guests_;
};

#endif  // SRC_VIRTUALIZATION_BIN_GUEST_MANAGER_GUEST_MANAGER_H_
