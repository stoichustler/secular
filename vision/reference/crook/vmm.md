# VMM

Crook's userspace hypervisor is **guest_manager** plus one **Vmm**
process per guest. SOS, UOS (ALPS / Android), and tbox are those
guests. This note is their lifecycle: launch, run, reboot, shutdown,
STR (suspend-to-RAM / s2idle), and the two watchdog layers that
restart a silent hang.

Read [`bootstrap.md`](bootstrap.md) for first `eret` into guest EL1,
[`scheduler.md`](scheduler.md) for VCPU pin / occupancy,
[`interrupt.md`](interrupt.md) for wakeup IRQs,
[`memory.md`](memory.md) for stage-2.

## Framework

```text
 sysmgr  LaunchService GuestManager
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ GuestManagerImpl                                               │
 │  catalog  guest_manager.pb.txt                                 │
 │  Guest[vmid]  FIDL GuestLifecycle → one Vmm component          │
 │  GuestManagerReporter  →  host GRT WDT                         │
 └────────────────────────────┬───────────────────────────────────┘
                              │ Create / Run / Stop / Bind
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ VmmController  (process file://sos  or UOS package)            │
 │  Vmm  Initialize + StartPrimaryVcpu                            │
 │  machina::Guest  VCPUs, VGIC, traps, PhysMem                   │
 │  Watchdog  per-VM heartbeat                                    │
 └────────────────────────────┬───────────────────────────────────┘
                              │ zx_vcpu_resume
                              ▼
   SOS (vmid -1→1)    UOS/ALPS (0→2)    tbox (1→3)
```

User vmid in FIDL / lua vs kernel `Guest::VMID_*`:

| Guest | User vmid | Kernel VMID | Typical start |
| --- | --- | --- | --- |
| SOS | −1 | 1 | `auto_start: true` |
| UOS / ALPS | 0 | 2 | SOS VHM `create_vm` / `main_uos` |
| tbox | 1 | 3 | `Launch(1)` or delayed auto_start |

QEMU catalog (`guest_manager_qemu.pb.txt`): SOS immediately, tbox
`delayed_launch_ms: 2000` and `auto_start: false` unless a board
flips it. UOS is usually spawned from SOS, not this file.

```text
 GuestManager.Launch(vmid)
    CreateApplication("file://sos")     Vmm process
    lifecycle.Create(config)            Vmm::Initialize
    lifecycle.Run                       StartPrimaryVcpu
    guest.StartVcpu(0)                  pin, eret guest
    BootstrapGuestWatchdog
```

## GuestManager state machine

Per catalog entry (`Guest` in `guest_manager.h`):

```text
                    auto_start / Launch()
 NOT_STARTED ──────────────────────────► STARTING
                                              │
                         Create OK            │ Create fail
                              │               ▼
                              ▼          STOPPED ── relaunch? ──► STARTING
                           RUNNING
                              │
              Stop / Kill / VCPU exit / probe hang
                              │
                              ▼
                           STOPPING
                              │
              HandleGuestStopped / lifecycle closed
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
           STOPPED                 VMM_UNEXPECTED_TERMINATION
              │                               │
              └──── should_relaunch ──────────┘
                            │
                            ▼ STARTING  (1 s later)
```

`is_guest_started`: STARTING | RUNNING | STOPPING.

Relaunch unless the stop was **manual** (`ForceShutdown`) or the
VCPU exit mapped to `SHUTDOWN` / `FORCE_STOPPED` / `OK`.
`REBOOT_REQUIRED` (`ZX_ERR_CANCELED`, guest `PSCI SYSTEM_RESET`)
always relaunches. Probe hang (`Bind` timeout × 2) sets
`relaunch_after_forced_teardown_` and `Kill()`s the Vmm component.

Management probe: every 15 s `lifecycle.Bind`; 5 s timeout; 1 s
retry; two failures → rebuild.

## Vmm / VCPU lifecycle

`VmmController` FIDL:

| Call | Action |
| --- | --- |
| `Create(config)` | `Vmm::Initialize` (aspace, VGIC, devices, lua) |
| `Run` | `StartPrimaryVcpu`; callback when VCPU 0 exits |
| `Bind` | `GuestController` (console, vsock, …) |
| `Stop` | teardown `FORCE_STOPPED` |

