//! Whole-machine checks: the seams between chips, driven through the console
//! the way the machine itself drives them.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::madam;
use retro3do::memory::{MADAM_BASE, SPORT_BASE, VRAM_BASE};
use retro3do::vdlp::control;
use retro3do::video::framebuffer_offset;
use retro3do::Console;

const fn rgb555(r: u16, g: u16, b: u16) -> u16 {
    (r << 10) | (g << 5) | b
}

#[test]
fn a_frame_from_the_console_is_black_before_anything_configures_video() {
    let mut console = Console::new();
    console.reset();
    console.run_frame();
    let frame = console.framebuffer();
    assert!(!frame.pixels.is_empty());
    assert_eq!(frame.pixels[0], 0xff00_0000);
}

#[test]
fn a_pattern_written_through_the_bus_comes_back_out_of_the_frame() {
    // Bus write, VRAM, display list, VDLP walk, pixel expansion: a break
    // anywhere fails here rather than as a black screen on a device.
    let mut console = Console::new();
    console.reset();
    let width = 320u32;
    let bus = console.system_mut();
    // Fill the whole field: the screen starts some way down the buffer.
    for y in 0..262u32 {
        for x in 0..width {
            let r = (x * 31 / (width - 1)) as u16;
            let g = (y * 31 / 261) as u16;
            bus.write16(
                VRAM_BASE + framebuffer_offset(x, y, width),
                rgb555(r, g, 31 - r),
            );
        }
    }
    // One entry for the whole field, vertical blank included.
    let list = 0x1000;
    bus.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE | 261);
    bus.write32(list + 4, VRAM_BASE);
    bus.write32(list + 8, VRAM_BASE);
    bus.write32(list + 12, 0);
    // Through MADAM's register, as the machine does.
    bus.write32(MADAM_BASE + madam::VDL_ADDRESS, list);

    console.run_frame();
    let frame = console.framebuffer();
    assert_eq!((frame.width, frame.height), (640, 480));
    assert_eq!(frame.pixels[0] & 0x00ff_00ff, 0x0000_00ff);
    assert_eq!(frame.pixels[frame.width - 1] & 0x00ff_00ff, 0x00ff_0000);
    let top_green = (frame.pixels[0] >> 8) & 0xff;
    let bottom_green = (frame.pixels[(frame.height - 1) * frame.width] >> 8) & 0xff;
    assert!(bottom_green > top_green);
}

#[test]
fn madam_and_the_display_agree_about_where_a_pixel_goes() {
    let mut console = Console::new();
    console.reset();
    let top = console.vdlp().buffer_start_line();
    let bus = console.system_mut();
    for y in 0..4 {
        for x in 0..4 {
            bus.write16(
                VRAM_BASE + framebuffer_offset(x, top + y, 320),
                rgb555(31, 31, 31),
            );
        }
    }
    let list = 0x2000;
    bus.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE | 261);
    bus.write32(list + 4, VRAM_BASE);
    bus.write32(list + 12, 0);
    bus.write32(MADAM_BASE + madam::VDL_ADDRESS, list);

    console.run_frame();
    let frame = console.framebuffer();
    for y in 0..4 {
        for x in 0..4 {
            assert_eq!(frame.pixels[y * frame.width + x], 0xffff_ffff, "({x},{y})");
        }
    }
}

#[test]
fn a_sport_copy_goes_through_the_machine_bus() {
    let mut console = Console::new();
    console.reset();
    let bus = console.system_mut();
    for offset in (0..2048).step_by(4) {
        bus.write32(VRAM_BASE + offset, 0xE1A0_0000);
    }
    bus.mem.write_watch_mut().clear();
    let before = bus.sport_accesses();
    bus.read32(SPORT_BASE); // latch page 0
    bus.write32(SPORT_BASE + (0x0800 >> 9), 0xffff_ffff);
    assert_eq!(bus.read32(VRAM_BASE + 0x800), 0xE1A0_0000);
    assert_eq!(bus.sport_accesses(), before + 2);
}

