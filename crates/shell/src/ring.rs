//! SPSC ring between the I/O side and the engine thread, LMAX-style:
//! the decoder produces `InputEvent`s, the pinned engine thread consumes
//! them, and a second ring carries `OutputEvent`s back out.
//!
//! Built on `crossbeam-queue`'s pre-allocated `ArrayQueue` rather than a
//! hand-rolled unsafe ring: lock-free correctness without loom-verified
//! unsafe code of our own. Concurrency lives HERE, in the shell — the
//! engine core stays single-threaded and never sees a thread or a lock.

use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Producer<T> {
    q: Arc<ArrayQueue<T>>,
    closed: Arc<AtomicBool>,
}

pub struct Consumer<T> {
    q: Arc<ArrayQueue<T>>,
    closed: Arc<AtomicBool>,
}

/// A bounded SPSC channel of the given capacity (pre-allocated once).
pub fn ring<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let q = Arc::new(ArrayQueue::new(capacity));
    let closed = Arc::new(AtomicBool::new(false));
    (
        Producer {
            q: q.clone(),
            closed: closed.clone(),
        },
        Consumer { q, closed },
    )
}

impl<T> Producer<T> {
    /// Busy-spin until the slot frees (backpressure): the ring is sized
    /// so this is the rare case, and the engine drains fast.
    pub fn send(&self, mut v: T) {
        loop {
            match self.q.push(v) {
                Ok(()) => return,
                Err(back) => {
                    v = back;
                    std::hint::spin_loop();
                }
            }
        }
    }

    pub fn try_send(&self, v: T) -> Result<(), T> {
        self.q.push(v)
    }
}

impl<T> Drop for Producer<T> {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

impl<T> Consumer<T> {
    pub fn try_recv(&self) -> Option<T> {
        self.q.pop()
    }

    /// Spin until an item arrives or the producer hangs up.
    pub fn recv(&self) -> Option<T> {
        loop {
            if let Some(v) = self.q.pop() {
                return Some(v);
            }
            if self.closed.load(Ordering::Acquire) {
                // Drain anything that raced the close flag.
                return self.q.pop();
            }
            std::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pipe a deterministic sim log through the ring on a real thread and
    /// into the engine: same final state hash as applying it inline.
    #[test]
    fn threaded_pipeline_matches_inline() {
        let sc = tessera_sim::SimConfig {
            seed: 11,
            events: 100_000,
            ..Default::default()
        };
        let sim = tessera_sim::run(sc);
        let log = sim.log;

        let (tx, rx) = ring::<tessera_core::InputEvent>(1024);
        let feeder = {
            let log = log.clone();
            std::thread::spawn(move || {
                for ev in log {
                    tx.send(ev);
                }
            })
        };

        let mut book = tessera_core::OrderBook::new(sc.book);
        let mut buf = tessera_core::EventBuffer::for_book(&sc.book);
        while let Some(ev) = rx.recv() {
            buf.clear();
            book.apply(ev, &mut buf);
        }
        feeder.join().unwrap();
        assert_eq!(book.state_hash(), sim.final_state_hash);
    }
}
