# Glossary

Short definitions as this handbook uses them. Numbers: QEMU vs SoC
in [`README.md`](README.md) **Two configs**.

**After this:** you can read any plane note without guessing a
word. Open it when a term is new; do not start here.

| Term | Meaning | Where |
| --- | --- | --- |
| Savior | Product name of the HEE host (Zircon type-1 hypervisor) | this page |
| Microkernel | Zircon host: objects + `zx_*`; virtio/VHM/lua in EL0 | [`microkernel.md`](microkernel.md) |
| Handle | Per-process `{Dispatcher*, rights}` integer | [`microkernel.md`](microkernel.md) [`objects.md`](objects.md) |
| FIDL | EL0 IPC: `.fidl` → Channel bytes + handles | [`fidl.md`](fidl.md) |
| GuestLifecycle | PA_USER0 FIDL: Create / Bind / Run / Stop | [`fidl.md`](fidl.md) [`vmm.md`](vmm.md) |
| ServiceName | ConnectToService key (`machina.VhmService`, …) | [`fidl.md`](fidl.md) |
| `request<T>` | FIDL: give away the server end of interface T | [`fidl.md`](fidl.md) |
| Dispatcher | Kernel object (`GuestDispatcher`, `ProcessDispatcher`, …) | [`microkernel.md`](microkernel.md) |
| vDSO | Kernel-provided `libzircon.so`; `zx_*` entry | [`microkernel.md`](microkernel.md) |
| Packet | `zx_port_packet_t` kernel → Vmm (`GUEST_MEM` / `VCPU` / `BELL`) | [`microkernel.md`](microkernel.md) [`cpu.md`](cpu.md) |
| HEE / BL33 | Hypervisor Execution Environment; Savior image | [`bootstrap.md`](bootstrap.md) |
| REE | Rich world: SOS / UOS / tbox Linux | [`README.md`](README.md) |
| TEE / BL32 | Secure EL1 payload. Not a Savior VM | [`tee.md`](tee.md) |
| AHYP | `BOOTMODE_AHYP`. Product path. QEMU Savior is this, not `QEMU_AHYP` | [`README.md`](README.md) |
| SOS | Service OS (Yocto). User vmid −1, kernel VMID 1 | [`vmm.md`](vmm.md) |
| UOS / ALPS | User OS (Android). User vmid 0, kernel VMID 2. Started by SOS VHM | [`vhm.md`](vhm.md) |
| tbox | Third guest. User vmid 1, kernel VMID 3 | [`vmm.md`](vmm.md) |
| VCPU | Pinned host `thread_t` that `eret`s into one guest CPU | [`cpu.md`](cpu.md) |
| pCPU | Physical core. Affinity of that thread | [`scheduler.md`](scheduler.md) |
| IPA / GPA | Guest physical address (stage-2 input) | [`memory.md`](memory.md) |
| HPA | Host physical address (DRAM / device) | [`memory.md`](memory.md) |
| Identity | GPA = HPA in that guest’s stage-2 | [`memory.md`](memory.md) |
| Stage-2 / `VTTBR` | IPA → PA table. Off while host runs (`VTTBR=0`) | [`cpu.md`](cpu.md) [`memory.md`](memory.md) |
| Trap | Unmapped IPA → DABT → VMM / VHM | [`devices.md`](devices.md) |
| Passthrough | Identity S2 + physical IRQ (+ SMMU). Guest drives HW | [`devices.md`](devices.md) |
| VGIC | Emulated GICD/GICR + `ICH_LR` virtual CPU interface | [`interrupt.md`](interrupt.md) |
| HW bit | `ICH_LR` HARDWARE. Guest EOI deactivates the **physical** INTID | [`interrupt.md`](interrupt.md) |
| PHYSICAL / VIRTUAL | Two inject types into the same LR list | [`interrupt.md`](interrupt.md) |
| VHM | ACRN-shaped FE/BE. SOS emulates UOS BARs | [`vhm.md`](vhm.md) |
| ioreq | Shared 4 KiB page, 16×256 slots, `processed` at 136 | [`vhm.md`](vhm.md) |
| SYNC / BELL | MEM trap blocks UOS; BELL posts the write | [`vhm.md`](vhm.md) |
| SWA | Software GPU virtualization. One Mali, exclusive GRANT | [`gpu.md`](gpu.md) |
| GRANT / STOP / LOST / RCVR | Virtual notify IRQs from the arbiter | [`gpu.md`](gpu.md) |
| Occupancy | RUN / PAUSE / SHARE / OFFLINE in shm. Guest parks the Linux CPU | [`scheduler.md`](scheduler.md) |
| Hint peek | `AddSchedHint` reads guest `preempt_count` / `policy`, raises host prio | [`scheduler.md`](scheduler.md) |
| `hyper_suspend_state` | Host gate: RUNNING or SUSPEND. Idle may ATF only when SUSPEND | [`pm.md`](pm.md) |
| User vmid / kernel VMID | FIDL −1/0/1 vs S2 VMID 1/2/3 | [`README.md`](README.md) |
| `HCR.TSC` / `TWI` / `IMO` | Trap SMC, trap WFI, take physical IRQ at EL2 | [`cpu.md`](cpu.md) |
| EL2 trampoline | `el2.S`. Host is EL1h; `HVC #4` enters guest | [`cpu.md`](cpu.md) |
| ramconsole | 64 KiB ring below bootdata. Whole-SoC kedump, not SOS VM reboot | [`logging.md`](logging.md) |
| `nebula` / `nbl` | Source identifiers. Same product as Savior | [`README.md`](README.md) |
| ipc-mbox | Guest mailbox; `HC_IPC_*` on `VHM_REQ`, lua `setIpcMbox` | [`system.md`](system.md) |
| guest-call | `HC_GUEST_*` (WDT kick, sched param, ktrace) on the same SMC | [`system.md`](system.md) [`vmm.md`](vmm.md) |
| Trusty-in-HEE | TAs inside Savior; `sm_handle_smc_call`. Not BL32 | [`system.md`](system.md) [`tee.md`](tee.md) |
| rproc | UOS host / SOS remote virtio; kick `INJECT_HW_IRQ` | [`system.md`](system.md) |
| GzFS vsock | SOS fuse ↔ SOS Vmm CID 10→2 port 5022, host `/data` | [`system.md`](system.md) |
| s2idle | Guest `PSCI CPU_SUSPEND` S2IDLE; host gate may then ATF | [`pm.md`](pm.md) |
| STR | Guest `PSCI SYSTEM_SUSPEND`; pCPU0 ATF `SYSTEM_SUSPEND` | [`pm.md`](pm.md) |
| FF-A | Optional TEE transport; Direct VM→SP under one lock | [`tee.md`](tee.md) |
| kick IRQ | Passthrough SPI that wakes SOS HSM (`setVhmIrq`) | [`vhm.md`](vhm.md) |
| Doorbell | Virtual IRQ into VCPU 0 (occupancy, lock, GRANT) | [`guest.md`](guest.md) |
| `El2State` | One page: guest/host GPRs + sysregs + `IchState` | [`objects.md`](objects.md) [`cpu.md`](cpu.md) |
| `GichState` | Software IRQ bitmaps + Wait; not the LR file | [`interrupt.md`](interrupt.md) |
| `ICH_LR` | 16 list registers; bit 61 = HARDWARE | [`interrupt.md`](interrupt.md) |
| TrapMap | Guest IPA → BELL/MEM + optional Port | [`objects.md`](objects.md) |
| `HC_ID` | `(x1>>24)&0xff`: `0x80` ACRN, `0xA0` IPC, `0xB0` guest-call | [`system.md`](system.md) |
| Catalog | `guest_manager.pb.txt` proto; UOS is not in it | [`vmm.md`](vmm.md) |
| `PA_USER0` | Startup Channel: GuestLifecycle server end | [`fidl.md`](fidl.md) [`microkernel.md`](microkernel.md) |
| Hypervisor resource | `GET_HYPERVISOR_RESOURCE`; `zx_guest_create_new` | [`microkernel.md`](microkernel.md) [`objects.md`](objects.md) |
| Job importance | 0–255; OOM kills least important job with running procs | [`microkernel.md`](microkernel.md) |
| Handle generation | arena slot reuse; stale `zx_handle_t` fails lookup | [`microkernel.md`](microkernel.md) |
| Inject (abort) | Savior writes guest ESR/FAR/ELR; next `eret` guest takes it | [`exceptions.md`](exceptions.md) |
| Exception port | Host Zircon: debugger→thread→process→job→system | [`exceptions.md`](exceptions.md) |
| lua `set*` | `guest_config.cc` into maps / IRQs / SWA | [`devices.md`](devices.md) |
| `SMC_ENTITY_NEBULA` | 52. FID `0x34000100` SC / `0xB4000100` FC (fn 256) | [`cpu.md`](cpu.md) |
| `FC_ID_*` | `SMC_FC_NBL_VHM_REQ` `x1`: IRQ / GPU / vmlog | [`cpu.md`](cpu.md) |
| `S2IDLE` | PSCI power_state `0x020180ff` | [`cpu.md`](cpu.md) [`pm.md`](pm.md) |
| Memory stomping | 踩内存: IPA overlap, cache alias, or missing barrier | [`shm.md`](shm.md) |
| Alias | Same PA, different MemAttr (WB vs Device) | [`shm.md`](shm.md) |
| ISH | Inner Shareable; Savior CACHED PTEs | [`shm.md`](shm.md) [`memory.md`](memory.md) |
| `dmb ishst` | Store-store barrier; virtio used ring | [`shm.md`](shm.md) |
| `HYPERVISER_GUEST_*` | `0xF0000000+` kernel SMCs (resume, event IRQ, ringbuf) | [`cpu.md`](cpu.md) |
| PSCI64 | ARM PSCI FIDs; RESET is canceled, RESET2 is STOP packet | [`cpu.md`](cpu.md) [`vmm.md`](vmm.md) |

---

Prev: [`logging.md`](logging.md) · Handbook: [`README.md`](README.md)

Everyday's a Hustle.
