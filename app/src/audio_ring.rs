//! Audio from the emulator to the platform's audio device.
//!
//! Single producer (the emulator thread), single consumer (the audio
//! callback), lock-free and allocation-free after construction. Each stereo
//! pair is one atomic word, so the ring is safe Rust with no locks.
//!
//! Two policies are deliberate:
//! - an underrun yields silence, not a repeated sample (that buzzes, and sounds
//!   like a broken emulator rather than a dropped frame);
//! - a full ring refuses the excess rather than blocking: the emulator must
//!   never wait on the audio thread, which on Android and iOS would stall the
//!   device's whole audio pipeline.
//!
//! `refused()` counts samples the ring had no room for. A producer that retries
//! loses nothing even though the count rises, so it measures back-pressure; the
//! emulator never retries, so for it the two coincide.
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const SAMPLE_RATE: u32 = 44_100;

/// One left/right pair of signed 16-bit samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StereoSample {
    pub left: i16,
    pub right: i16,
}

impl StereoSample {
    pub const SILENCE: Self = Self { left: 0, right: 0 };

    pub fn new(left: i16, right: i16) -> Self {
        Self { left, right }
    }

    fn pack(self) -> u32 {
        (u32::from(self.right as u16) << 16) | u32::from(self.left as u16)
    }

    fn unpack(word: u32) -> Self {
        Self {
            left: word as u16 as i16,
            right: (word >> 16) as u16 as i16,
        }
    }
}

pub struct AudioRing {
    buffer: Box<[AtomicU32]>,
    write: AtomicU32,
    read: AtomicU32,
    underruns: AtomicU64,
    refused: AtomicU64,
}

impl Default for AudioRing {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioRing {
    pub const CAPACITY: u32 = 32_768;
    const MASK: u32 = Self::CAPACITY - 1;

    pub fn new() -> Self {
        Self {
            buffer: (0..Self::CAPACITY).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicU32::new(0),
            read: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        }
    }

    /// Empty the ring and zero its counters. Only while nothing else uses it.
    pub fn reset(&self) {
        self.write.store(0, Ordering::Relaxed);
        self.read.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
        self.refused.store(0, Ordering::Relaxed);
    }

    fn slot(&self, index: u32) -> Option<&AtomicU32> {
        self.buffer.get((index & Self::MASK) as usize)
    }

    /// Pairs waiting to be pulled. The indices wrap; the difference is exact.
    pub fn available(&self) -> u32 {
        let write = self.write.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        write.wrapping_sub(read)
    }

    pub fn is_empty(&self) -> bool {
        self.available() == 0
    }

    /// Producer side. Returns how many were taken; the rest are refused.
    pub fn push(&self, samples: &[StereoSample]) -> u32 {
        let count = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        if count == 0 {
            return 0;
        }
        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        let space = Self::CAPACITY.saturating_sub(write.wrapping_sub(read));
        let writable = count.min(space);
        for (offset, sample) in (0..writable).zip(samples) {
            if let Some(slot) = self.slot(write.wrapping_add(offset)) {
                slot.store(sample.pack(), Ordering::Relaxed);
            }
        }
        self.write
            .store(write.wrapping_add(writable), Ordering::Release);
        if writable < count {
            self.refused
                .fetch_add(u64::from(count - writable), Ordering::Relaxed);
        }
        writable
    }

    /// Consumer side. Always fills all of `out`; any shortfall is silence, and
    /// the return value is how many pairs were real.
    pub fn pull(&self, out: &mut [StereoSample]) -> u32 {
        self.take(out.iter_mut(), |slot, sample| *slot = sample)
    }

    /// Pull straight into an interleaved left, right, left, right buffer, which
    /// is the layout the C ABI hands over. No staging copy, no allocation.
    pub fn pull_interleaved(&self, out: &mut [i16]) -> u32 {
        let (pairs, _) = out.as_chunks_mut::<2>();
        self.take(pairs.iter_mut(), |pair, sample| {
            *pair = [sample.left, sample.right]
        })
    }

    /// The consumer's one algorithm: real pairs first, then silence, and one
    /// underrun counted if the request could not be met in full.
    fn take<T>(
        &self,
        slots: impl ExactSizeIterator<Item = T>,
        mut put: impl FnMut(T, StereoSample),
    ) -> u32 {
        let wanted = u32::try_from(slots.len()).unwrap_or(u32::MAX);
        if wanted == 0 {
            return 0;
        }
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        let readable = wanted.min(write.wrapping_sub(read));
        for (offset, slot) in (0u32..).zip(slots) {
            let sample = if offset < readable {
                self.slot(read.wrapping_add(offset))
                    .map_or(StereoSample::SILENCE, |cell| {
                        StereoSample::unpack(cell.load(Ordering::Relaxed))
                    })
            } else {
                StereoSample::SILENCE
            };
            put(slot, sample);
        }
        self.read
            .store(read.wrapping_add(readable), Ordering::Release);
        if readable < wanted {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
        readable
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}