Primary VCPU exit → `GuestError`:

| `zx_vcpu_resume` | GuestError | Meaning |
| --- | --- | --- |
| `ZX_ERR_CANCELED` | `REBOOT_REQUIRED` | `PSCI SYSTEM_RESET` |
| `ZX_ERR_UNAVAILABLE` | `SHUTDOWN` | `PSCI SYSTEM_OFF` |
| other | `INTERNAL_ERROR` | crash / stop packet |

Teardown: `NotifyClientsShutdown` → `Vmm::Shutdown` → join VCPUs,
`TeeVmDestroy`, close traps. Then the component may exit; GuestManager
relaunches if the error says so.

Kernel VCPU power states (`Vcpu::STATE_*`):

```text
 INIT → RUNNING ⇄ SUSPEND
              ⇄ STANDBY
              ⇄ PWROFF
```

Guest PM (`GUEST_PM_*`), derived from all VCPUs:

| PM | When |
| --- | --- |
| `RUNNING` | any VCPU RUNNING/INIT |
| `SUSPEND_TO_IDLE` | all VCPUs SUSPEND/PWROFF (s2idle) |
| `SUSPEND_TO_RAM` | `handle_system_suspend` (STR) |
| `POWER_OFF` | no VCPU 0 |

## Launch / reboot / shutdown

### Launch

```text
 GuestManager.Launch / ScheduleAutoLaunch
    STARTING
    CreateApplication Vmm
    lifecycle.Create
       zx_guest_create_new, VmemSpec, traps, VGIC
    lifecycle.Run
       StartVcpu(0)  pin_thread  Resume loop
    RUNNING
    probe + watchdog arm
```

Secondary VCPUs: guest `PSCI CPU_ON` →
`ZX_PKT_GUEST_VCPU_STARTUP` → `StartVcpu(id)`.

### Reboot

```text
 guest  PSCI SYSTEM_RESET
    vmexit  ZX_ERR_CANCELED
    VmmController  REBOOT_REQUIRED
    teardown Vmm
    GuestManager  should_relaunch
    1 s  Launch() again     same vmid, new Vmm process
```

`SYSTEM_RESET2` is a `GUEST_VCPU_STOP` packet (same teardown path).

### Shutdown

```text
 guest  PSCI SYSTEM_OFF
    ZX_ERR_UNAVAILABLE → GuestError::SHUTDOWN
    no relaunch

 host  GuestManager.ForceShutdown(vmid)
    STOPPING
    lifecycle.Stop → FORCE_STOPPED
    manual_stop_requested_  no relaunch
```

## STR (suspend / resume)

Two guest idle depths, plus a host-wide suspend when **every** VM is
idle.

### s2idle (`PSCI CPU_SUSPEND`, `PSCI_IDLE_STATE_S2IDLE`)

```text
 guest WFI / cpuidle  PSCI CPU_SUSPEND  entry≠0
    handle_vcpu_suspend
       no pending ICH_LR
       STATE_SUSPEND
       WaitForSuspend  (VCPU thread blocks)
    all VCPUs of this VM idle → GUEST_PM_SUSPEND_TO_IDLE
    tbox: notify_modem_suspend
    if is_all_vm_suspend():
       DeviceSuspendNotify
       LK_INIT_FLAG_DEVICE_SUSPEND
       hyper_suspend_state = SUSPEND
 wakeup IRQ / HYPERVISER_GUEST_OS_RESUME
    handle_wakeup_guest_os
       s2idle: WakeupSuspend  inject wakeup SPI, wait STATE_RUNNING
    DeviceResumeNotify + DEVICE_RESUME
    tbox: notify_modem_resume
```

`CheckSkipWakeup`: ALPS drops SPIs not on the wakeup list while
s2idle ([`interrupt.md`](interrupt.md)). SOS has its own skip rules.

### STR (`handle_system_suspend`)

```text
 guest  system suspend  (last VCPU, often pCPU 0)
    GUEST_PM_SUSPEND_TO_RAM
    vcpu_thread_active--
    if last thread on this pCPU:
       pCPU≠0  suspend_for_cpu_off; SuspendThread
       pCPU==0  PSCI64_SYSTEM_SUSPEND  (ATF)
 resume
    platform_cpu_on if the pCPU was off
    ResumeThread
    SetGuestEntry(entry)
    GUEST_PM_RUNNING
```

