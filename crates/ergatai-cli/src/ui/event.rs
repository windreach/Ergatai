//! Merged event sources: crossterm terminal events + ACP session events.
//!
//! The runner consumes a single stream of [`Event`] values so that the main
//! loop can `select!` over user input and agent output uniformly.

use std::time::Duration;

use crossterm::event::{self as ct, Event as CtEvent};
use tokio::sync::mpsc;
use tokio::time;

use ergatai_core::acp::manager::{poll_events, NapiSessionEvent};

/// A unified event delivered to the runner's main loop.
pub enum Event {
    /// A crossterm terminal event (key, resize, mouse…).
    Term(CtEvent),
    /// An ACP session event (agent message chunk, tool call, closed…).
    Acp(NapiSessionEvent),
    /// A periodic tick — gives the runner a chance to drain ACP events
    /// even when no terminal events arrive.
    Tick,
}

/// Spawn a background task that converts crossterm events into
/// `Event::Term` values sent over the returned receiver.
pub fn spawn_crossterm_pump() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();

    std::thread::spawn(move || {
        // Poll in a tight blocking loop on a dedicated OS thread. Crossterm's
        // `poll()` is blocking, so we can't call it directly from tokio.
        loop {
            match ct::poll(Duration::from_millis(50)) {
                Ok(true) => match ct::read() {
                    Ok(ev) => {
                        if tx.send(Event::Term(ev)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => {
                    // Timeout — emit a tick so the runner can drain ACP events.
                    if tx.send(Event::Tick).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });

    rx
}

/// Drain all pending ACP events from the global event bus and forward them
/// into `tx`. Called by the runner on every tick / terminal event.
pub fn drain_acp_events(tx: &mpsc::UnboundedSender<Event>) {
    let events = poll_events();
    for ev in events {
        if tx.send(Event::Acp(ev)).is_err() {
            return;
        }
    }
}

/// Build a ticker that fires every 50ms. The runner uses this to guarantee
/// periodic ACP draining even if crossterm events are sparse.
#[allow(dead_code)]
pub fn tick_interval() -> time::Interval {
    time::interval(Duration::from_millis(50))
}
