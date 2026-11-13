# Userspace hypervisor

Secular's hypervisor is the smallest stack that can host an ARM64 Linux or RTOS
guest over the QEMU serial console. It is **not** a full Fuchsia virtualization
product: GPU, net, sound, input, Wayland, and per-OS guest-manager splits are
omitted.

## Framework in one page

Hardware virtualization lives in the **Zircon kernel** (ARM EL2). Device
emulation and kernel loading live in a **userspace VMM**. The serial shell
talks to a **guest_manager** that names guests by **vmid**.

```text
EL2  Zircon kernel hypervisor
     zx_guest_create / zx_vcpu_enter
     VMID, GPA, traps, vmexit
       │
EL0  host userspace (not a guest)
     component_manager, console, zxsh
     vmm CLI → GuestManager → VMM
       │  syscall
       ▼
EL1  Linux / RTOS guest
```

Launch on the console image:

```text
QEMU aarch64
  → Zircon
    → userboot → component_manager → bootstrap
         ├─ guest-manager (eager)
         │    └─ guest_pkg            /pkg: rtos.cfg, linux.cfg, kernels
         ├─ vmm_launcher              lazy; collection virtual_machine_managers
         │    ├─ vmm-1                one VMM per GuestLifecycle connect
         │    └─ vmm-N
         └─ console-launcher → zxsh
                └─ /boot/bin/vmm → fuchsia.virtualization.GuestManager
```

1. `vmm launch vmid=0` connects to `GuestManager`.
2. `guest_manager` reads `/guest_pkg/data/rtos.cfg` (or `linux.cfg` for vmid 1),
   merges CLI overrides, and connects a new `GuestLifecycle`.
3. `vmm_launcher` starts a `vmm-N` child for that channel. The VMM creates a
   `zx_guest`, maps GPA, installs GIC/UART/PCI/virtio, loads the Image and
   `board.dtb`, starts VCPU 0.
4. `vmm attach vmid=0` binds virtio-console and serial to the host terminal.

Guests are identified by **vmid**, not by guest type. `"linux"` / `"rtos"` in
the per-vmid config set `kernel_type`; both use the ARM64 Image+DTB protocol.
Each vmid has its own VMM, so `vmm launch 0` and `vmm launch 1` can run together.

## Type 1 vs Type 2

Secular is **not a strict Type 1 or Type 2 hypervisor**. It is the KVM split:
in-kernel hardware virtualization plus a userspace VMM.

| | Type 1 (bare metal) | Type 2 (hosted) | Secular |
| --- | --- | --- | --- |
| Lowest software | the hypervisor | a general-purpose OS | Zircon kernel **contains** the hypervisor |
| Host OS | none, or a privileged VM (Dom0) | always present; hypervisor is an app | host userspace is ordinary EL0, **not** a guest |
| Where vmexit runs | hypervisor kernel | host process / kernel module | Zircon EL2 (`el2.S`, `vmexit.cc`) |
| Device model | often in the hypervisor | in the hypervisor process | userspace `bin/vmm` |

- Not strict Type 1: a full host OS (console, drivers, `component_manager`)
  runs beside the hypervisor. The host is not a guest.
- Not strict Type 2: the hypervisor is not a process on top of the OS.
  `zx_vcpu_enter` switches into EL2; traps are handled in the kernel.

Call it **in-kernel hypervisor + userspace VMM**.

## Linux guest status

The **framework can boot ARM64 Linux and RTOS guests** from the guest package
(`data/linux.cfg` + `data/Image`, `data/rtos.cfg` + `data/zephyr.bin`).

| Layer | In the console image? | Role for Linux |
| --- | --- | --- |
| Zircon hypervisor (`zx_guest` / `zx_vcpu`, ARM VMID) | Yes | Guest CPU and GPA |
| `HypervisorResource` + `VmexResource` | Yes, routed to VMM | `zx_guest_create` / executable mappings |
| VMM: GIC, PL011 UART, PL031 RTC, PCI | Yes | ARM64 machine |
| virtio-console / block / rng / balloon / vsock / mem | Yes | Console, disk, entropy |
| ARM64 `Image` + DTB (`setup_linux`) | Yes | Linux at `0x2000000` (2 MiB aligned); RTOS at `0x2080000`; DTB `/pkg/data/board.dtb` |
| `vmm` CLI + `GuestManager` (vmid) | Yes | `vmm launch vmid=0` from zxsh |
| Guest kernel (`data/Image`) | Yes | Linux 7.3.1 ARM64 Image, `vmm launch vmid=1` |
| initrd (`data/minifs.cpio`) | Yes | Alpine busybox minifs; `rdinit=/init` |
| virtio-net / GPU / audio | No | Not needed for a serial Linux console |

