# FIDL

**After this:** you can say why a hypervisor call is FIDL and not
`zx_*`, draw the channel, and name every interface this product
uses. **First read:** Principle + diagrams 1–5 + GuestLifecycle.
**Next:** [`system.md`](system.md). Prev: [`objects.md`](objects.md).

FIDL (Fuchsia Interface Definition Language) is how **EL0
processes** talk. The kernel does not parse it. It only moves
bytes and handles on a Channel. Hypervisor policy (launch,
VHM, GPU handle dup, ipc-mbox) is FIDL. Guest enter/exit is
`zx_*` ([`objects.md`](objects.md)).

This tree’s FIDL is the older `interface` / numbered-method
dialect (`garnet/public/lib/fidl`), not modern `protocol` FIDL 3.
Bindings are C++: `fidl::Binding`, `InterfacePtr`,
`InterfaceRequest`.

## Principle

1. **FIDL is the microkernel API between servers.** Device
   drivers, GuestManager, Vmm, VhmService are processes. They
   cannot share kernel pointers. They share a Channel.

2. **A method is bytes + handles.** The compiler emits a
   fixed-layout struct (ordinal, args). Handles (VMO, Channel,
   Socket, GpuScheduler) travel **in-band** with the message
   (`zx_channel_write` handle array). After write, the sender
   no longer has that handle.

3. **Two ends, one Channel.** Client holds end A
   (`InterfacePtr`). Server holds end B (`Binding` /
   `InterfaceRequest`). Closing either end is
   `ZX_ERR_PEER_CLOSED` on the other.

4. **Ordinals are the ABI.** Methods are numbered (`1: Create`,
   `2: Bind`). Adding a method with a new number is evolution.
   Changing layout of an existing method is a break.

5. **FIDL never enters the guest.** Guest Linux uses SMC /
   virtio / ioreq. FIDL is host EL0 only. SOS Linux HSM is
   ioctl → SMC, not FIDL.

6. **GuestLifecycle is `PA_USER0`, not ServiceName.** The Vmm
   process does not exist until `CreateApplication`. There is
   no process to `ConnectToService` yet. The Channel is stuffed
   into processargs so the **first** thing `VmmController`
   binds is that handle. Named services (`machina.VhmService`,
   …) are for processes that already run (UOS Vmm → SOS Vmm).
   Mixing the two is “Create hangs / Bind timeout”.

## Framework

```text
 .fidl  (library virtualization / machina)
    fidlc  →  C++ headers  fuchsia/cpp/virtualization.h
                            fuchsia/cpp/machina.h
         │
         ▼
 Channel  (zx_channel_create → two handles)
    client  InterfacePtr<GuestLifecycle>     h1
    server  Binding<GuestLifecycle>          h2
         │  zx_channel_write / read
         ▼
 kernel ChannelDispatcher   copies bytes + moves handles
    does not know GuestLifecycle
```

Service discovery on this tree: `ServiceName="…"` +
`component::ServiceProviderBridge::AddService<T>`. The other
process `ConnectToService`. GuestLifecycle is **not** a named
service: it is passed as `PA_USER0` at process start.

## Diagrams

### 1. Why FIDL exists here

```text
 GuestManager  wants to  start SOS
    cannot zx_vcpu_resume   (no Vcpu handle — Vmm owns it)
    cannot call Vmm C++     (different process)
    Channel  GuestLifecycle.Create / Run
         │
         ▼
 Vmm  Create  →  zx_guest_create_new
      Run     →  zx_vcpu_resume
```

Kernel objects stay in the process that created them. FIDL
moves **handles** when sharing is required (GpuScheduler,
ioreq VMO).

### 2. Message on the wire

One `zx_channel_write(bytes, handles)`.

