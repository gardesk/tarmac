use std::collections::HashSet;

use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

/// Callback types for app lifecycle events.
pub type AppLaunchCallback = Box<dyn Fn(i32, String, String)>; // pid, name, bundle_id
pub type AppTerminateCallback = Box<dyn Fn(i32)>; // pid

/// Polls NSWorkspace.runningApplications to detect app launches and terminations.
/// Called periodically from a CFRunLoop timer on the main thread.
pub struct WorkspacePollingObserver {
    known_pids: HashSet<i32>,
    on_launch: AppLaunchCallback,
    on_terminate: AppTerminateCallback,
}

impl WorkspacePollingObserver {
    pub fn new(on_launch: AppLaunchCallback, on_terminate: AppTerminateCallback) -> Self {
        let mut known_pids = HashSet::new();
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
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();
        let count = apps.count();

        let mut current_pids = HashSet::with_capacity(count);

        for i in 0..count {
            let app = apps.objectAtIndex(i);
            if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
                continue;
            }

            let pid = app.processIdentifier();
            current_pids.insert(pid);

            if !self.known_pids.contains(&pid) {
                let name = app
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                let bundle = app
                    .bundleIdentifier()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                tracing::info!(pid, app = %name, bundle = %bundle, "app launched");
                (self.on_launch)(pid, name, bundle);
            }
        }

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
