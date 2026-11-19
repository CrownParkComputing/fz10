//! What the CPU sees of the machine.
//!
//! The ARM60 is generic over this trait rather than holding a reference to a
//! bus. The CPU borrows the machine for the length of one `run`, and the
//! machine answers every access, every instruction boundary and the state of
//! the interrupt inputs through it. Tests drive the CPU against plain
//! [`Memory`](crate::memory::Memory); the console drives it against
//! [`System`](crate::system::System), which routes device windows to the chips.
#![forbid(unsafe_code)]

use crate::memory::Memory;

pub trait CpuBus {
    /// Word read. The low two address bits are ignored.
    fn read32(&mut self, address: u32) -> u32;
    fn read8(&mut self, address: u32) -> u8;
    fn write32(&mut self, address: u32, value: u32);
    fn write8(&mut self, address: u32, value: u8);
    /// Instruction fetch, separate from data reads: it is the hottest path and
    /// only ever reads memory.
    fn fetch32(&mut self, address: u32) -> u32;

    /// Called once after every retired instruction, before interrupts are
    /// sampled. The cel engine starts here: writing its start port only arms
    /// it, and it begins reading CCBs once the CPU's current instruction is
    /// done (Need for Speed finishes writing a CCB in that gap).
    fn instruction_boundary(&mut self) {}

    /// The FIQ input as the machine drives it. CLIO holds this line while any
    /// enabled interrupt source is pending.
    fn fiq_line(&self) -> bool {
        false
    }

    /// A range of memory written behind the CPU's back (a DMA transfer that ran
    /// inside a store), whose decoded instructions are now stale. The CPU
    /// drains these after each instruction.
    fn take_code_invalidation(&mut self) -> Option<(u32, u32)> {
        None
    }
}

impl CpuBus for Memory {
    fn read32(&mut self, address: u32) -> u32 {
        Memory::read32(self, address)
    }
    fn read8(&mut self, address: u32) -> u8 {
        Memory::read8(self, address)
    }
    fn write32(&mut self, address: u32, value: u32) {
        Memory::write32(self, address, value);
    }
    fn write8(&mut self, address: u32, value: u8) {
        Memory::write8(self, address, value);
    }
    fn fetch32(&mut self, address: u32) -> u32 {
        Memory::fetch32(self, address)
    }
}