```text
 bytes
   header  txid | flags | ordinal | extra
   body    fixed struct (8-byte aligned)
   out-of-line  strings, vectors
 handles[]     VMO / Channel / Socket / …  moved, not copied

 ordinal  identifies the method
   GuestLifecycle.Create = 1
   GuestLifecycle.Bind   = 2
   GuestLifecycle.Run    = 3
   GuestLifecycle.Stop   = 4
```

Request/response: client allocates `txid`; server echoes it.
Fire-and-forget (`Stop() -> ()` still has an empty reply in
this binding, or a one-way depending on generator). This
tree’s generated C++ uses callbacks (`CreateCallback`).

`handle?` / `vector<T>?` is optional: presence flag, then
out-of-line or a handle slot.

### 3. Bind vs request (who is server)

```text
 zx::channel::create  →  h1, h2

 Client                         Server
 InterfacePtr.Bind(h1)          Binding(h2)  or
 InterfaceRequest(h2) passed
   to the server process

 request<GuestController>
    client creates a new pair
    sends the server end as a handle in the method
    keeps the client end as InterfacePtr<GuestController>
```

`request<T>` in FIDL **is** a Channel handle whose server
end is being given away.

### 4. Launch: processargs Channel (GuestLifecycle)

Not `ConnectToService`. The Channel is the newborn process’s
`PA_USER0`.

```text
 GuestManager.Launch(vmid)
    zx::channel::create  h1, h2
    lifecycle_.Bind(h1)                         client
    launch_info.url = "file://sos"
    launch_info.ta_session_request = h2         server end
    CreateApplication
         │
         ▼
 Vmm process start
    zx_get_startup_handle(PA_HND(PA_USER0, 0))
    VmmController  Binding<GuestLifecycle>(that handle)
         │
         ▼
 GuestManager  lifecycle_->Create(Config)
    FIDL Create  →  Vmm::Initialize
 GuestManager  lifecycle_->Run()
    FIDL Run     →  StartPrimaryVcpu
                   callback when VCPU 0 exits (REBOOT / SHUTDOWN / …)
```

UOS: SOS Vmm `CreateApplication("file://uos", argv=vmid)` —
same pattern, different URL. [`vmm.md`](vmm.md), [`vhm.md`](vhm.md) F8.

### 5. Handle passing (GpuScheduler, ioreq VMO)

```text
 SOS Vmm  zx_gpu_scheduler_create  →  handle S
    GpuScheduleService.GetSchedHandle
       channel_write  moves S to UOS
         │
         ▼
 UOS Vmm  now has the same GpuSchedDispatcher
    zx_gpu_scheduler_add_vcpu(S, …)
```

```text
 SOS Vmm  physical VMO of ioreq page
    VhmService.GetIoRequestVmo(vmid)
       moves VMO handle to UOS
 UOS  zx_vmar_map  that VMO     same 4 KiB as SOS GPA
```

After the write, SOS must not assume it still has a usable
duplicate unless it `zx_handle_duplicate`’d **before** the
call. Channel handles cannot be duplicated (no
`ZX_RIGHT_DUPLICATE`).

### 6. Named services inside one Vmm

SOS Vmm publishes into its `ServiceProviderBridge`. UOS Vmm
connects (same component namespace / environment).

```text
 SOS Vmm
    AddService<VhmService>           machina.VhmService
    AddService<GpuScheduleService>   machina.GpuScheduleService
    AddService<IpcService>           machina.IpcService
    AddService<IpcMessageService>    machina.IpcMessageService
    AddService<SharedIrqService>     machina.SharedIrqService
    AddService<AudioIrqService>      machina.AudioIrqService
    AddService<CrossVMLockSOSService> / CrossVMLockGMService
    AddService<RprocService>         fuchsia.virtualization.RprocService
    AddService<InspectService>
         │  ConnectToService
         ▼
 UOS Vmm  VhmDevice, Gpu client, IpcClient, …
```

GuestManager publishes `fuchsia.virtualization.GuestManager`
for `vmctl` / sysmgr.

## GuestManager / Lifecycle (library `virtualization`)