Watchdog `OnSuspend` / `OnResume` (guest `HC_GUEST_WDTK_SUSPEND` /
`RESUME`) pause timeout checks while `GUEST_PM_SUSPEND_TO_*`.

Host `DeviceSuspend` (guest_manager side) talks to `dmctl` for
device freeze around the same window.

## Watchdog

Two independent loops. Both must see the guest as alive.

```text
 Guest Linux  HC_GUEST_WDTK_KICK  ──►  Vmm Watchdog.Heartbeat
                                           30 s check_interval
                                           miss → dump + Stop(CANCELED)
                                           → GuestManager relaunch

 GuestManager  all auto_start guests RUNNING (or in grace)
      │
      ▼  GuestManagerReporter  ioctl GRT WDT
           10 s heartbeat, miss×3, 30 s boot grace
           host hardware WDT
```

### Per-VM `machina::Watchdog`

`BootstrapGuestWatchdog` after primary VCPU starts.
`StartMonitoring` waits for the **first** kick before arming.

| Event | Effect |
| --- | --- |
| `Heartbeat` (`HC_GUEST_WDTK_KICK`) | RUNNING, deadline += 30 s |
| `OnSuspend` | SUSPENDED, skip timeout |
| `OnResume` | RUNNING, refresh deadline |
| timeout | dump VCPU state, `ioctl set_rst_status`, `guest.Stop(CANCELED)` |

+2 s `wakeup_delay_tolerance` once per miss (host scheduling slack).
Does not extend without a new kick.

PowerState: RUNNING / SUSPENDING / SUSPENDED / RESUMING.

### GuestManager reporter

Obligation: any catalog guest with `auto_start` (or mid-relaunch)
must be `RUNNING` or inside grace (startup 60 s + delayed_launch,
recovery 90 s after stop). If any obligation is unhealthy, the
reporter stops kicking the **hardware** WDT → SoC reset.

Silent hang (probe) and VCPU watchdog both funnel to relaunch; the
reporter covers “Vmm never came back”.

## Flows

### Cold start (QEMU SOS)

```text
 sysmgr  LaunchService("fuchsia.virtualization.GuestManager")
    GuestManagerImpl::BuildFromFile(/system/data/guest_manager.pb.txt)
    LaunchGuests
       SOS  auto_start → Guest::Launch  (no delay)
       tbox auto_start false  (or delayed_launch_ms)
```

`Guest::Launch` (already-started → `ALREADY_RUNNING`):

```text
 zx::channel  h1/h2
 lifecycle_.Bind(h1)                         GuestLifecycle client
 launch_info.url = "file://sos"
 launch_info.ta_session_request = h2         PA_USER0 in the Vmm process
 CreateApplication → VmmController ctor
    zx_get_startup_handle(PA_USER0) Bind
 state_ = STARTING
 watchdog_obligation_active_ = true
 ArmWatchdogStartupGrace(0)                  + 60 s
 lifecycle_->Create(Config{vmid, lua, bootloader, memory})
```

`VmmController::Create` → `Vmm::Initialize`:

```text
 read product.lua + sos.lua / tbox.lua
 ioctl guest_memory_allocator
    memory_path  →  ioctl_get_guest_memory_with_path
    memory       →  ioctl_get_guest_memory  ("grt,tbox-reserved")
    else         →  ioctl_get_system_memory
 append VmemSpec ram  kCached identity
 setVmemAuto  reserved-memory nodes
 guest_.Init(vmem)                           zx_guest_create_new + maps
 SetCpuAffinity(bind_pcpus)
 interrupt_controller.Init  VGIC + traps
 virtio / UART / vsock / vSMMU / …
 bootloader_->Setup  →  guest_ip_  (Linux Image + 64 KiB)
 Create OK
```

`lifecycle_->Run` → `StartPrimaryVcpu`:

```text
 RegisterVcpuFactory  (Create + RegisterVcpu + SetGroup/Budget)
 guest_.StartVcpu(guest_ip_, id=0)
    thrd_create  "vcpu-0"
    ThreadEntry
       zx_vcpu_create(guest, 0, entry, &vcpu_)     pin pCPU
       WAITING_TO_START → STARTING
       WriteState  x0=DTB …
       start_callback_  GICR BindVcpu
       loop  zx_vcpu_resume                     eret guest EL1h
 BootstrapGuestWatchdog
    Init + StartMonitoring  (armed on first kick)
 HandleCreateResult OK
    state_ = RUNNING
    ScheduleManagementProbe  (15 s)
```

