use serde::Serialize;
use std::sync::Mutex;
use std::sync::mpsc::{self, TrySendError};

/// Events emitted by the window manager for IPC subscribers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum WmEvent {
    WorkspaceChanged { old: String, new: String },
    WindowFocused { window_id: u32, app_name: String },
    WindowCreated { window_id: u32, app_name: String },
    WindowClosed { window_id: u32 },
    MonitorChanged { index: usize },
    LayoutChanged { workspace: String },
    ModeChanged { mode: String },
}

impl WmEvent {
    /// Returns the event type string for filtering.
    pub fn event_type(&self) -> &str {
        match self {
            WmEvent::WorkspaceChanged { .. } => "workspace_changed",
            WmEvent::WindowFocused { .. } => "window_focused",
            WmEvent::WindowCreated { .. } => "window_created",
            WmEvent::WindowClosed { .. } => "window_closed",
            WmEvent::MonitorChanged { .. } => "monitor_changed",
            WmEvent::LayoutChanged { .. } => "layout_changed",
            WmEvent::ModeChanged { .. } => "mode_changed",
        }
    }
}

/// Broadcast event bus for IPC subscribers.
/// Thread-safe — the main thread publishes, IPC threads subscribe.
#[derive(Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<mpsc::SyncSender<WmEvent>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new subscriber. Returns a receiver for events.
    /// Subscriber is automatically removed when the receiver is dropped
    /// (try_send fails on a disconnected channel).
    pub fn subscribe(&self) -> mpsc::Receiver<WmEvent> {
        let (tx, rx) = mpsc::sync_channel(256);
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Publish an event to all subscribers.
    /// Disconnected subscribers are automatically removed.
    pub fn publish(&self, event: WmEvent) {
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|tx| match tx.try_send(event.clone()) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{EventBus, WmEvent};
    use std::time::Duration;

    #[test]
    fn full_subscriber_is_not_dropped() {
        let bus = EventBus::new();
        let rx = bus.subscribe();

        for window_id in 0..256 {
            bus.publish(WmEvent::WindowClosed { window_id });
        }

        bus.publish(WmEvent::WindowClosed { window_id: 999 });

        for _ in 0..256 {
            rx.recv_timeout(Duration::from_millis(50))
                .expect("buffered event missing");
        }

        bus.publish(WmEvent::WindowClosed { window_id: 1000 });
        let event = rx
            .recv_timeout(Duration::from_millis(50))
            .expect("subscriber should still be registered");
        assert!(matches!(event, WmEvent::WindowClosed { window_id: 1000 }));
    }

    #[test]
    fn disconnected_subscriber_is_removed() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        drop(rx);

        bus.publish(WmEvent::WindowClosed { window_id: 1 });

        let mut subscribers = bus.subscribers.lock().unwrap();
        assert!(subscribers.is_empty());
        subscribers.clear();
    }
}
