//! Pre-allocated output event buffer.
//!
//! `OrderBook::apply` is forbidden to allocate, so the caller hands it a
//! buffer sized once at startup. Worst case for a single input event is
//! bounded: every live order can contribute at most one event (a fill or
//! an STP cancel), plus the taker's own Ack / Cancelled / Rejected — so
//! `max_live_orders + 8` can never overflow.

use crate::config::BookConfig;
use crate::events::OutputEvent;
use alloc::vec::Vec;

pub struct EventBuffer {
    buf: Vec<OutputEvent>,
}

impl EventBuffer {
    pub fn with_capacity(n: usize) -> Self {
        EventBuffer {
            buf: Vec::with_capacity(n),
        }
    }

    /// A buffer guaranteed large enough for any single `apply` on a book
    /// with this config.
    pub fn for_book(cfg: &BookConfig) -> Self {
        Self::with_capacity(cfg.max_live_orders as usize + 8)
    }

    /// Never allocates: within capacity this is a plain write + length
    /// bump. Overflow is impossible when the buffer was sized by
    /// [`EventBuffer::for_book`]; a mis-sized buffer drops the event and
    /// trips a debug assertion rather than allocating or panicking in
    /// release.
    #[inline(always)]
    pub fn push(&mut self, ev: OutputEvent) {
        if self.buf.len() < self.buf.capacity() {
            self.buf.push(ev);
        } else {
            debug_assert!(false, "EventBuffer overflow — sized too small");
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[OutputEvent] {
        &self.buf
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}