Secondary CPUs:

```text
 guest  PSCI CPU_ON  (mpidr, entry)
    vmexit  packet STARTUP {id, entry}
    Vcpu 0 HandleVcpu  guest_->StartVcpu(entry, id)
    same factory  (must be VCPU 0)
 already-created PWROFF: handle_vcpu_on + SaveResumeEntry
```

UOS (not in QEMU catalog): SOS `HC_CREATE_VM` → VhmServiceImpl
`create_vm` → `main_uos` process, same `Guest::Init` / `StartVcpu`
path, user vmid 0.

### Guest reboot

```text
 Linux reboot
    PSCI64_SYSTEM_RESET
    vmexit.cpp  return ZX_ERR_CANCELED     (not a packet)
    zx_vcpu_resume  fails
    Vcpu thread  stop_callback_(CANCELED)
    VmmController  REBOOT_REQUIRED
    ScheduleVmmTeardown
       NotifyClientsShutdown
       Vmm::Shutdown  (bridge, Join VCPUs, TeeVmDestroy)
       FinishVmmTeardown
          run_callback_(REBOOT_REQUIRED)
          stop_component_callback_
 Guest::HandleGuestStopped(REBOOT_REQUIRED)
    ShouldRelaunchAfterStop  true
    STOPPED
    ArmWatchdogRecoveryGrace  90 s
    ScheduleRelaunch  delay 1 s
       Launch()  new Vmm process, same vmid
```

`SYSTEM_RESET2` → `ZX_PKT_GUEST_VCPU_STOP` → same teardown.
Watchdog timeout also `guest.Stop(ZX_ERR_CANCELED)` → same relaunch.

`supervision_generation_++` on every stop; a stale delayed Launch
is dropped.

### Guest shutdown (no restart)

**Guest poweroff:**

```text
 PSCI64_SYSTEM_OFF
    return ZX_ERR_UNAVAILABLE
    stop_callback_ → GuestError::SHUTDOWN
    ShouldRelaunchAfterStop  false
    watchdog_obligation_active_ = false
    STOPPED
```

**Host ForceShutdown(vmid):**

```text
 MarkManualStopRequested
    manual_stop_requested_ = true
    relaunch_after_forced_teardown_ = false
    obligation off; ++launch_generation_; ++supervision_generation_
 if not started:  complete callbacks
 else if already STOPPING:  wait
 else
    STOPPING
    lifecycle_->Stop
       VmmController::Stop  FORCE_STOPPED teardown
    HandleGuestStopped  no relaunch
```

Lifecycle channel drop while RUNNING and not STOPPING →
`VMM_UNEXPECTED_TERMINATION`; relaunch if `auto_start` or obligation.

### s2idle then wake

Per VCPU (`PSCI CPU_SUSPEND`, `power_state = S2IDLE`, `entry ≠ 0`):

```text
 handle_vcpu_suspend
    if ICH_LR in use or bitmap pending:  return 0  (do not sleep)
    STATE_SUSPEND
    vcpu_thread_active--, vcpu_suspend_active--
    if all VCPUs of this VM idle:
       GUEST_PM_SUSPEND_TO_IDLE
       tbox: notify_modem_suspend
    if is_all_vm_suspend() and hyper still RUNNING:
       SuspendDispatcher.DeviceSuspendNotify
       lk_init_level(DEVICE_SUSPEND)
       hyper_suspend_state = SUSPEND
       wakeup_all_other_cpus
    WaitForSuspend
       if S2IDLE and VM age ≥ 20 s:  no vtimer wait
       else if vtimer in future:  oneshot then
       gich_state_.Unsignal; Wait(INFINITE)
```

Wake (`HYPERVISER_GUEST_OS_RESUME` or wakeup SPI):

```text
 handle_wakeup_guest_os(vmid, irq_index)
    s2idle:  WakeupSuspend
       power_on_cpu all
       while STATE_SUSPEND (≤ 10 × 10 ms):
          if wakeup_irq > 32:  Interrupt(spi)
    after WaitForSuspend returns:
       STATE / PM RUNNING
       tbox: notify_modem_resume
    if hyper was SUSPEND:
       DeviceResumeNotify
       lk_init_level(DEVICE_RESUME)
 guest  HC_GUEST_WDTK_RESUME  →  Watchdog.OnResume + ioctl
```