File: `garnet/public/lib/virtualization/fidl/guest.fidl`.

### `GuestManager`  `[ServiceName="fuchsia.virtualization.GuestManager"]`

| # | Method | Effect |
| --- | --- | --- |
| 1 | `Launch(int8 vmid)` | catalog entry → CreateApplication + Create + Run |
| 2 | `Connect(vmid, request<GuestController>)` | console / vsock while RUNNING |
| 3 | `ForceShutdown(vmid)` | no relaunch |
| 4 | `Show()` | `vector<GuestInfo>` |

`vmid` here is **user** vmid (−1 SOS, 0 UOS, 1 tbox).

### `GuestLifecycle`  (PA_USER0, not a ServiceName)

| # | Method | Vmm |
| --- | --- | --- |
| 1 | `Create(Config)` | `Vmm::Initialize` (lua, maps, traps) |
| 2 | `Bind(request<GuestController>)` | management probe every 15 s |
| 3 | `Run()` | `StartPrimaryVcpu`; **returns when VCPU 0 exits** |
| 4 | `Stop()` | teardown `FORCE_STOPPED` |

`Config`: `default_cfg` (lua path), `bootloader`, `memory` /
`memory_path`, `vmid`, `log_level`.

`Run` is long: the callback is the guest’s death.
`GuestError`: `OK`, `NOT_FOUND`, `ALREADY_RUNNING`,
`REBOOT_REQUIRED` (`ZX_ERR_CANCELED`), `INITIALIZATION_FAILED`,
`START_VCPU_FAILED`, `OUT_OF_MEMORY`, `FORCE_STOPPED`,
`SHUTDOWN` (`ZX_ERR_UNAVAILABLE`), `INTERNAL_ERROR`.

`GuestStatus` on the manager: `NOT_STARTED` … `RUNNING` …
`STOPPED` / `VMM_UNEXPECTED_TERMINATION`. [`vmm.md`](vmm.md).

### `GuestController`

| # | Method |
| --- | --- |
| 1 | `GetConsole() -> socket` |
| 2 | `GetHostVsockEndpoint(request<HostVsockEndpoint>)` |
| 3 | `SetLogLevel` |

### vsock (`vsock.fidl`)

Host CID **2**, guest CID **10**. `HostVsockEndpoint.Listen(port,
acceptor)`. GzFS: port 5022. [`system.md`](system.md) IPC.

## machina services (library `machina`)

All under `garnet/lib/machina/fidl/`. SOS Vmm is the server
unless noted.

### `VhmService`  `machina.VhmService`

UOS is the client. Ordinals skip 2 (historical).

| # | Method | Use |
| --- | --- | --- |
| 1 | `GetIoRequestVmo(vmid) -> (status, vmo)` | map 4 KiB ioreq |
| 3 | `Kick()` | unused on `_VHM_USE_CHANNEL_` (kick is SPI) |
| 4 | `WaitForIoRequestCompletion(vmid, vcpu)` | unused on channel path |
| 5 | `RegisterInterruptListener(vmid, InterruptListener)` | MSI → UOS |
| 6 | `FetchVmConfig(vmid) -> Buffer` | lua/config bytes |
| 7 | `GetMmioMappings(vmid, MappingListener)` | MEM/BELL traps |
| 8 | `RegisterMessageListener` | `HC_CALL` |
| 9 | `SetupChannel(vmid) -> (cli, srv)` | two Channels; UOS viewpoint |

Listeners (server-to-client callbacks — UOS implements):

| Interface | Method |
| --- | --- |
| `InterruptListener` | `OnInterrupt(irq)` |
| `MappingListener` | `OnNewMapping(base, size, type)` |
| `MessageListener` | `OnNewCall(Buffer) -> Buffer` |

`VhmClientStatus`: `OK`, `NOT_FOUND`, `ALREADY_BOUND`.
Detail: [`vhm.md`](vhm.md).

### `GpuScheduleService`  `machina.GpuScheduleService`

