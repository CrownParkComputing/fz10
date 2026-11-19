#![forbid(unsafe_code)]
//! DSPP: the 3DO's audio digital signal processor.
//!
//! A 16-bit fixed-point machine with its own instruction set and its own two
//! memories. The CPU loads a program through CLIO's window, points MADAM's DMA
//! channels at sample buffers and then leaves it alone: the DSPP runs one
//! complete pass of that program per audio sample, mixes whatever the channels
//! hand it, writes two words to the DACs and sleeps until the next sample.
//!
//! The audio folio's frame clock is driven by the interrupt a DSPP program
//! raises, so a machine without a DSPP does not merely run silent: it stops
//! after its opening logo with every other subsystem healthy.
//!
//! # Shape of the machine
//!
//! * Program memory (NMem): 1024 words, filled with SLEEP at power-on so a
//!   DSPP that is started before it is loaded halts at once.
//! * Data memory (IMem): 1024 words, the top of the range mapped to hardware:
//!   the DACs, the semaphore pair, the interrupt word, and windows onto the
//!   thirteen input and four output DMA channels.
//! * A ten-bit program counter. A pass starts at zero and ends at SLEEP.
//!
//! # Instruction diagrams (after WO 94/16383)
//!
//! Every instruction is one word; bit 15 separates the two families.
//!
//! ```text
//! arithmetic  0 | NUMOPS:2 | M2SEL | MUXA:2 | MUXB:2 | ALU:4 | BS:4
//! control     1 | ........ opcode:8 (bits 14..7) ....... | operand
//! ```
//!
//! An arithmetic instruction is followed by operand words, each one an
//! "operand-group packet" whose top three bits say how to read it:
//!
//! ```text
//! 0..3  three registers   0 | R3DI | R3:4 | R2DI | R2:4 | R1DI | R1:4
//! 4     address           100 | WB1 | . | DI | ADDRESS:10
//! 5     two registers     101 | WB2 | WB1 | NUMREGS | R2DI | R2:4 | R1DI | R1:4
//! 6, 7  immediate         11 | JUSTIFY | VALUE:13 (signed)
//! ```
//!
//! The values a packet supplies feed, in a fixed order, the multiplier's two
//! inputs, the ALU's two latches and the barrel shifter; a packet's spare value
//! or explicit write-back bit names where the result goes.
//!
//! The DMA channels belong to MADAM and the interrupt belongs to CLIO, so the
//! DSPP reaches both through a [`DspHost`] that the machine passes to
//! [`Dsp::run`]; it holds no reference to any other chip.

/// What the DSPP needs from the rest of the machine. The DMA channels are
/// MADAM's and the audio interrupt is CLIO's; the machine wires them up.
pub trait DspHost {
    /// Take the next word from an input channel, advancing it.
    fn input_next(&mut self, channel: u16) -> u16;
    /// The word an input channel currently points at, without advancing.
    fn input_peek(&mut self, channel: u16) -> u16;
    /// An input channel's status word.
    fn input_status(&mut self, channel: u16) -> u16;
    /// An output channel's status word.
    fn output_status(&mut self, channel: u16) -> u16;
    /// Hand a word to an output channel.
    fn output(&mut self, channel: u16, value: u16);
    /// The DSPP has asked for the audio interrupt.
    fn audio_interrupt(&mut self);
}

/// A host with nothing attached: every channel reads zero, output is dropped
/// and the interrupt goes nowhere. This is what a DSPP sees with no machine
/// around it, which is useful for tests and for running a program in isolation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NullHost;

impl DspHost for NullHost {
    fn input_next(&mut self, _channel: u16) -> u16 {
        0
    }
    fn input_peek(&mut self, _channel: u16) -> u16 {
        0
    }
    fn input_status(&mut self, _channel: u16) -> u16 {
        0
    }
    fn output_status(&mut self, _channel: u16) -> u16 {
        0
    }
    fn output(&mut self, _channel: u16, _value: u16) {}
    fn audio_interrupt(&mut self) {}
}

/// CLIO register offsets of the DSPP window (byte offsets inside CLIO).
///
/// The window is several differently-shaped views, not one register file:
///
/// ```text
/// 0x17D0          semaphore (read: status << 16 | data, write: CPU data)
/// 0x17E8          reset (write)
/// 0x17FC          run (write: non-zero starts, zero stops)
/// 0x1800-0x1FFF   program memory, two DSPP words per CPU word (0x400 mirrors)
/// 0x2000-0x2FFF   program memory, one DSPP word per CPU word (0x800 mirrors)
/// 0x3000-0x33FF   data memory, two words per CPU word (write)
/// 0x3400-0x37FF   data memory, one word per CPU word (write)
/// 0x3800-0x3BFF   data memory from 0x300, two words per CPU word (read)
/// 0x3C00-0x3FFF   data memory from 0x300, one word per CPU word (read)
/// ```
pub mod window {
    /// First offset of the whole DSPP window in CLIO.
    pub const BASE: u32 = 0x1000;
    /// One past the last offset of the window.
    pub const END: u32 = 0x4000;
    /// Program memory, two words per CPU word.
    pub const PROGRAM_PAIRS: u32 = 0x1800;
    /// Program memory, one word per CPU word.
    pub const PROGRAM_SINGLE: u32 = 0x2000;
    /// Data memory writes, two words per CPU word.
    pub const DATA_PAIRS: u32 = 0x3000;
    /// Data memory writes, one word per CPU word.
    pub const DATA_SINGLE: u32 = 0x3400;
    /// Data memory reads (from 0x300), two words per CPU word.
    pub const READ_PAIRS: u32 = 0x3800;
    /// Data memory reads (from 0x300), one word per CPU word.
    pub const READ_SINGLE: u32 = 0x3C00;
    /// The CPU side of the semaphore.
    pub const SEMAPHORE: u32 = 0x17D0;
    /// Resets the DSPP's sequencer.
    pub const RESET: u32 = 0x17E8;
    /// CLIO's own noise source; not the DSPP's, and not handled here.
    pub const NOISE: u32 = 0x17F0;
    /// Starts (non-zero) or stops (zero) the DSPP.
    pub const RUN: u32 = 0x17FC;
}