`CheckSkipWakeup`: ALPS in s2idle ignores SPI not on `wakeup_irqs`.

### STR then wake

Last VCPU (typically pCPU 0) system suspend:

```text
 handle_system_suspend
    GUEST_PM_SUSPEND_TO_RAM
    vcpu_thread_active--
    if last thread on this pCPU:
       pCPU ≠ 0:  suspend_for_cpu_off; SuspendThread  (event wait)
       pCPU == 0:  PSCI64_SYSTEM_SUSPEND  →  ATF
    HC_GUEST_WDTK_SUSPEND
 resume
    if thread_active==0 && pCPU≠0:  platform_cpu_on; wait event
    ResumeThread
    SetGuestEntry(entry)
    GUEST_PM_RUNNING
    HC_GUEST_WDTK_RESUME
```

### Probe hang rebuild

```text
 RUNNING  every 15 s  lifecycle.Bind  (5 s timeout)
    non-OK or timeout  consecutive++
    < 2:  retry in 1 s
    ≥ 2:  RequestStackRebuildForSilentHang
             relaunch_after_forced_teardown_ = true
             STOPPING
             app_controller_->Kill
          HandleLifecycleClosed
             should_relaunch  (not manual)
             STOPPED + 90 s grace
             Launch in 1 s
```

### Watchdog miss

```text
 first HC_GUEST_WDTK_KICK  arms 30 s deadline
 kick  deadline = now + 30 s
 miss
    if SUSPENDED / GUEST_PM_SUSPEND_*:  skip, refresh
    if now < heartbeat+30s+2s:  defer once to hard_deadline
    else
       dump VCPU proto  ioctl set_rst_status
       stop_callback  guest.Stop(CANCELED)
       → reboot flow (REBOOT_REQUIRED relaunch)
 GuestManagerReporter
    if any obligation unhealthy:  stop kicking GRT WDT
    10 s / miss×3 / 30 s boot grace  →  SoC reset
```

## Terminology

| Term | Meaning here |
| --- | --- |
| GuestManager | sysmgr singleton; catalog + FIDL Launch/Stop. |
| Vmm | One process per guest; `GuestLifecycle`. |
| User vmid | FIDL/lua: SOS −1, ALPS 0, tbox 1. |
| Kernel VMID | SOS 1, ALPS 2, tbox 3. |
| `GuestStatus` | NOT_STARTED … VMM_UNEXPECTED_TERMINATION. |
| STR | Suspend-to-RAM (`GUEST_PM_SUSPEND_TO_RAM`). |
| s2idle | All VCPUs `STATE_SUSPEND` (`SUSPEND_TO_IDLE`). |
| Kick | Guest `HC_GUEST_WDTK_KICK` → Vmm Watchdog heartbeat. |
| Obligation | GuestManager must keep that VM RUNNING or in grace. |

## Code

| Step | File |
| --- | --- |
| Catalog / Launch | `garnet/bin/guest_manager/guest_manager.h` |
| QEMU guests | `garnet/bin/guest_manager/data/guest_manager_qemu.pb.txt` |
| Reporter | `garnet/bin/guest_manager/guest_manager_reporter.cc` |
| Vmm FIDL | `garnet/bin/guest/vmm_controller.cc` |
| Vmm init / VCPU 0 | `garnet/bin/guest/vmm.cc` |
| UOS entry | `garnet/bin/guest/main_uos.cc` |
| Per-VM WDT | `garnet/lib/machina/watchdog.cc` |
| WDT bootstrap | `garnet/bin/guest/watchdog_bootstrap.cc` |
| Kick / suspend hypercalls | `garnet/lib/machina/guest.cc` `HC_GUEST_WDTK_*` |
| PSCI / STR | `zircon/kernel/arch/arm64/hypervisor/vmexit.cpp` |
| PM aggregate | `zircon/kernel/arch/arm64/hypervisor/guest.cpp` `PmState` |
| Host device suspend | `garnet/bin/suspend/device_suspend.cc` |

---

Everyday's a Hustle.