| # | Method | Use |
| --- | --- | --- |
| 1 | `GetSchedHandle() -> handle` | **moves** GpuScheduler object |
| 2 | `GetSchedIrq() -> SchedIrqResult` | GRANT/STOP/LOST/RCVR + Mali SPIs |
| 3 | `GetSchedReg() -> gpu_regions` | MMIO IPA |
| 4 | `GetSchedTime() -> timer_period` | slice |
| 5 | `GetSchedRsv() -> gpu_reserved_mem` | |
| 6 | `GetSchedAeeInfo() -> aee_ipa/size` | |
| 7 | `RaiseTimeSlicedIrq(irq)` | UOS asks SOS to raise a PHYSICAL Mali IRQ |

UOS cannot `zx_interrupt_bind` SOS’s Mali SPI; method 7 is
that proxy. [`gpu.md`](gpu.md).

### `IpcService`  `machina.IpcService`

Mailbox, not ioreq.

| # | Method |
| --- | --- |
| 1 | `RegisterIpcEnumerator` → `OnNewIpcChannel(id, tx_vmo, rx_vmo)` |
| 2–3 | `RegisterIpcEventListener` / `GetIpcEventListener` (`OnRxReady` / `OnTxDone`) |
| 4 | `EnumerateIpcChannels` |
| 5 | `GetIrqInfo -> (rx_ready, tx_done)` |
| 6–7 | `SetupFastCallChannel` / `SetupVMCallChannel` |
| 8 | `GetSchedBuf` occupancy shm paddr |

UOS has no lua `setIpcMbox`; it is an `IpcClient` of SOS
(user vmid 0 → way 0). [`system.md`](system.md) F18.

### `IpcMessageService`

Debug carve-outs: trace / cam dma / monitor-virtio / DTB
paddr+size.

### `CrossVMLockGMService` / `CrossVMLockSOSService`

`GetVmo() -> vmo`. GuestManager creates the VMO; SOS Vmm
maps it host+guests (`zx_guest_set_cvm_lock_vmo`). Two
service names, one page. [`guest.md`](guest.md).

### `RprocService`  `fuchsia.virtualization.RprocService`

UOS host / SOS remote. `GetRemoteDevices` / `GetHostDevices`
→ `RprocDeviceSvc.GetInfo` (shm VMO + IRQs) / `GetChannel`.
`VmStateListener` online/offline. Loaded from
`/system/data/rproc.pb.txt` (product indep always ships it).
IRQ/mblock numbers: [`system.md`](system.md) **rproc**.

### `SharedIrqService` / `AudioIrqService`

SoC shared / AFE IRQs: listener + pending bitmap. Not QEMU.

### `InspectService`  `machina.InspectService`

`FetchGuestMemory` (RO VMO of guest RAM), `FetchGuestSerial`
(socket). Debug.

## Who talks to whom

```text
 vmctl / sysmgr
    GuestManager          fuchsia.virtualization.GuestManager
         │ PA_USER0 Channel
         ▼
 Vmm SOS                  GuestLifecycle
    │
    ├─ VhmService         ◄── UOS Vmm
    ├─ GpuScheduleService ◄── UOS Vmm
    ├─ IpcService         ◄── UOS Vmm / tbox
    ├─ RprocService       ◄── UOS (host) / SOS (remote)
    ├─ CrossVMLock*
    ├─ InspectService
    └─ zx_*               kernel objects  (objects.md)

 Vmm UOS / tbox
    GuestLifecycle from their launcher (SOS or GuestManager)
    zx_* on **their** Guest / Vcpu
```

tbox does not Connect VhmService.

## Language (this dialect)

```text
 library machina;
 using mem;

 enum VhmClientStatus { OK = 0; … };

 struct Config { string default_cfg; int64 vmid; … };

 [ServiceName="machina.VhmService"]
 interface VhmService {
   1: GetIoRequestVmo(uint16 vmid) -> (VhmClientStatus @status, handle<vmo> vmo);
   5: RegisterInterruptListener(uint16 vmid, InterruptListener listener) -> (…);
 };
```