`vmm launch vmid=0` starts the Zephyr RTOS guest. `vmm launch vmid=1` starts
Linux 7.3.1 with minifs (`products/prebuilt/linux/`). Config:

```json
{
    "linux": "data/Image",
    "ramdisk": "data/minifs.cpio",
    "cmdline": "earlycon=pl011,0x808300000 console=hvc0 rdinit=/init",
    "virtio-console": true
}
```

Use `console=ttyAMA0` if the guest uses PL011 instead of virtio-console. A
gzipped `vmlinuz` will not load: the VMM does not decompress the kernel.

## Code reading path

Read in this order. Paths are from the repository root. Skip `bin/vmm/arch/x64`
and `bin/vmm/device/virtio_{gpu,net,sound,input}` until you need them.

### 1. Product wiring — how it reaches the serial console

How the ZBI gets a VMM and how zxsh finds `GuestManager`.

| File | What to take away |
| --- | --- |
| `products/secular/BUILD.gn` | BOOTFS packages: `guest_manager`, `vmm_launcher`, `/boot/bin/vmm` |
| `userspace/hypervisor/meta/hypervisor.bootstrap_shard.cml` | Eager `guest-manager`, lazy `vmm_launcher`; offer `GuestManager` to `console-launcher` |
| `userspace/hypervisor/bin/guest_manager/meta/guest_manager.cml` | Static children `vmm` and `guest_pkg`; use `GuestLifecycle` from `#vmm` |
| `userspace/hypervisor/bin/vmm/meta/vmm.cml` | `GuestLifecycle` + virtio collections (console/block/rng/balloon/vsock/mem) |
| `userspace/hypervisor/bin/guest_manager/meta/guest_pkg.cml` | Serves `/pkg` (`data/rtos.cfg`, `data/linux.cfg`) |
| `userspace/secular/bin/console-launcher/meta/console-launcher.cml` | Optional `fuchsia.virtualization.GuestManager` in the shell namespace |

### 2. CLI → GuestManager

User types `vmm launch vmid=0`. Follow the FIDL, not guest_type.

| File | What to take away |
| --- | --- |
| `sdk/fidl/fuchsia.virtualization/guest_manager.fidl` | `Launch(vmid, config, controller)`, `Connect`, `GetInfo`, `List` |
| `sdk/fidl/fuchsia.virtualization/guest_config.fidl` | `kernel_type`, kernel/ramdisk files, virtio flags |
| `sdk/fidl/fuchsia.virtualization/guest.fidl` | Guest control: serial, console, balloon, vsock |
| `userspace/hypervisor/bin/guest/src/main.rs` | Subcommand dispatch |
| `userspace/hypervisor/lib/cli/args/src/lib.rs` | `Vmid` parses `vmid=0` or `0` |
| `userspace/hypervisor/lib/cli/src/launch.rs` | `manager.launch(vmid, config, …)` |
| `userspace/hypervisor/lib/cli/src/platform/fuchsia.rs` | Connect `/svc/fuchsia.virtualization.GuestManager` |
| `userspace/hypervisor/lib/cli/src/attach.rs` | Serial + virtio-console sockets |
| `userspace/hypervisor/lib/cli/src/stop.rs` | Graceful console command vs `ForceShutdown` |

### 3. guest_manager — config and lifecycle

One process, a map of vmid → instance. Still one static VMM child.

| File | What to take away |
| --- | --- |
| `userspace/hypervisor/bin/guest_manager/guest_manager_main.cc` | Serves `GuestManager` |
| `userspace/hypervisor/bin/guest_manager/guest_manager.cc` | `Launch`: merge cfg, `lifecycle.Create` + `Bind`; `Connect`/`GetInfo`/`List` by vmid |
| `userspace/hypervisor/bin/guest_manager/data/rtos.cfg` | vmid 0 RTOS: Zephyr `data/zephyr.bin` |
| `userspace/hypervisor/lib/guest_config/guest_config.cc` | JSON keys `"linux"` / `"rtos"` / `"ramdisk"` / `"cmdline"` / virtio |