#[test]
fn the_field_flag_flips_on_a_console_clio() {
    use retro3do::clio::reg;
    let mut console = Console::new();
    console.reset();
    let clio = console.clio_mut();
    clio.set_scanlines_per_field(263);
    assert_eq!(clio.read(reg::VCOUNT) & 0x800, 0);
    for _ in 0..300 {
        clio.tick(800);
    }
    assert_eq!(clio.read(reg::VCOUNT) & 0x800, 0x800);
    assert!(clio.read(reg::VCOUNT) & 0x7ff < 263);
}

#[test]
fn a_controller_scan_writes_the_pad_report_and_interrupts() {
    use retro3do::clio::{irq, reg};
    use retro3do::pad::PadButton;
    let mut console = Console::new();
    console.reset();
    console.pads().press(0, PadButton::A, true);
    let buffer = 0x4000;
    {
        let bus = console.system_mut();
        bus.madam.set_pbus_address(buffer);
        bus.madam.set_pbus_length(12); // bytes minus four: four words
        bus.service_pbus_dma();
    }
    let bus = console.system_mut();
    // The first word is stepped over; the report starts at the second.
    assert_eq!(bus.read32(buffer), 0);
    assert_eq!(bus.read32(buffer + 4) >> 24, 0x81); // pad id with A held
    assert_eq!(bus.read32(buffer + 8), 0xffff_ffff); // padding
    assert_eq!(
        bus.clio.read(reg::IRQ1_PENDING) & irq::BANK1_PBUS_COMPLETE,
        irq::BANK1_PBUS_COMPLETE
    );
    assert_eq!(
        bus.clio.read(reg::IRQ0_PENDING) & irq::SECONDARY_BANK,
        irq::SECONDARY_BANK
    );
}

#[test]
fn a_cel_drawn_into_vram_appears_in_the_frame() {
    // MADAM writes VRAM, the VDLP reads it: an off-by-a-row bug hides here.
    use retro3do::madam::*;
    const CCB_AT: u32 = 0x1000;
    const SOURCE_AT: u32 = 0x2000;
    let fixed = |whole: i32| (whole << 16) as u32;
    let mut console = Console::new();
    console.reset();
    let bus = console.system_mut();
    bus.write16(SOURCE_AT, rgb555(31, 0, 0));
    // Far enough down to land on the visible part of the screen.
    let cel_row = 7 + 16;
    let flags = CCB_LAST
        | CCB_NP_ABS
        | CCB_SP_ABS
        | CCB_PP_ABS
        | CCB_LD_SIZE
        | CCB_LD_PRS
        | CCB_LD_PPMP
        | CCB_LD_PLUT
        | CCB_CCB_PRE
        | CCB_YOXY
        | CCB_ACW
        | CCB_ACCW;
    // Direct 16-bit, linear, one row; one pixel wide, minimum two-word stride,
    // TLLSB pass-through. Horizontal steps are stored as 12.20.
    let words = [
        flags,
        0,
        SOURCE_AT,
        0,
        fixed(3),
        fixed(cel_row),
        fixed(1) << 4,
        0,
        0,
        fixed(1),
        0,
        0,
        0,
        6 | 0x10,
        0x0000_1000,
    ];
    for (i, word) in (0u32..).zip(words) {
        bus.write32(CCB_AT + i * 4, word);
    }
    // The list head goes in NEXTCCB; the start port only says "go".
    bus.write32(MADAM_BASE + MADAM_NEXT_CCB, CCB_AT);
    bus.write32(MADAM_BASE + MADAM_CEL_START, 0);
    // The engine starts at the next instruction boundary.
    retro3do::bus::CpuBus::instruction_boundary(bus);

    let list = 0x8000;
    bus.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE | 261);
    bus.write32(list + 4, VRAM_BASE);
    bus.write32(list + 8, VRAM_BASE);
    bus.write32(list + 12, 0);
    bus.write32(MADAM_BASE + madam::VDL_ADDRESS, list);

    console.run_frame();
    let frame = console.framebuffer();
    let row = cel_row - console.vdlp().buffer_start_line() as i32;
    assert!(row >= 0);
    assert_eq!(
        frame.pixels[(row as usize * 2) * frame.width + 3 * 2],
        0xffff_0000
    );
}