| Syntax | Meaning |
| --- | --- |
| `interface` | protocol (this tree; not `protocol`) |
| `N: Name(args) -> (rets)` | method ordinal N |
| `handle<vmo>` | typed handle, moved |
| `handle?` | optional handle |
| `request<T>` | server end of a new Channel for `T` |
| `T listener` | client sends server end of `T` (callback interface) |
| `vector<T>` / `string` | out-of-line |
| `T?` | optional |
| `@status` | named field (status is a keyword) |
| `[ServiceName=…]` | ConnectToService key |

Compiler: `zircon/system/host/fidl`. Output lands in
`out/…/fidling/gen/`. Do not edit generated `fuchsia/cpp/*.h`.

## C++ bindings (how Vmm uses it)

```text
 server
    fidl::BindingSet<GuestLifecycle> bindings_;
    bindings_.AddBinding(this, InterfaceRequest);
    class VmmController : public GuestLifecycle {
       void Create(Config, CreateCallback) override;
    };

 client
    GuestLifecyclePtr lifecycle_;
    lifecycle_.Bind(client_end);
    lifecycle_->Create(cfg, [](GuestError e){ … });
    lifecycle_.set_error_handler([]{ peer closed });
```

`InterfacePtr` is async on an `async_t` loop. Blocking
variants exist (`VhmServiceSyncPtr`) — UOS `VhmDevice` uses
sync for `GetIoRequestVmo` during Init.

## Pitfalls

- **FIDL hang vs Resume hang.** `lifecycle.Bind` 5 s timeout
  is Channel. `zx_vcpu_resume` stuck is an intercept.
- **Moved handle.** `GetSchedHandle` / `GetIoRequestVmo`
  transfer. Duplicate first if SOS still needs a copy.
- **Ordinal gap.** `VhmService` has no method 2. Do not
  renumber.
- **User vmid vs kernel VMID.** FIDL uses −1/0/1. Kernel
  objects use 1/2/3.
- **GuestLifecycle is not ConnectToService.** Missing
  `PA_USER0` → Vmm never binds; GuestManager `Create` hangs.
- **UOS `HC_*`.** No `VhmService` **server** in UOS; a UOS
  SMC `SMC_SC_NBL_VHM_REQ` `FXL_CHECK`s.
- **This is not FIDL 3.** `protocol`, `flexible`, `resource`
  syntax will not compile here.

## Terminology

| Term | Meaning here |
| --- | --- |
| FIDL | EL0 IPC language + bindings |
| Channel | Kernel object that carries one FIDL message |
| Ordinal | Method number in the header |
| InterfacePtr / Binding | Client / server C++ |
| `request<T>` | Giving away the server end of `T` |
| ServiceName | ConnectToService key |
| PA_USER0 | Startup handle: GuestLifecycle server end |
| Handle move | `zx_channel_write` takes the handle from the sender |

## Code

| Step | File |
| --- | --- |
| Language / wire | `zircon/docs/fidl/{index,language,wire-format/index}.md` |
| GuestManager FIDL | `garnet/public/lib/virtualization/fidl/guest.fidl` |
| vsock | `garnet/public/lib/virtualization/fidl/vsock.fidl` |
| machina FIDL | `garnet/lib/machina/fidl/*.fidl` |
| GuestManager | `garnet/bin/guest_manager/guest_manager.h` |
| VmmController | `garnet/bin/guest/vmm_controller.cc` |
| VhmService impl | `garnet/lib/machina/vhm_service_impl.cc` |
| Bindings (generated) | `out/…/fidling/gen/fuchsia/cpp/{virtualization,machina}.h` |

---

Prev: [`objects.md`](objects.md) · Next: [`system.md`](system.md) · [`README.md`](README.md)

Everyday's a Hustle.
