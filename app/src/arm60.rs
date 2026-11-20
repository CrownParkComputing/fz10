//! The ARM60: the 3DO's main processor.
//!
//! An ARM6-family core running the ARMv3 instruction set at 12.5 MHz: 32-bit
//! data and addressing, no Thumb, no long multiply, nothing on the coprocessor
//! interface. Behaviour follows ARM's published architecture (ARM DDI 0100) and
//! ARM's ARM6 instruction timing material; see `docs/PROVENANCE.md`.
//!
//! # Shape
//!
//! Each instruction word is decoded once into an `Instr`: the instruction
//! form with its operands already pulled out of the word. Decoded instructions
//! are cached per 4 KiB page of the address space, and a page is invalidated in
//! O(1) by bumping its generation stamp, so code that is written (DMA, a memory
//! test, a loader) costs nothing until it is executed again.
//!
//! The CPU does not own the machine. [`Arm60::step`] and [`Arm60::run`] borrow
//! anything implementing [`CpuBus`] for their duration; after every retired
//! instruction the bus is told ([`CpuBus::instruction_boundary`]) and only then
//! are the interrupt inputs sampled.
//!
//! # Timing
//!
//! Cycles are counted in the ARM's own units: a sequential memory cycle (S), a
//! non-sequential one (N) and an internal cycle (I). On this machine N costs
//! four S, and that ratio is most of what decides how fast the emulated CPU
//! runs relative to the hardware.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::bus::CpuBus;
use crate::memory::ROM_BASE;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Negative.
pub const FLAG_N: u32 = 1 << 31;
/// Zero.
pub const FLAG_Z: u32 = 1 << 30;
/// Carry (set on a subtraction that does not borrow).
pub const FLAG_C: u32 = 1 << 29;
/// Signed overflow.
pub const FLAG_V: u32 = 1 << 28;
/// IRQ disable.
pub const FLAG_I: u32 = 1 << 7;
/// FIQ disable.
pub const FLAG_F: u32 = 1 << 6;
/// Thumb state. Never set by an ARM60, but the bit exists.
pub const FLAG_T: u32 = 1 << 5;
/// The mode field: the low five bits of a PSR.
pub const MODE_MASK: u32 = 0x1f;

/// Exception vectors. These are ARM's addresses, not the 3DO's.
pub const VECTOR_RESET: u32 = 0x0000_0000;
pub const VECTOR_UNDEFINED: u32 = 0x0000_0004;
pub const VECTOR_SWI: u32 = 0x0000_0008;
pub const VECTOR_PREFETCH_ABORT: u32 = 0x0000_000c;
pub const VECTOR_DATA_ABORT: u32 = 0x0000_0010;
/// ARMv3's 26-bit address exception.
pub const VECTOR_ADDRESS_EXCEPTION: u32 = 0x0000_0014;
pub const VECTOR_IRQ: u32 = 0x0000_0018;
pub const VECTOR_FIQ: u32 = 0x0000_001c;

/// Where execution starts after reset. On the 3DO this is the base of the boot
/// ROM rather than address zero: the BIOS is mapped there and runs in place.
pub const RESET_PC: u32 = ROM_BASE;

// ---------------------------------------------------------------------------
// Cycle costs
// ---------------------------------------------------------------------------

/// A sequential memory cycle.
const S: u32 = 1;
/// A non-sequential memory cycle: four sequential ones on this machine.
const N: u32 = 4;
/// An internal cycle with no bus activity.
const I: u32 = 1;
/// Refilling the pipeline after a write to PC.
const REFILL: u32 = S + N;
/// A taken branch, and equally an exception entry: the pipeline is discarded
/// and refetched from the target (2S + 1N).
const BRANCH: u32 = 2 * S + N;
/// What the reference model charges for SWP, whatever its width.
const SWAP: u32 = 4;
/// What the reference model charges for an LDM/STM with an empty register
/// list, which it treats as a no-op.
const EMPTY_BLOCK: u32 = 2;

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

/// A processor mode: the CPSR's low five bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Mode {
    User = 0x10,
    Fiq = 0x11,
    Irq = 0x12,
    Supervisor = 0x13,
    Abort = 0x17,
    Undefined = 0x1b,
    System = 0x1f,
}

impl Mode {
    /// The mode named by a PSR's mode field, if it names one.
    pub fn from_bits(bits: u32) -> Option<Mode> {
        match bits & MODE_MASK {
            0x10 => Some(Mode::User),
            0x11 => Some(Mode::Fiq),
            0x12 => Some(Mode::Irq),
            0x13 => Some(Mode::Supervisor),
            0x17 => Some(Mode::Abort),
            0x1b => Some(Mode::Undefined),
            0x1f => Some(Mode::System),
            _ => None,
        }
    }

    /// The mode field value.
    pub fn bits(self) -> u32 {
        self as u32
    }
}

/// Which set of banked R13/R14/SPSR a mode field selects. User and System
/// share the user bank; so does any mode field that names no mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bank {
    User,
    Fiq,
    Irq,
    Supervisor,
    Abort,
    Undefined,
}

impl Bank {
    fn of(mode_bits: u32) -> Bank {
        match mode_bits & MODE_MASK {
            0x11 => Bank::Fiq,
            0x12 => Bank::Irq,
            0x13 => Bank::Supervisor,
            0x17 => Bank::Abort,
            0x1b => Bank::Undefined,
            _ => Bank::User,
        }
    }
}

/// Whether a mode field has an SPSR. Only User and System lack one; a mode
/// field that names no mode is treated as privileged with the user bank's slot.
fn has_spsr(mode_bits: u32) -> bool {
    let bits = mode_bits & MODE_MASK;
    bits != Mode::User.bits() && bits != Mode::System.bits()
}

#[derive(Clone, Copy, Debug, Default)]
struct Banked {
    r13: u32,
    r14: u32,
    spsr: u32,
}

#[derive(Clone, Debug, Default)]
struct Banks {
    user: Banked,
    fiq: Banked,
    irq: Banked,
    supervisor: Banked,
    abort: Banked,
    undefined: Banked,
}

impl Banks {
    fn get(&self, bank: Bank) -> &Banked {
        match bank {
            Bank::User => &self.user,
            Bank::Fiq => &self.fiq,
            Bank::Irq => &self.irq,
            Bank::Supervisor => &self.supervisor,
            Bank::Abort => &self.abort,
            Bank::Undefined => &self.undefined,
        }
    }