const MEMORY_WORDS: usize = 1024;
const ADDRESS_MASK: u16 = 0x3ff;

/// The SLEEP control word, which program memory holds at power-on.
pub const SLEEP: u16 = 0x8380;

/// A pass ends at SLEEP, so the program bounds it - but a loop with no SLEEP
/// would spin for ever inside one audio sample. This is the backstop.
pub const MAX_STEPS_PER_PASS: u32 = 1 << 20;

/// Ticks of the DSPP's own clock in one audio sample (25 MHz / 44.1 kHz).
const SYSTEM_TICKS: i16 = 568;

/// Data addresses with a hardware meaning, as the DSPP sees them.
mod port {
    pub const NOISE: u16 = 0x0ea;
    pub const AUDIO_OUT_STATUS: u16 = 0x0eb;
    pub const SEMAPHORE_STATUS: u16 = 0x0ec;
    pub const SEMAPHORE_DATA: u16 = 0x0ed;
    pub const PC: u16 = 0x0ee;
    pub const COUNTER: u16 = 0x0ef;

    pub const WRITE_AUDIO_OUT_STATUS: u16 = 0x3eb;
    pub const WRITE_SEMAPHORE_ACK: u16 = 0x3ec;
    pub const WRITE_SEMAPHORE_DATA: u16 = 0x3ed;
    pub const WRITE_INTERRUPT: u16 = 0x3ee;
    pub const WRITE_RELOAD: u16 = 0x3ef;
    pub const FLUSH_OUTPUT: u16 = 0x3fd;
    pub const LEFT_DAC: u16 = 0x3fe;
    pub const RIGHT_DAC: u16 = 0x3ff;
}

/// Semaphore status values.
const SEMAPHORE_ACKNOWLEDGED: u16 = 0x01;
const SEMAPHORE_DSPP_WROTE: u16 = 0x04;
const SEMAPHORE_CPU_WROTE: u16 = 0x08;

/// Extract `bits` bits of `word` starting at `shift`.
const fn field(word: u16, shift: u32, bits: u32) -> u16 {
    (word >> shift) & ((1u16 << bits) - 1)
}

const fn bit(word: u16, shift: u32) -> bool {
    (word >> shift) & 1 != 0
}

// ---------------------------------------------------------------------------
// Instruction decode
// ---------------------------------------------------------------------------

/// The sixteen ALU operations (the patent's ASEL field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AluOp {
    TransferA,
    NegateB,
    Add,
    /// A plus the carry: the datapath swaps B for the carry (ACSBU).
    AddCarry,
    Subtract,
    /// A minus the carry: the datapath swaps B for the carry (ACSBU).
    SubtractBorrow,
    Increment,
    Decrement,
    LogicalTransferA,
    Not,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
}

impl AluOp {
    const fn from_bits(bits: u16) -> Self {
        match bits & 0x0f {
            0 => Self::TransferA,
            1 => Self::NegateB,
            2 => Self::Add,
            3 => Self::AddCarry,
            4 => Self::Subtract,
            5 => Self::SubtractBorrow,
            6 => Self::Increment,
            7 => Self::Decrement,
            8 => Self::LogicalTransferA,
            9 => Self::Not,
            10 => Self::And,
            11 => Self::Nand,
            12 => Self::Or,
            13 => Self::Nor,
            14 => Self::Xor,
            _ => Self::Xnor,
        }
    }

    /// The two operations whose B input is the carry, not an operand.
    const fn uses_carry(self) -> bool {
        matches!(self, Self::AddCarry | Self::SubtractBorrow)
    }
}

/// What an ALU input multiplexer selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    Accumulator,
    Alu1,
    Alu2,
    Multiplier,
}

impl Input {
    const fn from_bits(bits: u16) -> Self {
        match bits & 3 {
            0 => Self::Accumulator,
            1 => Self::Alu1,
            2 => Self::Alu2,
            _ => Self::Multiplier,
        }
    }
}

/// The operand slots an arithmetic instruction asks to have filled. The bit
/// order matches the operand mask set by the MASK control instruction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Requests(u8);

impl Requests {
    const SHIFT: u8 = 1 << 0;
    const ALU2: u8 = 1 << 1;
    const ALU1: u8 = 1 << 2;
    const MULT2: u8 = 1 << 3;
    const MULT1: u8 = 1 << 4;

    const fn has(self, slot: u8) -> bool {
        self.0 & slot != 0
    }
}

/// An arithmetic instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Arithmetic {
    /// The four-bit BS field. 8 means "the shift comes from an operand".
    shift_field: u16,
    alu: AluOp,
    alu_bits: u16,
    mux_a: Input,
    mux_b: Input,
    /// The multiplier's second input is an operand rather than the accumulator.
    m2sel: bool,
    /// How many operand values the instruction says follow (0 = as needed).
    operand_count: u16,
}

impl Arithmetic {
    const fn decode(word: u16) -> Self {
        Self {
            shift_field: field(word, 0, 4),
            alu: AluOp::from_bits(field(word, 4, 4)),
            alu_bits: field(word, 4, 4),
            mux_b: Input::from_bits(field(word, 8, 2)),
            mux_a: Input::from_bits(field(word, 10, 2)),
            m2sel: bit(word, 12),
            operand_count: field(word, 13, 2),
        }
    }

