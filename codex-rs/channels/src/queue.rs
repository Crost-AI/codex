use std::collections::VecDeque;

/// Default number of pending channel events kept while a turn is running.
pub const DEFAULT_CHANNEL_QUEUE_CAPACITY: usize = 64;

/// A bounded FIFO of rendered channel events awaiting delivery. When full,
/// the oldest event is dropped so the queue always keeps the freshest events.
#[derive(Debug)]
pub struct BoundedEventQueue {
    items: VecDeque<String>,
    capacity: usize,
}

impl Default for BoundedEventQueue {
    fn default() -> Self {
        Self::new(DEFAULT_CHANNEL_QUEUE_CAPACITY)
    }
}

impl BoundedEventQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Enqueues an event. Returns the evicted oldest event when the queue was
    /// full so callers can log a warning.
    pub fn push(&mut self, event: String) -> Option<String> {
        let dropped = if self.items.len() >= self.capacity {
            self.items.pop_front()
        } else {
            None
        };
        self.items.push_back(event);
        dropped
    }

    pub fn drain_all(&mut self) -> Vec<String> {
        self.items.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
