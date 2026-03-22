use serde::Serialize;
use std::sync::Mutex;
use std::sync::mpsc;

/// Per-workspace summary included in workspace_changed events.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub active: bool,
    pub windows: usize,
    pub monitor: Option<usize>,
    pub urgent: bool,
}

/// Events emitted by the window manager for IPC subscribers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum WmEvent {
    #[serde(rename = "workspace_changed")]
    WorkspaceChanged {
        old: String,
        new: String,
        workspaces: Vec<WorkspaceInfo>,
        focused_workspace: String,
        focused_monitor: usize,
    },
    #[serde(rename = "window_focused")]
    WindowFocused {
        window_id: u32,
        title: String,
        app_name: String,
        app_bundle: String,
        workspace: String,
    },
    #[serde(rename = "window_created")]
    WindowCreated {
        window_id: u32,
        title: String,
        app_name: String,
        app_bundle: String,
        workspace: String,
    },
    #[serde(rename = "window_closed")]
    WindowClosed {
        window_id: u32,
        app_name: String,
    },
    #[serde(rename = "monitor_changed")]
    MonitorChanged {
        index: usize,
        monitor_count: usize,
        focused_workspace: String,
    },
    #[serde(rename = "layout_changed")]
    LayoutChanged {
        workspace: String,
        window_count: usize,
        layout_type: String,
    },
    #[serde(rename = "mode_changed")]
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
        subs.retain(|tx| tx.try_send(event.clone()).is_ok());
    }
}