    /// The five-bit barrel shifter code: four bits from BS, and a fifth
    /// borrowed from the top of the ALU field, which separates the logical
    /// right shifts from the arithmetic ones.
    const fn shift_code(self) -> u8 {
        (self.shift_field | ((self.alu_bits & 8) << 1)) as u8
    }

    /// Which operand slots must be filled, from the multiplexer selections.
    const fn requests(self) -> Requests {
        let mut slots = 0u8;
        if self.shift_field == 8 {
            slots |= Requests::SHIFT;
        }
        slots |= self.input_slots(self.mux_a);
        slots |= self.input_slots(self.mux_b);
        Requests(slots)
    }

    const fn input_slots(self, input: Input) -> u8 {
        match input {
            Input::Accumulator => 0,
            Input::Alu1 => Requests::ALU1,
            Input::Alu2 => Requests::ALU2,
            Input::Multiplier => {
                if self.m2sel {
                    Requests::MULT1 | Requests::MULT2
                } else {
                    Requests::MULT1
                }
            }
        }
    }
}

/// A conditional branch's five-bit condition field.
///
/// ```text
/// bit 4..3  mode: 1 every selected flag set, 2 every selected flag clear,
///                 3 signed / unsigned / exactness comparison
/// bit 2     select C/Z instead of N/V
/// bit 1     mask1: include status 0 (N or C); in mode 3 complements
/// bit 0     mask0: include status 1 (V or Z); in mode 3 selects variants
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Condition(u16);

impl Condition {
    fn holds(self, status: Status) -> bool {
        let mask0 = bit(self.0, 0);
        let mask1 = bit(self.0, 1);
        let select_cz = bit(self.0, 2);
        let mode = field(self.0, 3, 2);

        match mode {
            0 => false,
            // Mode 1 with neither mask bit: all twenty result bits zero, or
            // its complement, selected by S.
            1 if !mask0 && !mask1 => {
                let all_zero = status.zero && status.exact;
                all_zero != select_cz
            }
            1 | 2 => {
                let wanted = mode == 1;
                let (status0, status1) = if select_cz {
                    (status.carry, status.zero)
                } else {
                    (status.negative, status.overflow)
                };
                (!mask1 || status0 == wanted) && (!mask0 || status1 == wanted)
            }
            // Signed: N xor V is less-than; mask0 includes equal and mask1
            // complements the result.
            _ if !select_cz => {
                ((status.negative != status.overflow) || (status.zero && mask0)) != mask1
            }
            // Unsigned higher is C and not Z; mask0 complements it.
            _ if !mask1 => (status.carry && !status.zero) != mask0,
            // Exact or not exact.
            _ => status.exact != mask0,
        }
    }
}

/// A control instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Nop,
    /// Jump to the top ten bits of the accumulator's high half.
    BranchToAccumulator,
    /// Set the register base, already scaled by four.
    SetRegisterBase(u16),
    SelectRegisterMap(u8),
    Return,
    /// The operand mask: a set bit lets that slot be filled.
    SetOperandMask(u16),
    Sleep,
    Jump(u16),
    JumpSubroutine(u16),
    /// MOVE to a register-addressed destination.
    MoveToRegister {
        register: u16,
        indirect: bool,
    },
    /// MOVE to a directly addressed destination.
    MoveToAddress {
        address: u16,
        indirect: bool,
    },
    Branch {
        condition: Condition,
        target: u16,
    },
}

impl Control {
    const fn decode(word: u16) -> Self {
        let opcode = field(word, 7, 8);
        let address = field(word, 0, 10);
        match opcode {
            1 => Self::BranchToAccumulator,
            2 => Self::SetRegisterBase((address & 0x3f) << 2),
            3 => Self::SelectRegisterMap((address & 7) as u8),
            4 => Self::Return,
            5 => Self::SetOperandMask(!(address & 0x1f)),
            7 => Self::Sleep,
            8..=15 | 24..=31 => Self::Jump(address),
            16..=23 => Self::JumpSubroutine(address),
            32..=47 => Self::MoveToRegister {
                register: field(word, 0, 4),
                indirect: bit(word, 4),
            },
            48..=63 => Self::MoveToAddress {
                address,
                indirect: bit(word, 10),
            },
            64.. => Self::Branch {
                condition: Condition(field(word, 10, 5)),
                target: address,
            },
            _ => Self::Nop,
        }
    }
}

/// One instruction word, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Instruction {
    Arithmetic(Arithmetic),
    Control(Control),
}

impl Instruction {
    const fn decode(word: u16) -> Self {
        if bit(word, 15) {
            Self::Control(Control::decode(word))
        } else {
            Self::Arithmetic(Arithmetic::decode(word))
        }
    }
}

/// A register field of an operand word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegisterRef {
    number: u16,
    indirect: bool,
}

impl RegisterRef {
    const fn at(word: u16, shift: u32) -> Self {
        Self {
            number: field(word, shift, 4),
            indirect: bit(word, shift + 4),
        }
    }
}

/// An operand word, decoded by its top three bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperandWord {
    /// Supplies R3, R2, R1 in that order; R1's register is the destination.
    ThreeRegisters {
        r1: RegisterRef,
        r2: RegisterRef,
        r3: RegisterRef,
    },
    /// One value from a data address, which is also the destination.
    Address {
        address: u16,
        indirect: bool,
        write_back: bool,
    },
    /// R2 (when present) then R1; the destination is R1's effective address.
    TwoRegisters {
        r1: RegisterRef,
        r1_write_back: bool,
        r2: Option<(RegisterRef, bool)>,
    },
    /// A signed thirteen-bit value, optionally justified left by three.
    Immediate(u16),
}