    fn get_mut(&mut self, bank: Bank) -> &mut Banked {
        match bank {
            Bank::User => &mut self.user,
            Bank::Fiq => &mut self.fiq,
            Bank::Irq => &mut self.irq,
            Bank::Supervisor => &mut self.supervisor,
            Bank::Abort => &mut self.abort,
            Bank::Undefined => &mut self.undefined,
        }
    }
}

// ---------------------------------------------------------------------------
// Decoded instructions
// ---------------------------------------------------------------------------

/// A register number, always 0..=15.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reg(u8);

impl Reg {
    const PC: Reg = Reg(15);

    /// The register named by the four bits of `word` starting at `shift`.
    fn field(word: u32, shift: u32) -> Reg {
        Reg(((word >> shift) & 0xf) as u8)
    }

    fn index(self) -> usize {
        usize::from(self.0 & 0xf)
    }

    fn is_pc(self) -> bool {
        self.0 & 0xf == 15
    }
}

/// The condition field, bits 31..28.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cond {
    Eq,
    Ne,
    Cs,
    Cc,
    Mi,
    Pl,
    Vs,
    Vc,
    Hi,
    Ls,
    Ge,
    Lt,
    Gt,
    Le,
    Al,
    /// "Never" on ARMv3.
    Nv,
}

impl Cond {
    fn from_word(word: u32) -> Cond {
        match word >> 28 {
            0x0 => Cond::Eq,
            0x1 => Cond::Ne,
            0x2 => Cond::Cs,
            0x3 => Cond::Cc,
            0x4 => Cond::Mi,
            0x5 => Cond::Pl,
            0x6 => Cond::Vs,
            0x7 => Cond::Vc,
            0x8 => Cond::Hi,
            0x9 => Cond::Ls,
            0xa => Cond::Ge,
            0xb => Cond::Lt,
            0xc => Cond::Gt,
            0xd => Cond::Le,
            0xe => Cond::Al,
            _ => Cond::Nv,
        }
    }

    fn passes(self, cpsr: u32) -> bool {
        let n = cpsr & FLAG_N != 0;
        let z = cpsr & FLAG_Z != 0;
        let c = cpsr & FLAG_C != 0;
        let v = cpsr & FLAG_V != 0;
        match self {
            Cond::Eq => z,
            Cond::Ne => !z,
            Cond::Cs => c,
            Cond::Cc => !c,
            Cond::Mi => n,
            Cond::Pl => !n,
            Cond::Vs => v,
            Cond::Vc => !v,
            Cond::Hi => c && !z,
            Cond::Ls => !c || z,
            Cond::Ge => n == v,
            Cond::Lt => n != v,
            Cond::Gt => !z && n == v,
            Cond::Le => z || n != v,
            Cond::Al => true,
            Cond::Nv => false,
        }
    }
}

/// Data-processing opcodes, bits 24..21.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AluOp {
    And,
    Eor,
    Sub,
    Rsb,
    Add,
    Adc,
    Sbc,
    Rsc,
    Tst,
    Teq,
    Cmp,
    Cmn,
    Orr,
    Mov,
    Bic,
    Mvn,
}

impl AluOp {
    fn from_word(word: u32) -> AluOp {
        match (word >> 21) & 0xf {
            0x0 => AluOp::And,
            0x1 => AluOp::Eor,
            0x2 => AluOp::Sub,
            0x3 => AluOp::Rsb,
            0x4 => AluOp::Add,
            0x5 => AluOp::Adc,
            0x6 => AluOp::Sbc,
            0x7 => AluOp::Rsc,
            0x8 => AluOp::Tst,
            0x9 => AluOp::Teq,
            0xa => AluOp::Cmp,
            0xb => AluOp::Cmn,
            0xc => AluOp::Orr,
            0xd => AluOp::Mov,
            0xe => AluOp::Bic,
            _ => AluOp::Mvn,
        }
    }

    /// TST, TEQ, CMP and CMN only set flags.
    fn writes_result(self) -> bool {
        !matches!(self, AluOp::Tst | AluOp::Teq | AluOp::Cmp | AluOp::Cmn)
    }
}

/// Barrel shifter operation, bits 6..5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShiftKind {
    Lsl,
    Lsr,
    Asr,
    /// ROR, or RRX when an immediate amount of zero is encoded.
    Ror,
}

impl ShiftKind {
    fn from_word(word: u32) -> ShiftKind {
        match (word >> 5) & 3 {
            0 => ShiftKind::Lsl,
            1 => ShiftKind::Lsr,
            2 => ShiftKind::Asr,
            _ => ShiftKind::Ror,
        }
    }
}

/// The second operand of a data-processing instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operand2 {
    /// A rotated 8-bit immediate, already rotated. `carry` is the shifter carry
    /// it produces: bit 31 of the value when the rotation is non-zero, and
    /// "unchanged" otherwise.
    Immediate { value: u32, carry: Option<bool> },
    /// Rm shifted by a 5-bit immediate.
    ShiftedByImmediate {
        rm: Reg,
        kind: ShiftKind,
        amount: u8,
    },
    /// Rm shifted by the low byte of Rs. Costs an extra internal cycle, and PC
    /// reads twelve ahead as Rm or Rn.
    ShiftedByRegister { rm: Reg, kind: ShiftKind, rs: Reg },
}

/// The offset of a single data transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Offset {
    Immediate(u32),
    /// Rm shifted by a 5-bit immediate (register-specified amounts don't exist
    /// here: that encoding is the undefined-instruction hole).
    Register {
        rm: Reg,
        kind: ShiftKind,
        amount: u8,
    },
}

/// The source operand of MSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MsrSource {
    Immediate(u32),
    Register(Reg),
}

