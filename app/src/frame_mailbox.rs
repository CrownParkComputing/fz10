//! Finished frames, from the emulator to whoever draws them.
//!
//! A triple buffer: one slot being filled, one holding the newest finished
//! frame, one being read. Publishing is a single atomic exchange with the
//! "fresh" flag packed into the same word as the slot index, so a consumer can
//! never see an index that does not yet name a complete frame.
//!
//! Not a queue: a display that is behind wants the newest frame, not the oldest
//! unread one, and a queue is latency that never recovers. Not one shared lock:
//! the producer and the consumer each only ever touch a slot the other cannot
//! hold, so each slot's lock is uncontended by construction and neither side
//! ever waits for the other.
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

const SLOTS: usize = 3;
const FRESH: u32 = 0x8000_0000;
const OPAQUE_BLACK: u32 = 0xff00_0000;

pub struct FrameMailbox {
    slots: [Mutex<Vec<u32>>; SLOTS],
    width: AtomicUsize,
    height: AtomicUsize,
    /// Owned by the producer.
    writing: AtomicUsize,
    /// Owned by the consumer.
    reading: AtomicUsize,
    /// The newest finished slot, with `FRESH` set until a consumer takes it.
    ready: AtomicU32,
    published: AtomicU64,
    consumed: AtomicU64,
    /// The slots a resize replaced, kept alive until the next resize: a C
    /// consumer may still hold a pointer into one (see [`Self::acquire_raw`]).
    retired: Mutex<Vec<Vec<u32>>>,
}

impl Default for FrameMailbox {
    fn default() -> Self {
        Self::new()
    }
}

fn lock(slot: &Mutex<Vec<u32>>) -> MutexGuard<'_, Vec<u32>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FrameMailbox {
    pub fn new() -> Self {
        let mailbox = Self {
            slots: Default::default(),
            width: AtomicUsize::new(0),
            height: AtomicUsize::new(0),
            writing: AtomicUsize::new(0),
            reading: AtomicUsize::new(2),
            ready: AtomicU32::new(1),
            published: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            retired: Mutex::new(Vec::new()),
        };
        mailbox.resize(320, 240);
        mailbox
    }

    /// Reshape every slot, seeding it with opaque black so nobody ever sees
    /// uninitialised pixels.
    ///
    /// The producer may do this while a consumer runs (the frame size changes
    /// with the internal scale): every slot is swapped for a new buffer under
    /// its lock, and the old buffers are retired rather than freed, so a
    /// pointer from [`Self::acquire_raw`] stays readable until the consumer's
    /// next frame call. A consumer that reads the size separately must check it
    /// against the slot's length ([`Self::acquire_raw_sized`]): for one frame
    /// the two can disagree.
    pub fn resize(&self, width: usize, height: usize) {
        if width == 0 || height == 0 {
            return;
        }
        let pixels = width.saturating_mul(height);
        let mut retired = self.retired.lock().unwrap_or_else(PoisonError::into_inner);
        retired.clear();
        for slot in &self.slots {
            let mut slot = lock(slot);
            retired.push(std::mem::replace(&mut *slot, vec![OPAQUE_BLACK; pixels]));
        }
        self.width.store(width, Ordering::Release);
        self.height.store(height, Ordering::Release);
        self.writing.store(0, Ordering::Relaxed);
        self.reading.store(2, Ordering::Relaxed);
        self.ready.store(1, Ordering::Release);
    }

    pub fn width(&self) -> usize {
        self.width.load(Ordering::Acquire)
    }
    pub fn height(&self) -> usize {
        self.height.load(Ordering::Acquire)
    }

    fn slot(&self, index: usize) -> Option<&Mutex<Vec<u32>>> {
        self.slots.get(index)
    }

    // --- producer -------------------------------------------------------

    /// Fill the slot being written. Nothing is visible until [`publish`].
    ///
    /// [`publish`]: Self::publish
    pub fn write_with<R>(&self, fill: impl FnOnce(&mut [u32]) -> R) -> Option<R> {
        let slot = self.slot(self.writing.load(Ordering::Relaxed))?;
        Some(fill(&mut lock(slot)))
    }

    /// Hand the written slot over and take the previous ready one to write next.
    pub fn publish(&self) {
        let writing = self.writing.load(Ordering::Relaxed);
        let previous = self.ready.swap(writing as u32 | FRESH, Ordering::AcqRel);
        self.writing
            .store((previous & !FRESH) as usize, Ordering::Relaxed);
        self.published.fetch_add(1, Ordering::Relaxed);
    }

    /// Copy a finished frame in and publish it in one step, reshaping first if
    /// its size changed.
    pub fn publish_frame(&self, pixels: &[u32], width: usize, height: usize) {
        if width != self.width() || height != self.height() {
            self.resize(width, height);
        }
        self.write_with(|slot| {
            let count = slot.len().min(pixels.len());
            if let (Some(to), Some(from)) = (slot.get_mut(..count), pixels.get(..count)) {
                to.copy_from_slice(from);
            }
        });
        self.publish();
    }

    // --- consumer -------------------------------------------------------

    /// Take the newest finished frame if one arrived since the last call.
    /// Returns false, without waiting, when nothing is new.
    fn take_fresh(&self) -> bool {
        if self.ready.load(Ordering::Acquire) & FRESH == 0 {
            return false;
        }
        let reading = self.reading.load(Ordering::Relaxed);
        let previous = self.ready.swap(reading as u32, Ordering::AcqRel);
        self.reading
            .store((previous & !FRESH) as usize, Ordering::Relaxed);
        self.consumed.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Look at the newest frame if it is new. `None` means nothing arrived,
    /// which is the signal to skip a texture upload.
    pub fn acquire_with<R>(&self, read: impl FnOnce(&[u32]) -> R) -> Option<R> {
        if !self.take_fresh() {
            return None;
        }
        self.current_with(read)
    }

    /// Look at the frame last acquired, new or not.
    pub fn current_with<R>(&self, read: impl FnOnce(&[u32]) -> R) -> Option<R> {
        let slot = self.slot(self.reading.load(Ordering::Relaxed))?;
        Some(read(&lock(slot)))
    }

    /// For the C ABI: acquire, and hand back where the pixels are. The address
    /// stays valid until the consumer's next acquire, because the producer never
    /// touches the slot being read; only `resize` (which needs both sides
    /// stopped) moves it.
    pub fn acquire_raw(&self) -> Option<*const u32> {
        if !self.take_fresh() {
            return None;
        }
        self.current_raw()
    }

    pub fn current_raw(&self) -> Option<*const u32> {
        self.current_with(<[u32]>::as_ptr)
    }

    /// [`Self::acquire_raw`] with the slot's length in pixels.
    pub fn acquire_raw_sized(&self) -> Option<(*const u32, usize)> {
        if !self.take_fresh() {
            return None;
        }
        self.current_raw_sized()
    }

    /// [`Self::current_raw`] with the slot's length in pixels.
    pub fn current_raw_sized(&self) -> Option<(*const u32, usize)> {
        self.current_with(|pixels| (pixels.as_ptr(), pixels.len()))
    }

    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }
    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::Relaxed)
    }
}
