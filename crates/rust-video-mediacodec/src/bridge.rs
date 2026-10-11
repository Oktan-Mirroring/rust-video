//! Async callback events drained through polling.

use std::sync::mpsc;
use std::time::Duration;

#[cfg(target_os = "android")]
use ndk::media::media_codec::AsyncNotifyCallback;

/// Codec occurrence awaiting a poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecEvent {
    /// Input buffer ready for one packet.
    InputAvailable {
        /// Buffer index.
        index: usize,
    },
    /// Output buffer holding one frame.
    OutputAvailable {
        /// Buffer index.
        index: usize,
        /// Valid range start.
        offset: i32,
        /// Valid range length.
        size: i32,
        /// Presentation time in microseconds.
        pts_us: i64,
        /// Buffer flags.
        flags: u32,
    },
    /// Output layout changed.
    FormatChanged,
    /// Codec fault.
    Error {
        /// Client may continue after reset.
        recoverable: bool,
        /// Fault may clear on retry.
        transient: bool,
        /// Native error kind.
        kind: String,
    },
}

/// Polled queue fed by codec callbacks.
#[derive(Debug)]
pub struct EventQueue {
    /// Callback-side event intake.
    sender: mpsc::Sender<CodecEvent>,
    /// Decoder-side event drain.
    receiver: mpsc::Receiver<CodecEvent>,
}

impl EventQueue {
    /// Empty queue.
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver }
    }

    /// Intake handle for callback threads.
    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<CodecEvent> {
        self.sender.clone()
    }

    /// Async callbacks pushing into this queue.
    ///
    /// All four slots stay set; unset slots fault on older devices.
    /// <https://docs.rs/ndk/0.9.0/ndk/media/media_codec/struct.MediaCodec.html#method.set_async_notify_callback>
    #[cfg(target_os = "android")]
    #[must_use]
    pub fn async_callbacks(&self) -> AsyncNotifyCallback {
        let input = self.sender();
        let output = self.sender();
        let format = self.sender();
        let error = self.sender();
        AsyncNotifyCallback {
            on_input_available: Some(Box::new(move |index| {
                let _sent = input.send(CodecEvent::InputAvailable { index });
            })),
            on_output_available: Some(Box::new(move |index, info| {
                let _sent = output.send(CodecEvent::OutputAvailable {
                    index,
                    offset: info.offset(),
                    size: info.size(),
                    pts_us: info.presentation_time_us(),
                    flags: info.flags(),
                });
            })),
            on_format_changed: Some(Box::new(move |_format| {
                let _sent = format.send(CodecEvent::FormatChanged);
            })),
            on_error: Some(Box::new(move |kind, action, _detail| {
                let _sent = error.send(CodecEvent::Error {
                    recoverable: action.is_recoverable(),
                    transient: action.is_transient(),
                    kind: format!("{kind:?}"),
                });
            })),
        }
    }

    /// Queued events in arrival order, without blocking.
    #[must_use]
    pub fn drain(&self) -> Vec<CodecEvent> {
        self.receiver.try_iter().collect()
    }

    /// Queued events, waiting up to `timeout` for the first.
    ///
    /// # Arguments
    ///
    /// * `timeout` - wait budget when the queue starts empty.
    #[must_use]
    pub fn drain_blocking(&self, timeout: Duration) -> Vec<CodecEvent> {
        let mut events = self.drain();
        if events.is_empty() {
            if let Ok(event) = self.receiver.recv_timeout(timeout) {
                events.push(event);
                events.extend(self.drain());
            }
        }
        events
    }
}

impl Default for EventQueue {
    /// Empty queue.
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CodecEvent, EventQueue};

    #[test]
    fn drain_returns_arrival_order() {
        let queue = EventQueue::new();
        let sender = queue.sender();
        sender
            .send(CodecEvent::InputAvailable { index: 2 })
            .expect("queue live");
        sender.send(CodecEvent::FormatChanged).expect("queue live");
        assert_eq!(
            queue.drain(),
            vec![
                CodecEvent::InputAvailable { index: 2 },
                CodecEvent::FormatChanged
            ]
        );
        assert!(queue.drain().is_empty());
    }

    #[test]
    fn drain_blocking_waits_for_first_event() {
        let queue = EventQueue::new();
        let sender = queue.sender();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            let _sent = sender.send(CodecEvent::FormatChanged);
        });
        assert_eq!(
            queue.drain_blocking(Duration::from_secs(5)),
            vec![CodecEvent::FormatChanged]
        );
    }

    #[test]
    fn drain_blocking_times_out_empty() {
        let queue = EventQueue::default();
        assert!(queue.drain_blocking(Duration::from_millis(1)).is_empty());
    }

    #[test]
    fn output_event_carries_range_and_timing() {
        let event = CodecEvent::OutputAvailable {
            index: 1,
            offset: 0,
            size: 64,
            pts_us: 33_366,
            flags: 0,
        };
        assert_eq!(
            event,
            CodecEvent::OutputAvailable {
                index: 1,
                offset: 0,
                size: 64,
                pts_us: 33_366,
                flags: 0,
            }
        );
    }
}
