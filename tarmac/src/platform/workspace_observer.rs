use std::collections::HashSet;

use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

use super::application::get_cg_window_list;

/// Callback types for app lifecycle events.
pub type AppLaunchCallback = Box<dyn Fn(i32, String, String)>; // pid, name, bundle_id
pub type AppTerminateCallback = Box<dyn Fn(i32)>; // pid

/// Polls both CGWindowList and NSWorkspace to detect app launches and terminations.
/// CGWindowList catches non-bundled apps (wezterm-gui, etc.) that NSWorkspace misses.
/// Called periodically from a CFRunLoop timer on the main thread.
pub struct WorkspacePollingObserver {
    known_pids: HashSet<i32>,
    on_launch: AppLaunchCallback,
    on_terminate: AppTerminateCallback,
}

impl WorkspacePollingObserver {
    pub fn new(on_launch: AppLaunchCallback, on_terminate: AppTerminateCallback) -> Self {
        // Seed with PIDs from both sources
        let mut known_pids = HashSet::new();

        // CGWindowList: all on-screen window owners
        for cg in get_cg_window_list() {
            known_pids.insert(cg.pid);
        }

        // NSWorkspace: all Regular apps (may include apps with no on-screen windows)
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();
        let count = apps.count();
        for i in 0..count {
            let app = apps.objectAtIndex(i);
            if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                known_pids.insert(app.processIdentifier());
            }
        }

        tracing::debug!(known = known_pids.len(), "workspace polling initialized");

        Self {
            known_pids,
            on_launch,
            on_terminate,
        }
    }

    /// Call periodically to detect app launches and terminations.
    pub fn poll(&mut self) {
        let mut current_pids = HashSet::new();

        // Source 1: CGWindowList — catches non-bundled apps
        let cg_windows = get_cg_window_list();
        for cg in &cg_windows {
            if !current_pids.contains(&cg.pid) && !self.known_pids.contains(&cg.pid) {
                tracing::info!(pid = cg.pid, app = %cg.owner, "app launched (CG)");
                (self.on_launch)(cg.pid, cg.owner.clone(), String::new());
            }
            current_pids.insert(cg.pid);
        }

        // Source 2: NSWorkspace — catches bundled apps, gives bundle IDs
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();
        let count = apps.count();
        for i in 0..count {
            let app = apps.objectAtIndex(i);
            if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
                continue;
            }

            let pid = app.processIdentifier();
            if !current_pids.contains(&pid) && !self.known_pids.contains(&pid) {
                let name = app
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                let bundle = app
                    .bundleIdentifier()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                tracing::info!(pid, app = %name, bundle = %bundle, "app launched (NS)");
                (self.on_launch)(pid, name, bundle);
            }
            current_pids.insert(pid);
        }

        // Detect terminated apps: PIDs we knew about that are no longer in either source
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

        self.known_pids = current_pids;
    }
}