/// One instruction form with its operands extracted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Instr {
    DataProcessing {
        op: AluOp,
        set_flags: bool,
        rn: Reg,
        rd: Reg,
        operand: Operand2,
    },
    /// MUL / MLA: Rd = Rm * Rs (+ Rn).
    Multiply {
        accumulate: bool,
        set_flags: bool,
        rd: Reg,
        rn: Reg,
        rs: Reg,
        rm: Reg,
    },
    /// LDR / STR / LDRB / STRB.
    SingleTransfer {
        load: bool,
        byte: bool,
        pre_index: bool,
        add: bool,
        write_back: bool,
        rn: Reg,
        rd: Reg,
        offset: Offset,
    },
    /// LDM / STM.
    BlockTransfer {
        load: bool,
        pre_index: bool,
        add: bool,
        /// The S bit: user bank, or restore CPSR when PC is loaded.
        user_bank: bool,
        write_back: bool,
        rn: Reg,
        list: u16,
    },
    /// B / BL with the byte offset already sign-extended and scaled.
    Branch {
        link: bool,
        offset: i32,
    },
    SoftwareInterrupt,
    /// SWP / SWPB.
    Swap {
        byte: bool,
        rn: Reg,
        rd: Reg,
        rm: Reg,
    },
    /// MRS: read CPSR or SPSR.
    StatusToRegister {
        from_spsr: bool,
        rd: Reg,
    },
    /// MSR: write CPSR or SPSR, whole or flags only.
    RegisterToStatus {
        to_spsr: bool,
        flags_only: bool,
        source: MsrSource,
    },
    /// The undefined-instruction space, and every coprocessor instruction (the
    /// 3DO's ARM60 has no coprocessor attached).
    Undefined,
}

/// A cached decode: the condition and the instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Decoded {
    cond: Cond,
    instr: Instr,
}

/// The rotated 8-bit immediate of data processing and MSR.
fn rotated_immediate(word: u32) -> u32 {
    (word & 0xff).rotate_right(((word >> 8) & 0xf) * 2)
}

fn bit(word: u32, n: u32) -> bool {
    (word >> n) & 1 != 0
}

/// Decode one instruction word. The more specific encodings share bit patterns
/// with the data-processing space, so they are recognised first.
fn decode(word: u32) -> Decoded {
    let cond = Cond::from_word(word);
    let instr = if word & 0x0fc0_00f0 == 0x0000_0090 {
        Instr::Multiply {
            accumulate: bit(word, 21),
            set_flags: bit(word, 20),
            rd: Reg::field(word, 16),
            rn: Reg::field(word, 12),
            rs: Reg::field(word, 8),
            rm: Reg::field(word, 0),
        }
    } else if word & 0x0fb0_0ff0 == 0x0100_0090 {
        Instr::Swap {
            byte: bit(word, 22),
            rn: Reg::field(word, 16),
            rd: Reg::field(word, 12),
            rm: Reg::field(word, 0),
        }
    } else if word & 0x0fbf_0fff == 0x010f_0000 {
        Instr::StatusToRegister {
            from_spsr: bit(word, 22),
            rd: Reg::field(word, 12),
        }
    } else if word & 0x0db0_f000 == 0x0120_f000 {
        let source = if bit(word, 25) {
            MsrSource::Immediate(rotated_immediate(word))
        } else {
            MsrSource::Register(Reg::field(word, 0))
        };
        // Bit 16 selects the whole register; clear means the flags only.
        Instr::RegisterToStatus {
            to_spsr: bit(word, 22),
            flags_only: !bit(word, 16),
            source,
        }
    } else if word & 0x0f00_0000 == 0x0f00_0000 {
        Instr::SoftwareInterrupt
    } else if word & 0x0e00_0000 == 0x0a00_0000 {
        // 24-bit signed word offset, sign-extended and scaled to bytes.
        Instr::Branch {
            link: bit(word, 24),
            offset: ((word << 8) as i32) >> 6,
        }
    } else if word & 0x0e00_0000 == 0x0800_0000 {
        Instr::BlockTransfer {
            load: bit(word, 20),
            pre_index: bit(word, 24),
            add: bit(word, 23),
            user_bank: bit(word, 22),
            write_back: bit(word, 21),
            rn: Reg::field(word, 16),
            list: (word & 0xffff) as u16,
        }
    } else if word & 0x0c00_0000 == 0x0400_0000 {
        if word & 0x0e00_0010 == 0x0600_0010 {
            // Register offset with bit 4 set: the undefined-instruction hole.
            Instr::Undefined
        } else {
            let offset = if bit(word, 25) {
                Offset::Register {
                    rm: Reg::field(word, 0),
                    kind: ShiftKind::from_word(word),
                    amount: ((word >> 7) & 0x1f) as u8,
                }
            } else {
                Offset::Immediate(word & 0xfff)
            };
            Instr::SingleTransfer {
                load: bit(word, 20),
                byte: bit(word, 22),
                pre_index: bit(word, 24),
                add: bit(word, 23),
                write_back: bit(word, 21),
                rn: Reg::field(word, 16),
                rd: Reg::field(word, 12),
                offset,
            }
        }
    } else if word & 0x0c00_0000 == 0 {
        let operand = if bit(word, 25) {
            let value = rotated_immediate(word);
            let carry = (word & 0xf00 != 0).then_some(value & 0x8000_0000 != 0);
            Operand2::Immediate { value, carry }
        } else if bit(word, 4) {
            Operand2::ShiftedByRegister {
                rm: Reg::field(word, 0),
                kind: ShiftKind::from_word(word),
                rs: Reg::field(word, 8),
            }
        } else {
            Operand2::ShiftedByImmediate {
                rm: Reg::field(word, 0),
                kind: ShiftKind::from_word(word),
                amount: ((word >> 7) & 0x1f) as u8,
            }
        };
        Instr::DataProcessing {
            op: AluOp::from_word(word),
            set_flags: bit(word, 20),
            rn: Reg::field(word, 16),
            rd: Reg::field(word, 12),
            operand,
        }
    } else {
        // Coprocessor space: nothing attached, so it traps as undefined.
        Instr::Undefined
    };
    Decoded { cond, instr }
}

// ---------------------------------------------------------------------------
// Barrel shifter
// ---------------------------------------------------------------------------