impl OperandWord {
    const fn decode(word: u16) -> Self {
        match field(word, 13, 3) {
            0..=3 => Self::ThreeRegisters {
                r1: RegisterRef::at(word, 0),
                r2: RegisterRef::at(word, 5),
                r3: RegisterRef::at(word, 10),
            },
            4 => Self::Address {
                address: field(word, 0, 10),
                indirect: bit(word, 10),
                write_back: bit(word, 11),
            },
            5 => Self::TwoRegisters {
                r1: RegisterRef::at(word, 0),
                r1_write_back: bit(word, 11),
                r2: if bit(word, 10) {
                    Some((RegisterRef::at(word, 5), bit(word, 12)))
                } else {
                    None
                },
            },
            _ => Self::Immediate(immediate_value(word)),
        }
    }
}

/// An immediate is thirteen bits, signed, and justified by nothing or by
/// three bits. (The justify field is a one-bit signed field: it reads as zero
/// or minus one, and minus one in two bits is three, not one.)
const fn immediate_value(word: u16) -> u16 {
    let value = ((field(word, 0, 13) << 3) as i16) >> 3;
    let justify = if bit(word, 13) { 3 } else { 0 };
    (value << justify) as u16
}

/// The data address a register number lands on under one of the eight
/// register maps (before the register base is applied). The map decides which
/// of two banks a register lives in, which lets one program body serve either
/// half of a stereo pair.
const fn register_address(map: u8, register: u16) -> u16 {
    let register = register & 0x0f;
    let x = (register >> 2) & 1;
    let y = (register >> 3) & 1;
    let twist = match map {
        0..=3 => x,
        4 => y,
        5 => y ^ 1,
        6 => x & y,
        _ => x | y,
    };
    (register & 7) | (twist << 8) | ((register >> 3) << 9)
}

// ---------------------------------------------------------------------------
// ALU and barrel shifter
// ---------------------------------------------------------------------------

/// The status latch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Status {
    zero: bool,
    negative: bool,
    carry: bool,
    overflow: bool,
    /// The low "fraction" bits (15..12) of the result are all zero.
    exact: bool,
}

const TOP: u32 = 0x8000_0000;