`GetDefaultGuestConfig()` reads `/guest_pkg/data/rtos.cfg` (vmid 0) or
`data/linux.cfg` (vmid 1). Without a kernel file handle, `Vmm::IsValidConfig`
fails.

### 4. VMM — machine, kernel load, VCPU thread

This is the userspace half of the hypervisor.

| File | What to take away |
| --- | --- |
| `userspace/hypervisor/bin/vmm/main.cc` | Async loop + `VmmController` |
| `userspace/hypervisor/bin/vmm/vmm_controller.cc` | `GuestLifecycle`: `Create` → `Initialize`, `Run` → `StartPrimaryVcpu` |
| `userspace/hypervisor/bin/vmm/vmm.cc` | `IsValidConfig`; GIC, UART, PCI, virtio; `setup_linux` / `setup_zircon` |
| `userspace/hypervisor/bin/vmm/guest.cc` | `zx::guest::create`; GPA VMO; trap ranges vs device MMIO |
| `userspace/hypervisor/bin/vmm/sysinfo.h` | `get_hypervisor_resource()` |
| `userspace/hypervisor/bin/vmm/linux.cc` | ARM64 `Image` header `0x644d5241`; Linux `0x2000000`, RTOS `0x2080000`; DTB `/pkg/data/board.dtb` |
| `userspace/hypervisor/bin/vmm/arch/arm64/dts/board.dts` | Machina DTB: PSCI, GIC, PCI ECAM, PL011, PL031 |
| `userspace/hypervisor/bin/vmm/arch/arm64/gic_distributor.cc` | Emulated GIC |
| `userspace/hypervisor/bin/vmm/arch/arm64/pl011.cc` | Guest UART (serial) |
| `userspace/hypervisor/bin/vmm/vcpu.cc` | Thread: `zx::vcpu::create` → `vcpu.enter` loop → `HandlePacket` |
| `userspace/hypervisor/bin/vmm/io.cc` | MMIO/PIO traps dispatched to devices |

VCPU loop in `vcpu.cc`: `vcpu_.enter(&packet)` is `zx_vcpu_enter`. A packet
means a vmexit the kernel punted to userspace (MMIO, IO, bell). Handle it,
then enter again.

### 5. Virtio devices — PCI frontend, Rust backend

The VMM maps a virtio PCI device, then starts a child component that owns the
virtqueues.

| File | What to take away |
| --- | --- |
| `userspace/hypervisor/bin/vmm/pci.cc` | PCI bus + BAR traps |
| `userspace/hypervisor/bin/vmm/controller/virtio_console.cc` | Creates `#meta/virtio_console.cm` in `virtio_console_devices` |
| `userspace/hypervisor/bin/vmm/device/virtio_console/src/` | Queue walk; host socket ↔ guest console |
| `userspace/hypervisor/bin/vmm/controller/virtio_block.cc` | Same pattern for disks (needed for a rootfs) |
| `userspace/hypervisor/bin/vmm/device/BUILD.gn` | Packaged: console/block/rng/balloon/vsock/mem. Extra: gpu/net/sound/input |

### 6. Kernel hypervisor — EL2, VMID, vmexit

Userspace never implements VM-entry. After `zx_vcpu_enter`, read the kernel.

| File | What to take away |
| --- | --- |
| `zircon/vdso/guest.fidl` | `zx_guest_create`, `zx_guest_set_trap` |
| `zircon/vdso/vcpu.fidl` | `zx_vcpu_create`, `zx_vcpu_enter` |
| `zircon/kernel/object/guest.rs` | Guest kernel object |
| `zircon/kernel/object/vcpu.rs` | VCPU object; `cpp_vcpu_enter` |
| `zircon/kernel/arch/arm64/hypervisor/guest.cc` | Requires EL2 + VHE; `alloc_vmid()`; GPA aspace ASID = VMID |
| `zircon/kernel/arch/arm64/hypervisor/el2_cpu_state.cc` | Per-CPU EL2 state, VMID allocator |
| `zircon/kernel/arch/arm64/hypervisor/el2.S` | World switch into the guest |
| `zircon/kernel/arch/arm64/hypervisor/vcpu.cc` | Enter guest, GIC LR, timer |
| `zircon/kernel/arch/arm64/hypervisor/vmexit.cc` | ESR_EL2: WFI, sysregs, data abort → trap packet or resume |
| `zircon/kernel/hypervisor/` | Shared GPA aspace, trap map (Rust + FFI) |