// --- streaming expansion-bus DMA: drive -> CLIO -> MADAM -> RAM ------------
//
// An empty drive FIFO must stall the transfer, not turn the rest of a
// multi-sector read into zeroes.

mod streaming {
    use super::*;
    use retro3do::clio::{irq, reg, DMA_XBUS_BIT};
    use retro3do::memory::CLIO_BASE;
    use retro3do::xbus::CMD_READ;

    pub const DESTINATION: u32 = 0x10000;
    pub const SECTOR_CYCLES: u32 = 12_500_000 / 150;

    /// A cooked image whose sector n is filled with the byte n + 1.
    pub fn image(name: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("retro3do-machine-{}-{name}", std::process::id()));
        let bytes: Vec<u8> = (0..3u8)
            .flat_map(|sector| std::iter::repeat_n(sector + 1, 2048))
            .collect();
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    pub fn start(console: &mut Console, bytes: u32) {
        let bus = console.system_mut();
        for byte in [CMD_READ, 0, 2, 0, 0, 0, 3] {
            bus.write32(CLIO_BASE + reg::XBUS_COMMAND, u32::from(byte));
        }
        bus.write32(MADAM_BASE + madam::XBUS_DMA_ADDRESS, DESTINATION);
        bus.write32(MADAM_BASE + madam::XBUS_DMA_LENGTH, bytes - 4);
        bus.write32(CLIO_BASE + reg::DMA_REQUEST_SET, DMA_XBUS_BIT);
    }

    pub fn dma_irq(console: &mut Console) -> u32 {
        console.clio_mut().read(reg::IRQ0_PENDING) & irq::XBUS_DMA_COMPLETE
    }

    pub fn length(console: &Console) -> u32 {
        console.madam().read(madam::XBUS_DMA_LENGTH)
    }

    pub fn byte(console: &mut Console, offset: u32) -> u8 {
        console.system_mut().read8(DESTINATION + offset)
    }
}

#[test]
fn streaming_dma_waits_for_each_sector_and_interrupts_only_on_completion() {
    use retro3do::clio::{irq, reg};
    use streaming::*;
    let path = image("wait.iso");
    let mut console = Console::new();
    assert!(console.load_disc(&path));
    for i in 0..6144 {
        console.system_mut().write8(DESTINATION + i, 0xa5);
    }
    start(&mut console, 6144);

    assert!(console.clio().xbus_dma_requested());
    assert!(!console.clio().cdrom().has_chunk());
    assert_eq!(length(&console), 4092);
    assert_eq!(console.expansion_dma_count(), 0);
    assert_eq!(dma_irq(&mut console), 0);
    for i in 0..6144 {
        assert_eq!(
            byte(&mut console, i),
            if i < 2048 { 1 } else { 0xa5 },
            "byte {i}"
        );
    }

    console.system_mut().tick(SECTOR_CYCLES - 1);
    assert_eq!(byte(&mut console, 2048), 0xa5);
    assert_eq!(console.expansion_dma_count(), 0);
    console.system_mut().tick(1);
    assert_eq!(length(&console), 2044);
    assert_eq!(dma_irq(&mut console), 0);
    for i in 2048..6144 {
        assert_eq!(
            byte(&mut console, i),
            if i < 4096 { 2 } else { 0xa5 },
            "byte {i}"
        );
    }

    console.system_mut().tick(SECTOR_CYCLES);
    assert!(!console.clio().xbus_dma_requested());
    assert_eq!(length(&console), 0xffff_fffc);
    assert_eq!(console.expansion_dma_count(), 1);
    assert_eq!(dma_irq(&mut console), irq::XBUS_DMA_COMPLETE);
    for i in 0..6144 {
        assert_eq!(u32::from(byte(&mut console, i)), i / 2048 + 1, "byte {i}");
    }
    console
        .clio_mut()
        .write(reg::IRQ0_CLEAR, irq::XBUS_DMA_COMPLETE);
    console.system_mut().tick(SECTOR_CYCLES);
    assert_eq!(console.expansion_dma_count(), 1);
    assert_eq!(dma_irq(&mut console), 0);
    std::fs::remove_file(path).ok();
}