const fn add_carry(a: u32, b: u32, y: u32) -> bool {
    (a & b | a & !y | b & !y) & TOP != 0
}
const fn sub_carry(a: u32, b: u32, y: u32) -> bool {
    (a & !b | a & !y | !b & !y) & TOP != 0
}
const fn add_overflow(a: u32, b: u32, y: u32) -> bool {
    (a & b & !y | !a & !b & y) & TOP != 0
}
const fn sub_overflow(a: u32, b: u32, y: u32) -> bool {
    (a & !b & !y | !a & b & y) & TOP != 0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AluResult {
    value: u32,
    carry: bool,
    overflow: bool,
}

impl AluResult {
    const fn logical(value: u32) -> Self {
        Self {
            value,
            carry: false,
            overflow: false,
        }
    }
    const fn add(a: u32, b: u32) -> Self {
        let value = a.wrapping_add(b);
        Self {
            value,
            carry: add_carry(a, b, value),
            overflow: add_overflow(a, b, value),
        }
    }
    const fn subtract(a: u32, b: u32) -> Self {
        let value = a.wrapping_sub(b);
        Self {
            value,
            carry: sub_carry(a, b, value),
            overflow: sub_overflow(a, b, value),
        }
    }
}

/// The ALU, on 32-bit values whose top sixteen bits are the DSPP word.
/// Logical operations clear carry and overflow.
const fn evaluate_alu(op: AluOp, a: u32, b: u32) -> AluResult {
    const ONE_LSB_OF_FRACTION: u32 = 0x1000;
    match op {
        AluOp::TransferA | AluOp::LogicalTransferA => AluResult::logical(a),
        AluOp::NegateB => AluResult::subtract(0, b),
        AluOp::Add | AluOp::AddCarry => AluResult::add(a, b),
        AluOp::Subtract | AluOp::SubtractBorrow => AluResult::subtract(a, b),
        AluOp::Increment => AluResult::add(a, ONE_LSB_OF_FRACTION),
        AluOp::Decrement => AluResult::subtract(a, ONE_LSB_OF_FRACTION),
        AluOp::Not => AluResult::logical(!a),
        AluOp::And => AluResult::logical(a & b),
        AluOp::Nand => AluResult::logical(!(a & b)),
        AluOp::Or => AluResult::logical(a | b),
        AluOp::Nor => AluResult::logical(!(a | b)),
        AluOp::Xor => AluResult::logical(a ^ b),
        AluOp::Xnor => AluResult::logical(!(a ^ b)),
    }
}

/// Left shift amounts for codes whose low four bits are 1..=6.
const fn left_amount(low: u8) -> u32 {
    match low {
        6 => 8,
        n => n as u32,
    }
}

/// Right shift amounts for codes 9..=15 (arithmetic) and 25..=31 (logical).
const fn right_amount(code: u8) -> u32 {
    match code & 0x0f {
        9 => 16,
        10 => 8,
        11 => 5,
        12 => 4,
        13 => 3,
        14 => 2,
        _ => 1,
    }
}

/// The barrel shifter, after the ALU. Only code 8/24 (shift left through the
/// carry) touches the status latch.
fn barrel_shift(code: u8, value: u32, status: &mut Status) -> u32 {
    match (code, code & 0x0f) {
        (_, low @ 1..=6) => value << left_amount(low),
        // Saturate on overflow: the sign of the wrapped result says which way.
        (7 | 23, _) => match (status.overflow, status.negative) {
            (true, true) => 0x7fff_f000,
            (true, false) => 0x8000_0000,
            (false, _) => value,
        },
        // Shift left one through the carry, keeping the fraction nibble.
        (8 | 24, _) => {
            let shifted_out = value & TOP != 0;
            status.carry = shifted_out;
            ((value << 1) & 0xfffe_0000) | (u32::from(shifted_out) << 16) | (value & 0xf000)
        }
        (9..=15, _) => ((value as i32) >> right_amount(code)) as u32,
        (25..=31, _) => value >> right_amount(code),
        _ => value,
    }
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// The values one operand word contributed, and the destinations it names.
#[derive(Clone, Copy, Debug, Default)]
struct OperandPacket {
    values: [u16; 3],
    count: usize,
    /// The packet's natural destination, used when a value is left over.
    destination: u16,
    /// An explicit write-back selection. The latch holds an address and zero
    /// reads as "none", so marking address zero clears an earlier mark.
    marked: Option<u16>,
}

impl OperandPacket {
    fn push(&mut self, value: u16) {
        if let Some(slot) = self.values.get_mut(self.count) {
            *slot = value;
            self.count += 1;
        }
    }

    fn values(&self) -> &[u16] {
        self.values.get(..self.count).unwrap_or(&[])
    }
}

const fn nonzero(address: u16) -> Option<u16> {
    if address != 0 {
        Some(address)
    } else {
        None
    }
}

/// State local to one pass: a pass starts with a clear accumulator and status.
#[derive(Clone, Copy, Debug, Default)]
struct Pass {
    accumulator: u32,
    return_address: u32,
    status: Status,
}

/// The DSPP.
#[derive(Clone, Debug)]
pub struct Dsp {
    program: [u16; MEMORY_WORDS],
    data: [u16; MEMORY_WORDS],

    pc: u32,
    /// Register base, already scaled by four; XORed into register addresses.
    register_base: u16,
    register_map: u8,
    /// A set bit lets the matching operand slot be filled.
    operand_mask: u16,

    /// The multiplier inputs and ALU latches keep their values between
    /// instructions (and passes) until an operand refills them.
    mult1: u16,
    mult2: u16,
    alu1: u16,
    alu2: u16,

    /// Input channels (0..=12) whose next read takes a value the CPU wrote.
    cpu_supplied: u16,

    audio_out_status: u16,
    semaphore_status: u16,
    semaphore_data: u16,
    interrupt_word: u16,
    interrupt_pending: bool,
    counter: i16,
    reload: i16,

    running: bool,
    noise_state: u32,
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Dsp {
    /// A DSPP at power-on: program memory full of SLEEP, stopped.
    pub fn new() -> Self {
        let mut dsp = Self {
            program: [SLEEP; MEMORY_WORDS],
            data: [0; MEMORY_WORDS],
            pc: 0,
            register_base: 0,
            register_map: 0,
            operand_mask: 0xffff,
            mult1: 0,
            mult2: 0,
            alu1: 0,
            alu2: 0,
            cpu_supplied: 0,
            audio_out_status: 0,
            semaphore_status: 0,
            semaphore_data: 0,
            interrupt_word: 0,
            interrupt_pending: false,
            counter: SYSTEM_TICKS,
            reload: SYSTEM_TICKS,
            running: false,
            noise_state: 0xdead_beef,
        };
        dsp.reset();
        dsp
    }

    /// Reset the sequencer: PC, register base and map, operand mask and the
    /// sample counter. Memories, semaphore and the run state are untouched.
    pub fn reset(&mut self) {
        self.counter = self.reload;
        self.pc = 0;
        self.register_base = 0;
        self.register_map = 0;
        self.operand_mask = 0xffff;
    }

    /// Start or stop the DSPP. A stopped DSPP is not merely quiet: it never
    /// reaches the instruction that raises the audio interrupt.
    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// The DAC words as `(right << 16) | left`.
    pub fn dac_words(&self) -> u32 {
        (u32::from(self.data_word(port::RIGHT_DAC)) << 16)
            | u32::from(self.data_word(port::LEFT_DAC))
    }

    /// Run one pass of the program, up to its SLEEP (one audio sample), and
    /// return the DAC words as `(right << 16) | left`. A stopped DSPP does
    /// nothing and returns the DACs as they stand.
    pub fn run(&mut self, host: &mut impl DspHost) -> u32 {
        if !self.running {
            return self.dac_words();
        }
        self.reset();

        let mut pass = Pass::default();
        for _ in 0..MAX_STEPS_PER_PASS {
            let word = self.next_program_word();
            match Instruction::decode(word) {
                Instruction::Control(Control::Sleep) => break,
                Instruction::Control(control) => self.execute_control(host, control, &mut pass),
                Instruction::Arithmetic(arithmetic) => {
                    self.execute_arithmetic(host, arithmetic, &mut pass);
                }
            }
        }

        if self.interrupt_pending {
            self.interrupt_pending = false;
            host.audio_interrupt();
        }

        self.counter = self.counter.wrapping_sub(SYSTEM_TICKS);
        if self.counter <= 0 {
            self.counter = self.counter.wrapping_add(self.reload);
        }

        self.dac_words()
    }

    // --- CPU side ----------------------------------------------------------

    /// Write one program word (the address wraps at 1024).
    pub fn write_program(&mut self, address: u16, value: u16) {
        if let Some(slot) = self.program.get_mut(usize::from(address & ADDRESS_MASK)) {
            *slot = value;
        }
    }

    /// Write two program words from one CPU word: the high half at `address`,
    /// the low half at `address + 1`.
    pub fn write_program_pair(&mut self, address: u16, value: u32) {
        self.write_program(address, (value >> 16) as u16);
        self.write_program(address.wrapping_add(1), value as u16);
    }

    /// Write one data word from the CPU. Only the low 128 words are the
    /// CPU's, and 0x70..=0x7C is special: a value written there is handed to
    /// the DSPP, which reads it once and then goes back to its DMA channel.
    pub fn write_data(&mut self, address: u16, value: u16) {
        if (0x70..=0x7c).contains(&address) {
            self.cpu_supplied |= 1 << (address - 0x70);
            self.set_data_word(address & 0x7f, value);
        } else if address & 0x80 == 0 {
            self.set_data_word(address & 0x7f, value);
        }
    }

    /// Write two data words from one CPU word: high half at `address`, low
    /// half at `address + 1`.
    pub fn write_data_pair(&mut self, address: u16, value: u32) {
        self.write_data(address, (value >> 16) as u16);
        self.write_data(address.wrapping_add(1), value as u16);
    }

    /// Read a data word as the CPU sees it. 0x3EB..=0x3EF read back the audio
    /// output status, semaphore, interrupt word and counter reload.
    pub fn read_data(&self, address: u16) -> u16 {
        match address {
            port::WRITE_AUDIO_OUT_STATUS => self.audio_out_status,
            port::WRITE_SEMAPHORE_ACK => self.semaphore_status,
            port::WRITE_SEMAPHORE_DATA => self.semaphore_data,
            port::WRITE_INTERRUPT => self.interrupt_word,
            port::WRITE_RELOAD => self.reload as u16,
            _ => self.data_word(address),
        }
    }

    /// Two data words as one CPU word: `read_data(address) << 16 |
    /// read_data(address + 1)`.
    pub fn read_data_pair(&self, address: u16) -> u32 {
        (u32::from(self.read_data(address)) << 16)
            | u32::from(self.read_data(address.wrapping_add(1)))
    }

    /// The CPU's side of the semaphore: it writes data and marks itself as
    /// the last writer.
    pub fn write_semaphore(&mut self, value: u32) {
        self.semaphore_data = value as u16;
        self.semaphore_status = SEMAPHORE_CPU_WROTE;
    }

    /// `status << 16 | data`.
    pub fn read_semaphore(&self) -> u32 {
        (u32::from(self.semaphore_status) << 16) | u32::from(self.semaphore_data)
    }

    /// A CPU store into CLIO's DSPP window (see [`window`]). Returns `true`
    /// when the DSPP took it; `false` means CLIO should keep the word in its
    /// own raw window image (unmapped offsets, including the noise register).
    pub fn cpu_window_write(&mut self, offset: u32, value: u32) -> bool {
        use window::*;
        match offset {
            PROGRAM_PAIRS..=0x1fff => {
                // The 0x400 bit is a mirror.
                let address = ((offset & !0x400) - PROGRAM_PAIRS) >> 1;
                self.write_program_pair(address as u16, value);
            }
            PROGRAM_SINGLE..=0x2fff => {
                // The 0x800 bit is a mirror.
                let address = ((offset & !0x800) - PROGRAM_SINGLE) >> 2;
                self.write_program(address as u16, value as u16);
            }
            DATA_PAIRS..=0x33ff => {
                let address = ((offset - DATA_PAIRS) >> 1) & 0xff;
                self.write_data_pair(address as u16, value);
            }
            DATA_SINGLE..=0x37ff => {
                let address = ((offset - DATA_SINGLE) >> 2) & 0xff;
                self.write_data(address as u16, value as u16);
            }
            // The read views ignore stores.
            READ_PAIRS..=0x3fff => {}
            SEMAPHORE => self.write_semaphore(value),
            RESET => self.reset(),
            RUN => self.set_running(value != 0),
            _ => return false,
        }
        true
    }

    /// A CPU load from CLIO's DSPP window. `None` means the DSPP has nothing
    /// there and CLIO should answer from its own raw window image (or, for
    /// [`window::NOISE`], its own noise source).
    pub fn cpu_window_read(&self, offset: u32) -> Option<u32> {
        use window::*;
        match offset {
            READ_PAIRS..=0x3bff => {
                let address = (((offset - READ_PAIRS) >> 1) & 0xff) + 0x300;
                Some(self.read_data_pair(address as u16))
            }
            READ_SINGLE..=0x3fff => {
                let address = (((offset - READ_SINGLE) >> 2) & 0xff) + 0x300;
                Some(u32::from(self.read_data(address as u16)))
            }
            SEMAPHORE => Some(self.read_semaphore()),
            _ => None,
        }
    }

    // --- Diagnostics -------------------------------------------------------

    /// A raw program word; no hardware path, changes nothing.
    pub fn peek_program(&self, address: u16) -> u16 {
        self.program
            .get(usize::from(address & ADDRESS_MASK))
            .copied()
            .unwrap_or(0)
    }

    /// A raw data word; no hardware path, changes nothing.
    pub fn peek_raw_data(&self, address: u16) -> u16 {
        self.data_word(address)
    }

    /// Overwrite a raw data word, bypassing the CPU's write rules.
    pub fn poke_raw_data(&mut self, address: u16, value: u16) {
        self.set_data_word(address, value);
    }

    /// Where the program counter stands (after the last pass, one past its
    /// SLEEP). Not masked: it reads exactly as the DSPP's PC port would.
    pub fn pc(&self) -> u32 {
        self.pc
    }

    // --- Internals ---------------------------------------------------------

    fn data_word(&self, address: u16) -> u16 {
        self.data
            .get(usize::from(address & ADDRESS_MASK))
            .copied()
            .unwrap_or(0)
    }

    fn set_data_word(&mut self, address: u16, value: u16) {
        if let Some(slot) = self.data.get_mut(usize::from(address & ADDRESS_MASK)) {
            *slot = value;
        }
    }

    fn next_program_word(&mut self) -> u16 {
        let word = self.peek_program(self.pc as u16);
        self.pc = self.pc.wrapping_add(1);
        word
    }

    fn noise(&mut self) -> u16 {
        self.noise_state = self.noise_state.wrapping_add(0xfc15);
        let hashed = self.noise_state.wrapping_mul(0x02ab);
        ((hashed >> 16) ^ hashed) as u16
    }

    /// Take a CPU-supplied value for an input channel, if one is waiting.
    fn take_cpu_value(&mut self, channel: u16) -> bool {
        let flag = 1u16.checked_shl(u32::from(channel)).unwrap_or(0);
        let waiting = self.cpu_supplied & flag != 0;
        self.cpu_supplied &= !flag;
        waiting
    }

    fn cpu_value_waiting(&self, channel: u16) -> bool {
        self.cpu_supplied & 1u16.checked_shl(u32::from(channel)).unwrap_or(0) != 0
    }

    /// The DSPP's read of its data space. The low addresses are not all
    /// memory: they are the DMA channel windows, the noise source, the
    /// semaphore, the PC and the counter.
    fn read(&mut self, host: &mut impl DspHost, address: u16) -> u16 {
        match address {
            port::NOISE => self.noise(),
            port::AUDIO_OUT_STATUS => self.audio_out_status,
            port::SEMAPHORE_STATUS => self.semaphore_status,
            port::SEMAPHORE_DATA => self.semaphore_data,
            port::PC => self.pc as u16,
            port::COUNTER => self.counter as u16,
            // Input FIFOs, advancing. A CPU-supplied value is consumed but
            // what is read is the noise source.
            0xf0..=0xfc => {
                if self.take_cpu_value(address - 0xf0) {
                    self.noise()
                } else {
                    host.input_next(address & 0x0f)
                }
            }
            // Input FIFOs, peeking - or the value the CPU handed over.
            0x70..=0x7c => {
                if self.take_cpu_value(address - 0x70) {
                    self.data_word(address)
                } else {
                    host.input_peek(address & 0x0f)
                }
            }
            0xd0..=0xde => {
                if self.cpu_value_waiting(address & 0x0f) {
                    2
                } else {
                    host.input_status(address & 0x0f)
                }
            }
            0xe0..=0xe3 => host.output_status(address & 0x0f),
            // The DSPP's own bank; 0x200..=0x2FF mirrors it.
            0x100..=0x2ff => self.data_word(0x100 | (address & 0xff)),
            // Everything else reads the low (CPU-written) bank, mirrored.
            _ => self.data_word(address & 0x7f),
        }
    }

    /// The DSPP's write of its data space.
    fn write(&mut self, host: &mut impl DspHost, address: u16, value: u16) {
        let address = address & ADDRESS_MASK;
        match address {
            port::WRITE_AUDIO_OUT_STATUS => self.audio_out_status = value,
            port::WRITE_SEMAPHORE_ACK => self.semaphore_status |= SEMAPHORE_ACKNOWLEDGED,
            port::WRITE_SEMAPHORE_DATA => {
                self.semaphore_data = value;
                self.semaphore_status = SEMAPHORE_DSPP_WROTE;
            }
            // The audio interrupt is raised at the end of the pass: a program
            // may write it and keep running.
            port::WRITE_INTERRUPT => {
                self.interrupt_word = value;
                self.interrupt_pending = true;
            }
            port::WRITE_RELOAD => self.reload = value as i16,
            port::FLUSH_OUTPUT => {}
            port::LEFT_DAC | port::RIGHT_DAC => self.set_data_word(address, value),
            0x3f0..=0x3f3 => host.output(address & 0x0f, value),
            // The low half belongs to the CPU.
            0x000..=0x0ff => {}
            0x100..=0x2ff => self.set_data_word(0x100 | (address & 0xff), value),
            _ => self.set_data_word(address, value),
        }
    }

    /// The data address of register `number` under the current map and base.
    fn register(&self, number: u16) -> u16 {
        register_address(self.register_map, number) ^ self.register_base
    }

    /// Read an operand through `address`, once or (indirect) twice. Returns
    /// the effective address and the value found there.
    fn fetch(&mut self, host: &mut impl DspHost, address: u16, indirect: bool) -> (u16, u16) {
        let effective = if indirect {
            self.read(host, address)
        } else {
            address
        };
        (effective, self.read(host, effective))
    }

    /// Decode one operand word into the values it supplies, in order, with
    /// its destinations. The destination is kept with the packet rather than
    /// applied while decoding; the caller chooses which one wins.
    fn operand_packet(&mut self, host: &mut impl DspHost, word: u16) -> OperandPacket {
        let mut packet = OperandPacket::default();
        match OperandWord::decode(word) {
            OperandWord::ThreeRegisters { r1, r2, r3 } => {
                for reg in [r3, r2, r1] {
                    let address = self.register(reg.number);
                    let (_, value) = self.fetch(host, address, reg.indirect);
                    packet.push(value);
                    packet.destination = address;
                }
            }
            OperandWord::Address {
                address,
                indirect,
                write_back,
            } => {
                let (_, value) = self.fetch(host, address, indirect);
                packet.push(value);
                packet.destination = address;
                if write_back {
                    packet.marked = nonzero(address);
                }
            }
            OperandWord::TwoRegisters {
                r1,
                r1_write_back,
                r2,
            } => {
                let registers = r2.into_iter().chain(Some((r1, r1_write_back)));
                for (reg, write_back) in registers {
                    let address = self.register(reg.number);
                    let (effective, value) = self.fetch(host, address, reg.indirect);
                    packet.push(value);
                    packet.destination = effective;
                    if write_back {
                        packet.marked = nonzero(effective);
                    }
                }
            }
            OperandWord::Immediate(value) => {
                packet.push(value);
                packet.destination = value;
            }
        }
        packet
    }

    /// The single source of a MOVE. The packed register spellings name it in
    /// their last register field (R3 of three, R1 of two).
    fn move_source(&mut self, host: &mut impl DspHost) -> u16 {
        let word = self.next_program_word();
        let (address, indirect) = match OperandWord::decode(word) {
            OperandWord::ThreeRegisters { r3, .. } => (self.register(r3.number), r3.indirect),
            OperandWord::Address {
                address, indirect, ..
            } => (address, indirect),
            OperandWord::TwoRegisters { r1, .. } => (self.register(r1.number), r1.indirect),
            OperandWord::Immediate(value) => return value,
        };
        self.fetch(host, address, indirect).1
    }

    /// Read operand words until the instruction has what it asked for, fill
    /// the multiplier and ALU latches, and return the shift operand (if any)
    /// and the write-back address (if any).
    fn gather_operands(
        &mut self,
        host: &mut impl DspHost,
        requests: Requests,
        stated_count: u16,
    ) -> (Option<u16>, Option<u16>) {
        let wanted = match (stated_count, requests.0) {
            (0, 0) => return (None, None),
            (0, _) => 4,
            (n, _) => usize::from(n),
        };

        let mut values = [0u16; 8];
        let mut count = 0usize;
        let mut last_destination: u16;
        let mut marked: Option<u16> = None;
        loop {
            let word = self.next_program_word();
            let packet = self.operand_packet(host, word);
            for &value in packet.values() {
                if let Some(slot) = values.get_mut(count) {
                    *slot = value;
                }
                count += 1;
            }
            last_destination = packet.destination;
            if packet.marked.is_some() {
                marked = packet.marked;
            }
            if count >= wanted {
                break;
            }
        }

        // Slots are filled in a fixed order; the mask suppresses a slot
        // without changing which value goes to the next one.
        let requests = Requests(requests.0 & self.operand_mask as u8);
        let mut consumed = 0usize;
        let mut take = || {
            let value = values.get(consumed).copied().unwrap_or(0);
            consumed += 1;
            value
        };
        if requests.has(Requests::MULT1) {
            self.mult1 = take();
        }
        if requests.has(Requests::MULT2) {
            self.mult2 = take();
        }
        if requests.has(Requests::ALU1) {
            self.alu1 = take();
        }
        if requests.has(Requests::ALU2) {
            self.alu2 = take();
        }
        let shift = requests.has(Requests::SHIFT).then(take);

        // An explicit write-back bit wins. Otherwise a value left over (or
        // missing) makes the last packet's natural destination the target.
        let write_back = marked.or_else(|| {
            (count != consumed)
                .then_some(last_destination)
                .and_then(nonzero)
        });
        (shift, write_back)
    }

    fn execute_arithmetic(&mut self, host: &mut impl DspHost, op: Arithmetic, pass: &mut Pass) {
        let (shift_operand, write_back) =
            self.gather_operands(host, op.requests(), op.operand_count);
        let shift_code = shift_operand.map_or(op.shift_code(), |value| value as u8);

        let accumulator = pass.accumulator;
        let carry = pass.status.carry;
        let mult1 = i32::from(self.mult1 as i16);
        let mult2 = i32::from(self.mult2 as i16);
        let product = || mult1.wrapping_mul(mult2).wrapping_mul(2) as u32;
        let high = |word: u16| u32::from(word) << 16;

        let a = match op.mux_a {
            Input::Accumulator => accumulator,
            Input::Alu1 => high(self.alu1),
            Input::Alu2 => high(self.alu2),
            Input::Multiplier if op.m2sel => product(),
            // With the carry-taking operations the multiplier path passes
            // MULT1 through, gated by the carry.
            Input::Multiplier if op.alu.uses_carry() => {
                if carry {
                    high(self.mult1)
                } else {
                    0
                }
            }
            Input::Multiplier => mult1.wrapping_mul(((accumulator as i32) >> 15) & !1) as u32,
        };

        let b = if op.alu.uses_carry() {
            u32::from(carry) << 16
        } else {
            match op.mux_b {
                Input::Accumulator => accumulator,
                Input::Alu1 => high(self.alu1),
                Input::Alu2 => high(self.alu2),
                Input::Multiplier if op.m2sel => product(),
                Input::Multiplier => (mult1.wrapping_mul((accumulator as i32) >> 15) as u32) & !1,
            }
        };

        let result = evaluate_alu(op.alu, a, b);
        let y = result.value;
        pass.status = Status {
            carry: result.carry,
            overflow: result.overflow,
            zero: y & 0xffff_0000 == 0,
            negative: y & TOP != 0,
            exact: y & 0x0000_f000 == 0,
        };

        let y = barrel_shift(shift_code, y, &mut pass.status);
        pass.accumulator = y;
        if let Some(address) = write_back {
            self.write(host, address, (y >> 16) as u16);
        }
    }

    fn execute_control(&mut self, host: &mut impl DspHost, control: Control, pass: &mut Pass) {
        match control {
            Control::Nop | Control::Sleep => {}
            Control::BranchToAccumulator => self.pc = (pass.accumulator >> 16) & 0x3ff,
            Control::SetRegisterBase(base) => self.register_base = base,
            Control::SelectRegisterMap(map) => self.register_map = map,
            Control::Return => self.pc = pass.return_address,
            Control::SetOperandMask(mask) => self.operand_mask = mask,
            Control::Jump(target) => self.pc = u32::from(target),
            Control::JumpSubroutine(target) => {
                pass.return_address = self.pc;
                self.pc = u32::from(target);
            }
            Control::MoveToRegister { register, indirect } => {
                let value = self.move_source(host);
                let mut address = self.register(register);
                if indirect {
                    address = self.read(host, address);
                }
                self.write(host, address, value);
            }
            Control::MoveToAddress { address, indirect } => {
                let value = self.move_source(host);
                let address = if indirect {
                    self.read(host, address)
                } else {
                    address
                };
                self.write(host, address, value);
            }
            Control::Branch { condition, target } => {
                if condition.holds(pass.status) {
                    self.pc = u32::from(target);
                }
            }
        }
    }
}
