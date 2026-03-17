use std::collections::{HashMap, HashSet};

use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

use super::application::get_cg_window_list;

/// Callback for new windows detected via CGWindowList. Args: pid, owner_name, window_id
pub type NewWindowCallback = Box<dyn Fn(i32, String, u32)>;
/// Callback for windows that disappeared from screen. Args: window_id, pid
pub type WindowClosedCallback = Box<dyn Fn(u32, i32)>;
/// Callback for app termination. Args: pid
pub type AppTerminateCallback = Box<dyn Fn(i32)>;

/// Polls CGWindowList to detect new windows appearing and apps terminating.
/// This is the ground truth — if a window is on screen, CGWindowList sees it.
pub struct WorkspacePollingObserver {
    /// wid → pid for all known on-screen windows
    known_windows: HashMap<u32, i32>,
    /// All known PIDs (windows + NSWorkspace)
    known_pids: HashSet<i32>,
    on_new_window: NewWindowCallback,
    on_window_closed: WindowClosedCallback,
    on_terminate: AppTerminateCallback,
}

impl WorkspacePollingObserver {
    pub fn new(
        on_new_window: NewWindowCallback,
        on_window_closed: WindowClosedCallback,
        on_terminate: AppTerminateCallback,
    ) -> Self {
        let mut known_windows = HashMap::new();
        let mut known_pids = HashSet::new();

        for cg in get_cg_window_list() {
            known_windows.insert(cg.wid, cg.pid);
            known_pids.insert(cg.pid);
        }

        // Also seed PIDs from NSWorkspace
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();
        let count = apps.count();
        for i in 0..count {
            let app = apps.objectAtIndex(i);
            if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                known_pids.insert(app.processIdentifier());
            }
        }

        tracing::debug!(
            windows = known_windows.len(),
            pids = known_pids.len(),
            "workspace polling initialized"
        );

        Self {
            known_windows,
            known_pids,
            on_new_window,
            on_window_closed,
            on_terminate,
        }
    }

    /// Detect new windows and terminated apps.
    pub fn poll(&mut self) {
        let cg_windows = get_cg_window_list();

        let mut current_windows: HashMap<u32, i32> = HashMap::new();
        let mut current_pids = HashSet::new();

        for cg in &cg_windows {
            current_windows.insert(cg.wid, cg.pid);
            current_pids.insert(cg.pid);

            // New window we haven't seen?
            if !self.known_windows.contains_key(&cg.wid) {
                tracing::info!(wid = cg.wid, pid = cg.pid, owner = %cg.owner, "new window detected");
                (self.on_new_window)(cg.pid, cg.owner.clone(), cg.wid);
            }
        }

        // Detect windows that disappeared
        for (wid, pid) in &self.known_windows {
            if !current_windows.contains_key(wid) {
                tracing::info!(wid, pid, "window disappeared");
                (self.on_window_closed)(*wid, *pid);
            }
        }

        // Also check NSWorkspace for PIDs (some apps don't have on-screen windows yet)
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();
        let count = apps.count();
        for i in 0..count {
            let app = apps.objectAtIndex(i);
            if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                current_pids.insert(app.processIdentifier());
            }
        }

        // Detect terminated PIDs
        let terminated: Vec<i32> = self
            .known_pids
            .iter()
            .filter(|pid| !current_pids.contains(pid))
            .copied()
            .collect();

        for pid in &terminated {
            tracing::info!(pid, "app terminated");
            (self.on_terminate)(*pid);
        }

        self.known_windows = current_windows;
        self.known_pids = current_pids;
    }
}
