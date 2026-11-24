# The machine's numbers

This crate is an emulation core rather than a game, so its "mechanics" are the
clocks, sizes and policies that decide how the emulated 3DO behaves.

The original "program" is the person's C++ core (the `cpp-reference/` tree,
removed from this repository on 2026-11-24), which is source only. To read it as a program, it was built here unchanged (CMake,
Release) and the two resulting executables were opened and decompiled with
Craft Anything (Ghidra and the built-in lifter):

- `gamecheck` — sha256 `47d35e54…742b`, links the whole machine.
- `retro3do_core_tests` — sha256 `1c5aa821…2716`, also links the thread,
  mailbox and C ABI.

Addresses below are in those executables. Every value was read from the
decompiled code (constants in the instructions or in its data section) and
matches the Rust implementation; nothing on this sheet is estimated.

## Clocks and the frame loop

| Value | Number | Where in the original | Rust |
|---|---|---|---|
| CPU budget per field | 208,333 cycles NTSC (`0x32dcd`), 250,000 PAL (`0x3d090`) = 12.5 MHz / 60 or 50 | `Console::run_frame` @0x14aa0 (gamecheck) | `Region::cycles_per_frame` |
| CPU slice | 396 cycles NTSC (`0x18c`), 399 PAL: half of (budget / lines) | `Console::run_frame` @0x14aa0 | `Region::cycles_per_scanline() / 2` |
| CPU boost | ARM target = hardware cycles × percent / 100, only when percent ≠ 100 (`cmp 0x64`) | `Console::run_frame` @0x14aa0 | `Console::run_frame` |
| CPU boost clamp | 100 (`0x64`) .. 150 (`0x96`) | `retro3do_set_cpu_scale_percent` @0xa8090 (core_tests) | `set_cpu_scale_percent` |
| Scanline length | 12,500,000 / (60 × lines) cycles (`0xbebc20 / (lines·0x3c)`); 60 is used in PAL too, so a PAL field wraps after ~208k cycles | `Clio::tick` @0x32450 | `clio::cycles_per_scanline` |
| Field wrap | line ≥ lines → line 0, field flag toggles, field complete | `Clio::tick` @0x32450 | `Clio::advance_clock` |
| Vertical blank | line == VINT0 raises bit 0, line == VINT1 raises bit 1 | `Clio::tick` @0x32450 | `Clio::advance_clock` |
| Order inside a tick | drive tick → resume expansion DMA if requested and a chunk is ready → media change → drive interrupt (bit 2) → timers → lines | `Clio::tick` @0x32450 | `System::tick` |
| Timer clock | one count per (12.5 MHz × 2¹⁶ × TIMERCTL / 21 MHz) fixed-point cycles (`0xbebc200000`, divide by 21,000,000 via `0x33216c94739703b3 >> 22`); minimum 1 | `Clio::tick_timers` @0x326f0 | `Timers::advance` |
| TIMERCTL at reset | 64 (`0x40`) | `Clio::reset` @0x8b400 (core_tests) | `TIMER_DELAY_RESET` |
| Timer step | carry starts set; disabled timer clears carry; cascade needs carry; decrement, wrap past zero reloads (reload bit) or stops (clears its decrement bit); interrupt bit = 3 + (15 − timer)/2 | `Clio::step_timers` @0x32340 | `Timers::step` |
| DSP pass | one pass of the DSP program every 283 cycles (`0x11a`/`0x11b` compare) → 44,100 Hz | `Console::tick_dsp` @0x144c0 | `SAMPLE_PERIOD` |
| CLIO revision | `0x02020000` | `Clio::reset` @0x8b400 | `REVISION_VALUE` |
| Drive poll control at reset | 15 (`0x0f`) | `Clio::reset` @0x8b400 | `poll::control_reset` |

## Picture

| Value | Number | Where | Rust |
|---|---|---|---|
| NTSC | 320 × 240 (`0x140`, `0xf0`), 263 lines (`0x107`), last line 262 (`0x106`) | `Console::set_region` @0x13ca0 | `Region` |
| PAL | 320 × 288 (`0x120`), 313 lines (`0x139`), last line 312 (`0x138`) | `Console::set_region` @0x13ca0 | `Region` |
| First visible field line | 21 (`0x15`) | `Console::set_region` @0x13ca0 | `FIRST_VISIBLE_LINE` |
| MADAM default target | VRAM `0x200000`, stride 2 × width | `Console::set_region` @0x13ca0 | `Console::apply_geometry` |
| Output | 2× both ways: 640 × 480 / 640 × 576, filled opaque black | `Console::set_region` @0x13ca0 | `Console::set_region` |
| Internal cel resolution (Rust-only addition) | 1 (default, native) to 4; output (640·k) × (480·k) / (640·k) × (576·k) | not in the reference: a project addition, `retro3do_set_internal_scale` | `hires::MAX_INTERNAL_SCALE`, `Console::set_internal_scale` |
| Hi-res mirrors kept | 4 surfaces, least recently used dropped first; VRAM change pages of 4096 bytes | project choice | `hires::MAX_SURFACES`, `memory::VramWatch::PAGE_BYTES` |
| Hi-res helper thread | a cel list recording 4096 hi-res jobs moves to a helper thread; jobs go over in batches of 4096, 32 batches kept for reuse; a job is 24 bytes | project choice, tuned on Need for Speed | `HIRES_WORKER_THRESHOLD`, `HIRES_BATCH`, `HIRES_POOL` in `madam` |
| GUI internal scale | settings slider, 1–4×; 2× is the default enhanced picture | the person's design | `internal_scale` in the GUI settings |

