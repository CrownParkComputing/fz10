//! The frame mailbox: a triple buffer between the emulator and the display.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use retro3do::frame_mailbox::FrameMailbox;

fn publish(mailbox: &FrameMailbox, value: u32) {
    mailbox.write_with(|pixels| pixels.fill(value)).unwrap();
    mailbox.publish();
}

fn uniform(pixels: &[u32]) -> Option<u32> {
    let first = *pixels.first()?;
    pixels.iter().all(|&p| p == first).then_some(first)
}

#[test]
fn a_published_frame_comes_back_to_the_consumer() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(4, 4);
    publish(&mailbox, 0xAAAA_AAAA);
    let (first, last) = mailbox.acquire_with(|f| (f[0], f[15])).expect("a frame");
    assert_eq!(first, 0xAAAA_AAAA);
    assert_eq!(last, 0xAAAA_AAAA);
}

#[test]
fn acquiring_twice_reports_that_nothing_is_new() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(4, 4);
    publish(&mailbox, 1);
    assert!(mailbox.acquire_with(|_| ()).is_some());
    assert!(mailbox.acquire_with(|_| ()).is_none());
    assert!(mailbox.acquire_with(|_| ()).is_none());
}

#[test]
fn the_most_recent_frame_is_still_readable_when_nothing_is_new() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(4, 4);
    publish(&mailbox, 0x1234);
    assert!(mailbox.acquire_with(|_| ()).is_some());
    assert!(mailbox.acquire_with(|_| ()).is_none());
    assert_eq!(mailbox.current_with(|f| f[0]), Some(0x1234));
}

#[test]
fn a_slow_consumer_skips_to_the_newest_frame_rather_than_a_backlog() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(4, 4);
    for value in 1..=5 {
        publish(&mailbox, value);
    }
    assert_eq!(mailbox.acquire_with(|f| f[0]), Some(5));
}

#[test]
fn the_producer_never_writes_the_slot_the_consumer_is_reading() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(4, 4);
    publish(&mailbox, 100);
    let held = mailbox.acquire_raw().expect("a frame");
    assert_eq!(mailbox.current_with(|f| f[0]), Some(100));

    publish(&mailbox, 200);
    publish(&mailbox, 300);

    // The slot the consumer holds is untouched: same address, same pixels.
    assert_eq!(mailbox.current_raw(), Some(held));
    assert_eq!(mailbox.current_with(|f| (f[0], f[15])), Some((100, 100)));
}

#[test]
fn resizing_reshapes_every_slot() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(320, 240);
    assert_eq!(mailbox.width(), 320);
    assert_eq!(mailbox.height(), 240);

    mailbox.resize(320, 288);
    assert_eq!(mailbox.height(), 288);
    for value in [7, 8, 9] {
        publish(&mailbox, value);
    }
    assert_eq!(mailbox.acquire_with(|f| f[320 * 288 - 1]), Some(9));
}

#[test]
fn a_frame_is_never_seen_half_written() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(64, 64);
    let stop = AtomicBool::new(false);
    let torn = AtomicUsize::new(0);
    let mut seen = 0;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut value = 1u32;
            while !stop.load(Ordering::Relaxed) {
                publish(&mailbox, value);
                value += 1;
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while seen < 200 && Instant::now() < deadline {
            match mailbox.acquire_with(|f| uniform(f).is_some()) {
                Some(whole) => {
                    if !whole {
                        torn.fetch_add(1, Ordering::Relaxed);
                    }
                    seen += 1;
                }
                None => std::thread::yield_now(),
            }
        }
        stop.store(true, Ordering::Relaxed);
    });
    assert_eq!(torn.load(Ordering::Relaxed), 0);
    assert_eq!(seen, 200);
}

#[test]
fn frames_only_ever_go_forwards() {
    let mailbox = FrameMailbox::new();
    mailbox.resize(16, 16);
    let stop = AtomicBool::new(false);
    let mut went_backwards = false;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut value = 1u32;
            while !stop.load(Ordering::Relaxed) {
                publish(&mailbox, value);
                value += 1;
            }
        });
        let mut previous = 0;
        for _ in 0..20_000 {
            if let Some(value) = mailbox.acquire_with(|f| f[0]) {
                if value < previous {
                    went_backwards = true;
                    break;
                }
                previous = value;
            }
        }
        stop.store(true, Ordering::Relaxed);
    });
    assert!(!went_backwards);
}