/// Shift `value`, returning the result and the shifter's carry out.
///
/// `amount` is 0..=31 for an immediate shift and the low byte of Rs for a
/// register shift. Immediate encodings give amount zero special meanings
/// (LSL #0 is no shift, LSR #0 and ASR #0 mean 32, ROR #0 is RRX); a register
/// amount of zero leaves both value and carry alone.
fn shift(value: u32, kind: ShiftKind, amount: u32, by_register: bool, carry: bool) -> (u32, bool) {
    if by_register && amount == 0 {
        return (value, carry);
    }
    match kind {
        ShiftKind::Lsl => match amount {
            0 => (value, carry),
            1..=31 => (value << amount, (value >> (32 - amount)) & 1 != 0),
            32 => (0, value & 1 != 0),
            _ => (0, false),
        },
        ShiftKind::Lsr => match amount {
            0 | 32 => (0, value & 0x8000_0000 != 0),
            1..=31 => (value >> amount, (value >> (amount - 1)) & 1 != 0),
            _ => (0, false),
        },
        ShiftKind::Asr => {
            let signed = value as i32;
            match amount {
                1..=31 => ((signed >> amount) as u32, (signed >> (amount - 1)) & 1 != 0),
                _ => {
                    let fill = (signed >> 31) as u32;
                    (fill, fill != 0)
                }
            }
        }
        ShiftKind::Ror => {
            if amount == 0 {
                // RRX: a 33-bit rotate through carry.
                let top = if carry { 0x8000_0000 } else { 0 };
                return ((value >> 1) | top, value & 1 != 0);
            }
            match amount & 31 {
                0 => (value, value & 0x8000_0000 != 0),
                rot => (value.rotate_right(rot), (value >> (rot - 1)) & 1 != 0),
            }
        }
    }
}

/// a + b + carry_in, with the carry out and signed overflow of the 32-bit sum.
fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let wide = u64::from(a) + u64::from(b) + u64::from(carry_in);
    let sum = wide as u32;
    let carry = wide >> 32 != 0;
    let overflow = !(a ^ b) & (a ^ sum) & 0x8000_0000 != 0;
    (sum, carry, overflow)
}

/// Internal cycles of the ARM6's 2-bit Booth multiplier for multiplier `rs`.
///
/// ARM's ARM6 timing describes the array consuming two bits of Rs per internal
/// cycle and stopping when the rest is zero, up to sixteen: zero and one take
/// one cycle, and for n > 1 the values 2^(2n-3)..2^(2n-1)-1 take n. That is a
/// count of significant bits, not bytes (the byte-granular table is the ARM7's).
fn booth_cycles(rs: u32) -> u32 {
    if rs <= 1 {
        return 1;
    }
    let significant_bits = 32 - rs.leading_zeros();
    ((significant_bits + 2) / 2).min(16)
}

// ---------------------------------------------------------------------------
// Decode cache
// ---------------------------------------------------------------------------

const PAGE_SHIFT: u32 = 12;
const PAGE_BYTES: u32 = 1 << PAGE_SHIFT;
const PAGE_SLOTS: usize = (PAGE_BYTES / 4) as usize;
const FAST_SLOTS: usize = 64;

#[derive(Clone, Copy)]
struct Slot {
    /// The page generation this decode belongs to; anything else is stale.
    stamp: u32,
    decoded: Decoded,
}

const EMPTY_SLOT: Slot = Slot {
    stamp: 0,
    decoded: Decoded {
        cond: Cond::Nv,
        instr: Instr::Undefined,
    },
};

struct Page {
    generation: u32,
    slots: Vec<Slot>,
}

impl Page {
    fn new() -> Page {
        Page {
            generation: 1,
            slots: vec![EMPTY_SLOT; PAGE_SLOTS],
        }
    }

    /// Make every decode in the page stale.
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Wrapped: old stamps could look current again.
            self.slots.fill(EMPTY_SLOT);
            self.generation = 1;
        }
    }
}

/// Decoded instructions, one page per 4 KiB of address space actually
/// executed, found through a small direct-mapped table in front of a map.
struct DecodeCache {
    pages: Vec<Page>,
    index: HashMap<u32, usize>,
    /// (page key, index into `pages`); `None` is an empty slot.
    fast: [Option<(u32, usize)>; FAST_SLOTS],
}

impl DecodeCache {
    fn new() -> DecodeCache {
        DecodeCache {
            pages: Vec::new(),
            index: HashMap::new(),
            fast: [None; FAST_SLOTS],
        }
    }

    fn clear(&mut self) {
        self.pages.clear();
        self.index.clear();
        self.fast = [None; FAST_SLOTS];
    }

    fn page_index(&mut self, key: u32) -> usize {
        let fast_slot = (key as usize) % FAST_SLOTS;
        if let Some(Some((cached_key, index))) = self.fast.get(fast_slot) {
            if *cached_key == key {
                return *index;
            }
        }
        let index = match self.index.get(&key) {
            Some(&index) => index,
            None => {
                self.pages.push(Page::new());
                let index = self.pages.len() - 1;
                self.index.insert(key, index);
                index
            }
        };
        if let Some(entry) = self.fast.get_mut(fast_slot) {
            *entry = Some((key, index));
        }
        index
    }

    /// The slot for `address`, creating its page if needed, with the page's
    /// current generation.
    fn slot_mut(&mut self, address: u32) -> Option<(&mut Slot, u32)> {
        let index = self.page_index(address >> PAGE_SHIFT);
        let page = self.pages.get_mut(index)?;
        let generation = page.generation;
        let slot = page
            .slots
            .get_mut(((address & (PAGE_BYTES - 1)) >> 2) as usize)?;
        Some((slot, generation))
    }

