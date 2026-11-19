//! The matrix engine: 4x4 and 3x3 products of signed 16.16 values and the
//! projecting divide, as the SDK's `operamath.h` defines the operations and
//! WO 94/10641 describes the unit.
//!
//! The outputs are DOUBLE BUFFERED. An operation computes into a holding set,
//! and it is the PREVIOUS result that moves into the readable outputs. Software
//! reads the answer to the operation before the one it just asked for.
#![forbid(unsafe_code)]

/// 4x4 matrix, row-major, 16.16.
pub const MADAM_MATRIX_IN: u32 = 0x0600;
/// The vector, four words.
pub const MADAM_MATRIX_VEC: u32 = 0x0640;
/// The published results, four words.
pub const MADAM_MATRIX_OUT: u32 = 0x0660;
/// The 64-bit numerator of the projecting divide.
pub const MADAM_MATRIX_NUM_HI: u32 = 0x0680;
pub const MADAM_MATRIX_NUM_LO: u32 = 0x0684;
/// Writing here performs the operation the value names.
pub const MADAM_MATRIX_CTL: u32 = 0x07fc;

/// Publish the previous result and compute nothing.
pub const MATRIX_COPY_ONLY: u32 = 0;
pub const MATRIX_MULTIPLY_4X4: u32 = 1;
pub const MATRIX_MULTIPLY_3X3: u32 = 2;
/// Transform and project: x and y are divided through by z.
pub const MATRIX_MULTIPLY_3X3_DIVIDE_Z: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    CopyOnly,
    Multiply4x4,
    Multiply3x3,
    Project,
}

impl Operation {
    const fn decode(value: u32) -> Option<Self> {
        match value {
            MATRIX_COPY_ONLY => Some(Self::CopyOnly),
            MATRIX_MULTIPLY_4X4 => Some(Self::Multiply4x4),
            MATRIX_MULTIPLY_3X3 => Some(Self::Multiply3x3),
            MATRIX_MULTIPLY_3X3_DIVIDE_Z => Some(Self::Project),
            _ => None,
        }
    }
}

/// Which word of an array of 32-bit registers an offset names.
fn word_in(offset: u32, base: u32, words: u32) -> Option<usize> {
    let relative = offset.checked_sub(base)?;
    (relative < words * 4).then_some((relative / 4) as usize)
}

#[derive(Clone, Debug, Default)]
pub(super) struct MatrixEngine {
    matrix: [i32; 16],
    vector: [i32; 4],
    output: [i32; 4],
    pending: [i64; 4],
    numerator_hi: u32,
    numerator_lo: u32,
}

impl MatrixEngine {
    /// A register read, or `None` if the offset is not the matrix unit's.
    pub(super) fn read(&self, offset: u32) -> Option<u32> {
        if let Some(i) = word_in(offset, MADAM_MATRIX_IN, 16) {
            return self.matrix.get(i).map(|&v| v as u32);
        }
        if let Some(i) = word_in(offset, MADAM_MATRIX_VEC, 4) {
            return self.vector.get(i).map(|&v| v as u32);
        }
        if let Some(i) = word_in(offset, MADAM_MATRIX_OUT, 4) {
            return self.output.get(i).map(|&v| v as u32);
        }
        match offset {
            MADAM_MATRIX_NUM_HI => Some(self.numerator_hi),
            MADAM_MATRIX_NUM_LO => Some(self.numerator_lo),
            _ => None,
        }
    }

    /// A register write; `false` if the offset is not the matrix unit's.
    pub(super) fn write(&mut self, offset: u32, value: u32) -> bool {
        let slot = if let Some(i) = word_in(offset, MADAM_MATRIX_IN, 16) {
            self.matrix.get_mut(i)
        } else if let Some(i) = word_in(offset, MADAM_MATRIX_VEC, 4) {
            self.vector.get_mut(i)
        } else if let Some(i) = word_in(offset, MADAM_MATRIX_OUT, 4) {
            self.output.get_mut(i)
        } else {
            None
        };
        if let Some(slot) = slot {
            *slot = value as i32;
            return true;
        }
        match offset {
            MADAM_MATRIX_NUM_HI => self.numerator_hi = value,
            MADAM_MATRIX_NUM_LO => self.numerator_lo = value,
            MADAM_MATRIX_CTL => self.execute(value),
            _ => return false,
        }
        true
    }

    /// Row `row` dotted with the vector over `columns` columns, accumulated at
    /// full precision and shifted back to 16.16 once, after the sum.
    fn dot(&self, row: usize, columns: usize) -> i64 {
        let start = row * 4;
        let row = self.matrix.get(start..start + columns).unwrap_or(&[]);
        row.iter().zip(&self.vector).fold(0i64, |sum, (&m, &v)| {
            sum.wrapping_add(i64::from(m) * i64::from(v))
        }) >> 16
    }

    fn execute(&mut self, operation: u32) {
        // Publish the previous result first, whatever this operation is.
        for (out, &pending) in self.output.iter_mut().zip(&self.pending) {
            *out = pending as i32;
        }
        let Some(operation) = Operation::decode(operation) else {
            return;
        };
        let rows = match operation {
            Operation::CopyOnly => return,
            Operation::Multiply4x4 => 4,
            Operation::Multiply3x3 => 3,
            Operation::Project => {
                self.project();
                return;
            }
        };
        for row in 0..rows {
            let value = self.dot(row, rows);
            if let Some(slot) = self.pending.get_mut(row) {
                *slot = value;
            }
        }
    }

    /// Transform, then divide x and y by z. Software loads the numerator, so it
    /// sets the field of view. A vertex on the eye plane leaves the numerator
    /// undivided rather than trapping.
    fn project(&mut self) {
        let numerator = (u64::from(self.numerator_hi) << 32 | u64::from(self.numerator_lo)) as i64;
        let z = self.dot(2, 3);
        let scale = if z != 0 {
            numerator.wrapping_div(z)
        } else {
            numerator
        };
        let x = self.dot(0, 3).wrapping_mul(scale) >> 32;
        let y = self.dot(1, 3).wrapping_mul(scale) >> 32;
        self.pending = [x, y, z, self.pending.get(3).copied().unwrap_or(0)];
    }
}