## Controllers

| Value | Rule | Where | Rust |
|---|---|---|---|
| Report capacity | 256 bytes (`0x100`); a pad is added only if two bytes fit | `Pbus::add_joypad` @0x21eb0 | `pbus::CAPACITY` |
| First pad byte | `0x80` \| A·1 \| Left·2 \| Right·4 \| Up·8 \| Down·16 | `Pbus::add_joypad` @0x21eb0 | `FIRST_BYTE` |
| Second pad byte | L·4 \| R·8 \| X·16 \| P·32 \| C·64 \| B·128 | `Pbus::add_joypad` @0x21eb0 | `SECOND_BYTE` |
| End of chain | 8 bytes of `0xFF`, while under 256 (`0xff` check) | `Pbus::pad` @0x222b0 | `Pbus::pad` |
| Transfer | skip if length < 0; first word stepped over; words big-endian; rest `0xFFFFFFFF`; length parked at `0xFFFFFFFC`; bank-1 bit 0 raised | `Console::service_pbus_dma` @0x147d0 | `System::service_pbus_dma` |
| Report ending mid-word | the original reads the whole word, including up to two bytes past the report that it never wrote (whatever the buffer held); the Rust core reads those as `0xFF` ("anything the devices did not fill reads as absent", the original's own comment). The only deliberate difference; it never changed a frame in the comparisons below. | `Console::service_pbus_dma` @0x147d0 | `System::service_pbus_dma` |
| Button press | atomic read-modify-write (`lock cmpxchg`), pads 0..7 (`≤ 7`) | `PadState::press` @0x21e20 | `PadState::press` |

## Audio

| Value | Number | Where | Rust |
|---|---|---|---|
| Ring | 32,768 pairs (buffer `0x20000` bytes, indices at `+0x20000/+0x20004`) | `AudioRing::push` @0x2edc0, `pull` @0x2ee50 | `AudioRing::CAPACITY` |
| Underrun | fill the rest with zero, count one underrun per short pull | `AudioRing::pull` @0x2ee50 | `AudioRing::take` |
| Overflow | refuse the excess and count it | `AudioRing::push` @0x2edc0 | `AudioRing::push` |

## Thread, pacing and frames

| Value | Number | Where | Rust |
|---|---|---|---|
| Field period | 16,666,667 ns (`0xfe502b`) NTSC, 20,000,000 ns PAL | `EmulatorThread::run` @0xa8fb0 (core_tests) | `field_period` |
| Clock restart | when more than 4 periods behind: 66,666,668 ns (`0x3f940ac`) NTSC, +13,333,332 (`0xcb7354`) = 80,000,000 ns PAL | `EmulatorThread::run` @0xa8fb0 | `period * 4` |
| Paused | sleep 8 ms (`0x7a1200` ns at data 0x101b50) and reset the deadline | `EmulatorThread::run` @0xa8fb0 | `Shared::run` |
| Frame time | stopwatch starts after a pending reset; ms = ns / 1,000,000 (data 0x101d40); smoothed 0.1 × this + 0.9 × previous (0x101d50, 0x101d48) | `EmulatorThread::run` @0xa8fb0 | `Shared::run` (stopwatch moved to match) |
| Emulated fps | frames / seconds over a window of at least 500 ms (`499999999 <`) | `EmulatorThread::run` @0xa8fb0 | `Shared::run` |
| Mailbox publish | swap ready ← writing \| `0x80000000`, take the old ready slot to write | `FrameMailbox::publish` @0x981f0 | `FrameMailbox::publish` |
| Mailbox acquire | only if the fresh bit is set; swap ready ← reading | `FrameMailbox::acquire` @0x98210 | `FrameMailbox::take_fresh` |

## Memory and media

| Value | Number | Where | Rust |
|---|---|---|---|
| BIOS load | refuse empty or larger than the 1 MB window; zero the window, copy, mark loaded | `Bus::load_bios` @0x88610 (core_tests) | `Memory::load_rom` |
| NVRAM | 32 KB, formatted image restored by `Bus::format_nvram` @0x88420; restore needs exact size (`Bus::restore_nvram` @0x884f0) | core_tests | `Memory::format_nvram`, `restore_nvram` |
| Memory map | 2 MB DRAM, 1 MB VRAM at `0x200000`, ROM at `0x3000000` | the retired `cpp-reference/core/bus.h` | `memory.rs` |

## Controls (pad bit order, as the C ABI numbers them)

Up 0, Down 1, Left 2, Right 3, A 4, B 5, C 6, P (Play) 7, X (Stop) 8,
Left shift 9, Right shift 10.

## How the whole sheet was checked in behaviour

Both cores were run side by side on the person's own BIOS (`panafz10.bin`) and
discs, printing a fingerprint of every field (CPU cycles, CPU position, picture
hash, audio hash, disc transfers): the demo ROM (300 fields), the BIOS alone
(1,200), and Doom, Flashback and Battle Chess (2,400 each, pad input from field
900). Every field matched.