Kernel path for `zx_vcpu_enter`: `vcpu.rs` → `arch/arm64/hypervisor/vcpu.cc`
→ `el2.S` (guest EL1) → vmexit → `vmexit.cc` → either resume or
`ZX_OK` + `zx_port_packet_t` back to `Vcpu::Loop`.

### 7. Optional later

| File | When |
| --- | --- |
| `userspace/hypervisor/bin/vmm_launcher/src/vmm_launcher.rs` | Multi-instance VMM collection (`vmm-N` children) |
| `userspace/hypervisor/packages/guest_package.gni` | How a real kernel/ramdisk would be packaged |
| `userspace/hypervisor/packages/linux_guest/BUILD.gn` | Upstream Debian recipe (not in this image) |
| `userspace/hypervisor/bin/vmm/zircon.cc` | ZBI guest loader (not used by Secular) |

Suggested first pass: **1 → 2 → 4 (`vmm.cc` + `linux.cc` + `vcpu.cc`) → 6
(`guest.cc` + `vmexit.cc`)**. That is enough to see a launch die on missing
`kernel_type`, and to see where a real `Image` would be mapped.

## Components

| Path | Role |
| --- | --- |
| `bin/guest` (`/boot/bin/vmm`) | Serial CLI (`vmm launch` / `stop` / `attach` / `list`) |
| `bin/guest_manager` | `GuestManager`; vmid table; reads rtos.cfg / linux.cfg |
| `bin/vmm` | Machine + virtio; Linux/RTOS/Zircon loaders |
| `bin/vmm_launcher` | `vmm-*` children per `GuestLifecycle` connection |
| `lib/guest_config` | JSON `guest.cfg` parser |
| `lib/cli` | Shared CLI for `vmm` and `ffx guest` |
| `packages/` | `guest_package()` template; Debian/Zircon recipes (not in the console ZBI) |
| `meta/hypervisor.bootstrap_shard.cml` | Wires guest-manager + vmm_launcher into bootstrap |

## Console commands

```text
vmm launch vmid=0
vmm launch vmid=1
vmm launch vmid=0 --memory 536870912 --cpus 2
vmm list
vmm list vmid=0
vmm attach vmid=0
vmm stop vmid=0
vmm stop vmid=0 -f
```

`vmid=0` and a bare `0` are both accepted.

## Monitor guest exit statistics

`kstats -v` prints guest exits per second and the reason. ARM64 example:

```none
cpu   vm_entry vm_exit inst_abt data_abt wfx_inst sys_inst smc_inst ints
  0      43        43        0      6      27        1        0      9
  1     226       225        0    111       3       17        0     94
```

- `inst_abt`: instruction abort exits
- `data_abt`: data abort exits
- `wfx_inst`: WFE/WFI exits
- `sys_inst`: system-register access exits
- `smc_inst`: SMC exits
- `ints`: interrupt exits

x64 example (Secular's board is qemu-arm64; this table is leftover):

```none
 cpu    vm_entry vm_exit ints ints_win ept ctrl_reg msr(rd wr) inst(io hlt cpuid ple vmcall xsetbv)
  0       40       40     10      0     0      0        10 10        5  5    0    0    0      0
```

- `ints`: interrupt exits
- `ints_win`: interrupt-window exits
- `ept`: EPT violation exits
- `ctrl_reg`: CR access exits
- `msr`: MSR read/write exits
- `inst`: selected instruction exits

[ref.virtualization_overview]:
    https://fuchsia.dev/fuchsia-src/development/virtualization/overview
[ref.virtualization_get_started]:
    https://fuchsia.dev/fuchsia-src/development/virtualization/get_started