    fn invalidate(&mut self, address: u32, length: u32) {
        let first = address >> PAGE_SHIFT;
        let last = address.wrapping_add(length).wrapping_sub(1) >> PAGE_SHIFT;
        for key in first..=last {
            if let Some(page) = self
                .index
                .get(&key)
                .and_then(|&index| self.pages.get_mut(index))
            {
                page.invalidate();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The CPU
// ---------------------------------------------------------------------------

/// The ARM60 core.
pub struct Arm60 {
    /// The live registers. R15 holds the current instruction's address plus
    /// eight while it executes, as the pipeline makes it read.
    regs: [u32; 16],
    cpsr: u32,
    /// R13, R14 and SPSR of every bank other than the live one's R13/R14.
    banks: Banks,
    /// Whichever of the user and FIQ R8..R12 sets is not live.
    shadow_r8_r12: [u32; 5],

    /// Set by anything that writes PC. Comparing PC against what was installed
    /// cannot tell a branch of offset zero from a fall-through.
    pc_written: bool,

    irq_line: bool,
    irq_latched: bool,
    fiq_line: bool,

    cache: DecodeCache,

    total_cycles: u64,
    total_instructions: u64,
    irqs_taken: u64,
    fiqs_taken: u64,
    decodes: u64,
    invalidations: u64,
}

impl Default for Arm60 {
    fn default() -> Self {
        Self::new()
    }
}

impl Arm60 {
    /// A powered-on CPU, in supervisor mode at the reset vector.
    pub fn new() -> Self {
        let mut cpu = Arm60 {
            regs: [0; 16],
            cpsr: 0,
            banks: Banks::default(),
            shadow_r8_r12: [0; 5],
            pc_written: false,
            irq_line: false,
            irq_latched: false,
            fiq_line: false,
            cache: DecodeCache::new(),
            total_cycles: 0,
            total_instructions: 0,
            irqs_taken: 0,
            fiqs_taken: 0,
            decodes: 0,
            invalidations: 0,
        };
        cpu.reset();
        cpu
    }

    /// Return to the power-on state: every register zero, supervisor mode with
    /// IRQ and FIQ masked, PC at [`RESET_PC`], interrupt inputs low, the decode
    /// cache empty and the cycle and instruction counts zero. The interrupt,
    /// decode and invalidation counters keep counting across resets.
    pub fn reset(&mut self) {
        self.regs = [0; 16];
        self.banks = Banks::default();
        self.shadow_r8_r12 = [0; 5];
        self.cpsr = Mode::Supervisor.bits() | FLAG_I | FLAG_F;
        self.set_r(Reg::PC, RESET_PC);
        self.pc_written = false;
        self.irq_line = false;
        self.irq_latched = false;
        self.fiq_line = false;
        self.total_cycles = 0;
        self.total_instructions = 0;
        self.cache.clear();
    }

    // --- interrupt inputs ------------------------------------------------

    /// Hold the IRQ line. While it is high and unmasked the CPU keeps taking
    /// the exception, so a handler that does not acknowledge is re-entered.
    pub fn set_irq(&mut self, asserted: bool) {
        self.irq_line = asserted;
    }

    /// Latch one IRQ edge, consumed when the CPU takes it. The boot ROM's
    /// vertical-blank handler never acknowledges anything, so a held level
    /// would livelock it.
    pub fn signal_irq(&mut self) {
        self.irq_latched = true;
    }

    /// Hold this CPU's own FIQ input. The effective FIQ input is this OR the
    /// bus's [`CpuBus::fiq_line`].
    pub fn set_fiq(&mut self, asserted: bool) {
        self.fiq_line = asserted;
    }

    // --- decode cache ----------------------------------------------------

    /// Drop every cached decode. Needed whenever code memory may have changed
    /// wholesale: a new BIOS, a save-state load.
    pub fn invalidate_decode_cache(&mut self) {
        self.cache.clear();
    }

    /// Make cached decodes covering `address..address + length` stale. O(1)
    /// per 4 KiB page touched.
    pub fn invalidate_decode_range(&mut self, address: u32, length: u32) {
        if length == 0 {
            return;
        }
        self.invalidations = self.invalidations.wrapping_add(1);
        self.cache.invalidate(address, length);
    }

    // --- registers -------------------------------------------------------

    /// A register of the current mode's view (index taken modulo 16).
    pub fn reg(&self, index: usize) -> u32 {
        self.regs.get(index & 0xf).copied().unwrap_or(0)
    }

    /// Set a register of the current mode's view (index taken modulo 16).
    /// Writing R15 sets the address of the next instruction.
    pub fn set_reg(&mut self, index: usize, value: u32) {
        if let Some(slot) = self.regs.get_mut(index & 0xf) {
            *slot = value;
        }
    }

    pub fn pc(&self) -> u32 {
        self.r(Reg::PC)
    }

    pub fn cpsr(&self) -> u32 {
        self.cpsr
    }

    /// Write the whole CPSR, switching register banks if the mode changes.
    pub fn set_cpsr(&mut self, value: u32) {
        self.switch_mode(value & MODE_MASK);
        self.cpsr = (value & !MODE_MASK) | (self.cpsr & MODE_MASK);
    }

    /// The current mode's SPSR; in User and System mode, which have none, the
    /// CPSR.
    pub fn spsr(&self) -> u32 {
        if has_spsr(self.cpsr) {
            self.banks.get(Bank::of(self.cpsr)).spsr
        } else {
            self.cpsr
        }
    }

    /// The current mode. A mode field that names no ARMv3 mode reads as
    /// [`Mode::User`], whose register bank it uses.
    pub fn mode(&self) -> Mode {
        Mode::from_bits(self.cpsr).unwrap_or(Mode::User)
    }

    // --- counters --------------------------------------------------------

    /// IRQ exceptions taken.
    pub fn irqs_taken(&self) -> u64 {
        self.irqs_taken
    }

    /// FIQ exceptions taken. CLIO drives FIQ, so this says whether the machine
    /// is being serviced at all.
    pub fn fiqs_taken(&self) -> u64 {
        self.fiqs_taken
    }

    /// Instructions decoded (cache misses).
    pub fn decodes(&self) -> u64 {
        self.decodes
    }

    /// Range invalidations requested.
    pub fn invalidations(&self) -> u64 {
        self.invalidations
    }

    /// Cycles executed since reset.
    pub fn total_cycles(&self) -> u64 {
        self.total_cycles
    }

    /// Instructions retired since reset.
    pub fn total_instructions(&self) -> u64 {
        self.total_instructions
    }

    // --- running ---------------------------------------------------------

    /// Run until at least `cycles` have elapsed. Returns the number actually
    /// run, which may overshoot by up to one instruction.
    pub fn run<B: CpuBus>(&mut self, bus: &mut B, cycles: u32) -> u32 {
        let mut spent: u32 = 0;
        while spent < cycles {
            spent = spent.saturating_add(self.step(bus));
        }
        spent
    }

    /// Execute exactly one instruction, then let the bus see the instruction
    /// boundary and sample the interrupt inputs. Returns its cycle count.
    pub fn step<B: CpuBus>(&mut self, bus: &mut B) -> u32 {
        let address = self.r(Reg::PC);
        let decoded = self.decoded_at(bus, address);

        // The pipeline makes PC read as the instruction's address plus eight.
        self.set_r(Reg::PC, address.wrapping_add(8));
        self.pc_written = false;

        let cycles = if decoded.cond.passes(self.cpsr) {
            self.execute(bus, decoded.instr)
        } else {
            S
        };

        if !self.pc_written {
            self.set_r(Reg::PC, address.wrapping_add(4));
        }

        self.total_cycles = self.total_cycles.wrapping_add(u64::from(cycles));
        self.total_instructions = self.total_instructions.wrapping_add(1);

        bus.instruction_boundary();
        // DMA that ran inside this instruction's store may have replaced code.
        while let Some((start, length)) = bus.take_code_invalidation() {
            self.invalidate_decode_range(start, length);
        }
        let fiq = self.fiq_line || bus.fiq_line();
        self.check_interrupts(fiq);
        cycles
    }

    fn decoded_at<B: CpuBus>(&mut self, bus: &mut B, address: u32) -> Decoded {
        match self.cache.slot_mut(address) {
            Some((slot, generation)) => {
                if slot.stamp != generation {
                    self.decodes = self.decodes.wrapping_add(1);
                    *slot = Slot {
                        stamp: generation,
                        decoded: decode(bus.fetch32(address)),
                    };
                }
                slot.decoded
            }
            None => {
                self.decodes = self.decodes.wrapping_add(1);
                decode(bus.fetch32(address))
            }
        }
    }

    // --- register helpers ------------------------------------------------

    fn r(&self, reg: Reg) -> u32 {
        self.regs.get(reg.index()).copied().unwrap_or(0)
    }

    /// A register read in a slot where PC reads twelve ahead rather than eight
    /// (the extra pipeline step of a register-specified shift, and STR/STM of
    /// PC).
    fn r_plus12(&self, reg: Reg) -> u32 {
        let value = self.r(reg);
        if reg.is_pc() {
            value.wrapping_add(4)
        } else {
            value
        }
    }

    /// Set a register without treating a write to R15 as a jump. Used for base
    /// write-back, which the pipeline then overrides for R15.
    fn set_r(&mut self, reg: Reg, value: u32) {
        if let Some(slot) = self.regs.get_mut(reg.index()) {
            *slot = value;
        }
    }

    fn write_pc(&mut self, value: u32) {
        self.set_r(Reg::PC, value);
        self.pc_written = true;
    }

    /// Write an instruction's destination: R15 is a jump.
    fn write_dest(&mut self, reg: Reg, value: u32) {
        if reg.is_pc() {
            self.write_pc(value);
        } else {
            self.set_r(reg, value);
        }
    }

    fn carry(&self) -> bool {
        self.cpsr & FLAG_C != 0
    }

    // --- modes and exceptions ---------------------------------------------

    /// Change the mode field to `new_bits`, swapping banked registers.
    fn switch_mode(&mut self, new_bits: u32) {
        let new_bits = new_bits & MODE_MASK;
        let old_bits = self.cpsr & MODE_MASK;
        if old_bits == new_bits {
            return;
        }

        // R8..R12 are banked only for FIQ.
        let fiq = Mode::Fiq.bits();
        if (old_bits == fiq) != (new_bits == fiq) {
            if let Some(live) = self.regs.get_mut(8..13) {
                live.swap_with_slice(&mut self.shadow_r8_r12);
            }
        }

        let old_bank = Bank::of(old_bits);
        let new_bank = Bank::of(new_bits);
        if old_bank != new_bank {
            let (r13, r14) = (self.r(Reg(13)), self.r(Reg(14)));
            let saved = self.banks.get_mut(old_bank);
            saved.r13 = r13;
            saved.r14 = r14;
            let loaded = *self.banks.get(new_bank);
            self.set_r(Reg(13), loaded.r13);
            self.set_r(Reg(14), loaded.r14);
        }

        self.cpsr = (self.cpsr & !MODE_MASK) | new_bits;
    }

    fn enter_exception(&mut self, vector: u32, mode: Mode, return_address: u32, disable_fiq: bool) {
        let saved_cpsr = self.cpsr;
        self.switch_mode(mode.bits());
        self.banks.get_mut(Bank::of(mode.bits())).spsr = saved_cpsr;
        self.set_r(Reg(14), return_address);
        self.cpsr |= FLAG_I;
        if disable_fiq {
            self.cpsr |= FLAG_F;
        }
        self.write_pc(vector);
    }

    /// Take FIQ, else IRQ, if asserted and unmasked. PC already holds the next
    /// instruction's address; R14 gets that plus the pipeline's four.
    fn check_interrupts(&mut self, fiq: bool) {
        let next = self.r(Reg::PC);
        if fiq && self.cpsr & FLAG_F == 0 {
            self.fiqs_taken = self.fiqs_taken.wrapping_add(1);
            self.enter_exception(VECTOR_FIQ, Mode::Fiq, next.wrapping_add(4), true);
            return;
        }
        if (self.irq_line || self.irq_latched) && self.cpsr & FLAG_I == 0 {
            // A latched edge is consumed; a held line is not.
            self.irq_latched = false;
            self.irqs_taken = self.irqs_taken.wrapping_add(1);
            self.enter_exception(VECTOR_IRQ, Mode::Irq, next.wrapping_add(4), false);
        }
    }

    /// Copy the current mode's SPSR into the CPSR: the exception return of
    /// `MOVS pc, ...` and `LDM ..., {..., pc}^`. Nothing happens in a mode
    /// without an SPSR.
    fn restore_cpsr_from_spsr(&mut self) {
        if has_spsr(self.cpsr) {
            let spsr = self.banks.get(Bank::of(self.cpsr)).spsr;
            self.set_cpsr(spsr);
        }
    }

    // --- execution ---------------------------------------------------------

    fn execute<B: CpuBus>(&mut self, bus: &mut B, instr: Instr) -> u32 {
        match instr {
            Instr::DataProcessing {
                op,
                set_flags,
                rn,
                rd,
                operand,
            } => self.data_processing(op, set_flags, rn, rd, operand),
            Instr::Multiply {
                accumulate,
                set_flags,
                rd,
                rn,
                rs,
                rm,
            } => self.multiply(accumulate, set_flags, rd, rn, rs, rm),
            Instr::SingleTransfer {
                load,
                byte,
                pre_index,
                add,
                write_back,
                rn,
                rd,
                offset,
            } => {
                let transfer = SingleTransfer {
                    load,
                    byte,
                    pre_index,
                    add,
                    write_back,
                    rn,
                    rd,
                };
                self.single_transfer(bus, transfer, offset)
            }
            Instr::BlockTransfer {
                load,
                pre_index,
                add,
                user_bank,
                write_back,
                rn,
                list,
            } => {
                let transfer = BlockTransfer {
                    load,
                    pre_index,
                    add,
                    user_bank,
                    write_back,
                    rn,
                    list,
                };
                self.block_transfer(bus, transfer)
            }
            Instr::Branch { link, offset } => self.branch(link, offset),
            Instr::SoftwareInterrupt => {
                let return_address = self.pc().wrapping_sub(4);
                self.enter_exception(VECTOR_SWI, Mode::Supervisor, return_address, false);
                BRANCH
            }
            Instr::Swap { byte, rn, rd, rm } => self.swap(bus, byte, rn, rd, rm),
            Instr::StatusToRegister { from_spsr, rd } => {
                let status = if from_spsr { self.spsr() } else { self.cpsr };
                self.write_dest(rd, status);
                S
            }
            Instr::RegisterToStatus {
                to_spsr,
                flags_only,
                source,
            } => {
                self.move_to_status(to_spsr, flags_only, source);
                S
            }
            Instr::Undefined => {
                let return_address = self.pc().wrapping_sub(4);
                self.enter_exception(VECTOR_UNDEFINED, Mode::Undefined, return_address, false);
                BRANCH
            }
        }
    }

    fn data_processing(
        &mut self,
        op: AluOp,
        set_flags: bool,
        rn: Reg,
        rd: Reg,
        operand: Operand2,
    ) -> u32 {
        let carry_in = self.carry();
        let mut cycles = S;

        let (operand2, shifter_carry, rn_value) = match operand {
            Operand2::Immediate { value, carry } => (value, carry.unwrap_or(carry_in), self.r(rn)),
            Operand2::ShiftedByImmediate { rm, kind, amount } => {
                let (value, carry) = shift(self.r(rm), kind, u32::from(amount), false, carry_in);
                (value, carry, self.r(rn))
            }
            Operand2::ShiftedByRegister { rm, kind, rs } => {
                // The shift amount comes from a register: one more internal
                // cycle, and PC reads twelve ahead as Rm or Rn.
                cycles += I;
                let amount = self.r(rs) & 0xff;
                let (value, carry) = shift(self.r_plus12(rm), kind, amount, true, carry_in);
                (value, carry, self.r_plus12(rn))
            }
        };

        let (result, arithmetic) = match op {
            AluOp::And | AluOp::Tst => (rn_value & operand2, None),
            AluOp::Eor | AluOp::Teq => (rn_value ^ operand2, None),
            AluOp::Orr => (rn_value | operand2, None),
            AluOp::Mov => (operand2, None),
            AluOp::Bic => (rn_value & !operand2, None),
            AluOp::Mvn => (!operand2, None),
            AluOp::Sub | AluOp::Cmp => split(add_with_carry(rn_value, !operand2, true)),
            AluOp::Rsb => split(add_with_carry(operand2, !rn_value, true)),
            AluOp::Add | AluOp::Cmn => split(add_with_carry(rn_value, operand2, false)),
            AluOp::Adc => split(add_with_carry(rn_value, operand2, carry_in)),
            AluOp::Sbc => split(add_with_carry(rn_value, !operand2, carry_in)),
            AluOp::Rsc => split(add_with_carry(operand2, !rn_value, carry_in)),
        };

        if set_flags {
            if rd.is_pc() {
                // S with PC as destination is the exception return.
                self.restore_cpsr_from_spsr();
            } else {
                let mut flags = self.cpsr & !(FLAG_N | FLAG_Z | FLAG_C | FLAG_V);
                flags |= result & FLAG_N;
                if result == 0 {
                    flags |= FLAG_Z;
                }
                // Arithmetic sets C and V from the adder; logical operations
                // take C from the shifter and leave V alone.
                let (c, v) = arithmetic.unwrap_or((shifter_carry, self.cpsr & FLAG_V != 0));
                if c {
                    flags |= FLAG_C;
                }
                if v {
                    flags |= FLAG_V;
                }
                self.cpsr = flags;
            }
        }

        if op.writes_result() {
            self.write_dest(rd, result);
            if rd.is_pc() {
                cycles += REFILL;
            }
        }
        cycles
    }

    fn multiply(
        &mut self,
        accumulate: bool,
        set_flags: bool,
        rd: Reg,
        rn: Reg,
        rs: Reg,
        rm: Reg,
    ) -> u32 {
        let mut result = self.r(rm).wrapping_mul(self.r(rs));
        if accumulate {
            result = result.wrapping_add(self.r(rn));
        }
        self.write_dest(rd, result);

        if set_flags {
            // C is architecturally unpredictable after a multiply; it is left
            // alone, as is V.
            let mut flags = self.cpsr & !(FLAG_N | FLAG_Z);
            flags |= result & FLAG_N;
            if result == 0 {
                flags |= FLAG_Z;
            }
            self.cpsr = flags;
        }

        // The reference samples the multiplier for timing after the result is
        // written, so Rd == Rs times the product. Kept for cycle parity.
        let mut cycles = S + booth_cycles(self.r(rs)) * I;
        // MLA feeds the accumulator through one more pass of the array.
        if accumulate {
            cycles += I;
        }
        cycles
    }

    fn single_transfer<B: CpuBus>(
        &mut self,
        bus: &mut B,
        t: SingleTransfer,
        offset: Offset,
    ) -> u32 {
        let offset = match offset {
            Offset::Immediate(value) => value,
            Offset::Register { rm, kind, amount } => {
                shift(self.r(rm), kind, u32::from(amount), false, self.carry()).0
            }
        };
        let base = self.r(t.rn);
        let indexed = if t.add {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if t.pre_index { indexed } else { base };

        if t.load {
            let value = if t.byte {
                u32::from(bus.read8(address))
            } else {
                // An unaligned word load rotates the addressed word so the
                // wanted byte lands in the low bits.
                bus.read32(address).rotate_right((address & 3) * 8)
            };
            // Write-back lands before the loaded value, so a load into the base
            // register keeps the loaded value.
            self.single_write_back(&t, indexed);
            self.write_dest(t.rd, value);
            if t.rd.is_pc() {
                S + N + I + REFILL
            } else {
                S + N + I
            }
        } else {
            // A stored PC is the instruction's address plus twelve.
            let value = self.r_plus12(t.rd);
            if t.byte {
                bus.write8(address, value as u8);
            } else {
                bus.write32(address, value);
            }
            self.single_write_back(&t, indexed);
            2 * N
        }
    }

    /// Post-indexing always writes the base back; pre-indexing only with W.
    fn single_write_back(&mut self, t: &SingleTransfer, indexed: u32) {
        if !t.pre_index || t.write_back {
            self.set_r(t.rn, indexed);
        }
    }

    fn block_transfer<B: CpuBus>(&mut self, bus: &mut B, t: BlockTransfer) -> u32 {
        // An empty list is architecturally unpredictable; it does nothing.
        if t.list == 0 {
            return EMPTY_BLOCK;
        }

        let count = t.list.count_ones();
        let total = count * 4;
        let base = self.r(t.rn);

        // Transfers always run upwards from the lowest address, whichever way
        // the addressing mode points.
        let mut address = match (t.add, t.pre_index) {
            (true, true) => base.wrapping_add(4),
            (true, false) => base,
            (false, true) => base.wrapping_sub(total),
            (false, false) => base.wrapping_sub(total).wrapping_add(4),
        };
        let final_base = if t.add {
            base.wrapping_add(total)
        } else {
            base.wrapping_sub(total)
        };

        // S without a PC load means "the user bank": act as User mode for the
        // duration of the transfer.
        let pc_in_list = t.list & 0x8000 != 0;
        let saved_mode = self.cpsr & MODE_MASK;
        let swap_banks = t.user_bank
            && !(t.load && pc_in_list)
            && saved_mode != Mode::User.bits()
            && saved_mode != Mode::System.bits();
        if swap_banks {
            self.switch_mode(Mode::User.bits());
        }

        // The registers in the list, in order. Walking the list's bits beats
        // testing all sixteen: a transfer names a handful of them.
        let mut list = [Reg(0); 16];
        let mut list_len = 0usize;
        let mut bits = t.list;
        while bits != 0 {
            let bit = bits.trailing_zeros();
            bits &= bits - 1;
            if let Some(slot) = list.get_mut(list_len) {
                *slot = Reg(bit as u8);
            }
            list_len += 1;
        }
        let registers = list.get(..list_len).unwrap_or(&[]);
        if t.load {
            // Write-back first, so a base also in the list ends up loaded.
            if t.write_back {
                self.set_r(t.rn, final_base);
            }
            for &reg in registers {
                let value = bus.read32(address);
                self.write_dest(reg, value);
                address = address.wrapping_add(4);
            }
            if pc_in_list && t.user_bank && has_spsr(saved_mode) {
                // LDM with PC and S: the usual way out of an exception handler.
                let spsr = self.banks.get(Bank::of(saved_mode)).spsr;
                self.set_cpsr(spsr);
            }
        } else {
            for &reg in registers {
                let value = self.r_plus12(reg);
                bus.write32(address, value);
                address = address.wrapping_add(4);
            }
            // The base, if in the list, was stored with its original value.
            if t.write_back {
                self.set_r(t.rn, final_base);
            }
        }

        if swap_banks {
            self.switch_mode(saved_mode);
        }

        if t.load {
            let cycles = count * S + N + I;
            if pc_in_list {
                cycles + REFILL
            } else {
                cycles
            }
        } else {
            (count - 1) * S + 2 * N
        }
    }

    fn branch(&mut self, link: bool, offset: i32) -> u32 {
        let pc = self.pc();
        if link {
            // PC reads eight ahead, so the return address is four back.
            self.set_r(Reg(14), pc.wrapping_sub(4));
        }
        self.write_pc(pc.wrapping_add_signed(offset));
        BRANCH
    }

    fn swap<B: CpuBus>(&mut self, bus: &mut B, byte: bool, rn: Reg, rd: Reg, rm: Reg) -> u32 {
        let address = self.r(rn);
        let old = if byte {
            let old = u32::from(bus.read8(address));
            bus.write8(address, self.r(rm) as u8);
            old
        } else {
            let old = bus.read32(address).rotate_right((address & 3) * 8);
            bus.write32(address, self.r(rm));
            old
        };
        self.write_dest(rd, old);
        SWAP
    }

    fn move_to_status(&mut self, to_spsr: bool, flags_only: bool, source: MsrSource) {
        const FLAGS: u32 = 0xf000_0000;
        let value = match source {
            MsrSource::Immediate(value) => value,
            MsrSource::Register(rm) => self.r(rm),
        };
        let mode_bits = self.cpsr & MODE_MASK;
        let privileged = mode_bits != Mode::User.bits();

        if to_spsr {
            if has_spsr(mode_bits) {
                let target = &mut self.banks.get_mut(Bank::of(mode_bits)).spsr;
                *target = if flags_only {
                    (*target & !FLAGS) | (value & FLAGS)
                } else {
                    value
                };
            }
        } else if flags_only || !privileged {
            // User mode may only change the condition flags.
            self.cpsr = (self.cpsr & !FLAGS) | (value & FLAGS);
        } else {
            self.set_cpsr(value);
        }
    }
}

/// The addressing bits of LDR/STR, grouped for the executor.
struct SingleTransfer {
    load: bool,
    byte: bool,
    pre_index: bool,
    add: bool,
    write_back: bool,
    rn: Reg,
    rd: Reg,
}

/// The addressing bits of LDM/STM, grouped for the executor.
struct BlockTransfer {
    load: bool,
    pre_index: bool,
    add: bool,
    user_bank: bool,
    write_back: bool,
    rn: Reg,
    list: u16,
}

fn split((sum, carry, overflow): (u32, bool, bool)) -> (u32, Option<(bool, bool)>) {
    (sum, Some((carry, overflow)))
}