#[test]
fn disabling_or_clearing_streaming_dma_cancels_a_waiting_transfer() {
    use retro3do::clio::{reg, DMA_XBUS_BIT};
    use retro3do::memory::CLIO_BASE;
    use streaming::*;
    let path = image("cancel.iso");
    for clear in [reg::DMA_REQUEST_CLEAR, reg::FIFO_CLEAR] {
        let mut console = Console::new();
        assert!(console.load_disc(&path));
        console.system_mut().write8(DESTINATION + 2048, 0xa5);
        start(&mut console, 6144);
        assert!(console.clio().xbus_dma_requested());
        console
            .system_mut()
            .write32(CLIO_BASE + clear, DMA_XBUS_BIT);
        console.system_mut().tick(SECTOR_CYCLES);
        assert!(!console.clio().xbus_dma_requested());
        assert_eq!(byte(&mut console, 2048), 0xa5);
        assert_eq!(console.expansion_dma_count(), 0);
        assert_eq!(dma_irq(&mut console), 0);
    }
    std::fs::remove_file(path).ok();
}

#[test]
fn a_dma_request_smaller_than_the_buffered_sector_completes_immediately() {
    use streaming::*;
    let path = image("short.iso");
    let mut console = Console::new();
    assert!(console.load_disc(&path));
    console.system_mut().write8(DESTINATION + 32, 0xa5);
    start(&mut console, 32);
    assert!(!console.clio().xbus_dma_requested());
    assert!(console.clio().cdrom().has_chunk());
    assert_eq!(console.expansion_dma_count(), 1);
    for i in 0..32 {
        assert_eq!(byte(&mut console, i), 1);
    }
    assert_eq!(byte(&mut console, 32), 0xa5);
    std::fs::remove_file(path).ok();
}

#[test]
fn a_disc_swap_reports_the_change_without_resetting_the_machine() {
    use retro3do::clio::reg;
    use retro3do::memory::CLIO_BASE;
    use retro3do::xbus::CMD_EJECT;
    use streaming::*;

    let one = image("swap-one.iso");
    let two = image("swap-two.iso");
    let mut console = Console::new();
    console.reset();
    assert!(console.load_disc(&one));
    console.run_frame();
    // Whatever the machine had done by now is what a swap must preserve.
    let pc_before = console.cpu().reg(15);
    let frame_before = console.frame_count();

    // Software asks for the next disc: EJECT is the request a front end
    // watches for.
    for byte in [CMD_EJECT, 0, 0, 0, 0, 0, 0] {
        console
            .system_mut()
            .write32(CLIO_BASE + reg::XBUS_COMMAND, u32::from(byte));
    }
    assert!(console.disc_tray_open());

    // Insert the next disc in the set: the same eject and insert the tray UI
    // already does, with no reset anywhere in it.
    assert!(console.load_disc(&two));
    assert!(!console.disc_tray_open(), "the drawer shuts on the new disc");
    assert!(console.disc_loaded());
    assert!(
        console.disc().is_some_and(|d| d.path() == two),
        "the drive holds disc two"
    );
    assert_eq!(console.cpu().reg(15), pc_before, "no reset: the CPU carries on");
    assert_eq!(console.frame_count(), frame_before);

    std::fs::remove_file(one).ok();
    std::fs::remove_file(two).ok();
}
