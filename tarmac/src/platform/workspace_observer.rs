use std::collections::{HashMap, HashSet};

use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

use super::application::get_cg_window_list;

/// Callback for new windows detected via CGWindowList. Args: pid, owner_name, window_id
pub type NewWindowCallback = Box<dyn Fn(i32, String, u32)>;
/// Callback for windows that disappeared from screen. Args: window_id, pid
pub type WindowClosedCallback = Box<dyn Fn(u32, i32)>;
/// Callback for app termination. Args: pid
pub type AppTerminateCallback = Box<dyn Fn(i32)>;
/// Callback for frontmost regular app changes. Args: pid
pub type FrontmostAppCallback = Box<dyn Fn(i32)>;
/// Callback to check if a window is intentionally hidden (on inactive workspace).
pub type IsHiddenCallback = Box<dyn Fn(u32) -> bool>;

/// Polls CGWindowList to detect new windows appearing and apps terminating.
/// This is the ground truth — if a window is on screen, CGWindowList sees it.
pub struct WorkspacePollingObserver {
    /// wid → pid for all known on-screen windows
    known_windows: HashMap<u32, i32>,
    /// All known PIDs (windows + NSWorkspace)
    known_pids: HashSet<i32>,
    /// PID of the last observed frontmost regular app.
    known_frontmost_pid: Option<i32>,
    on_new_window: NewWindowCallback,
    on_window_closed: WindowClosedCallback,
    on_terminate: AppTerminateCallback,
    on_frontmost_app: FrontmostAppCallback,
    is_hidden: IsHiddenCallback,
}

impl WorkspacePollingObserver {
    pub fn new(
        on_new_window: NewWindowCallback,
        on_window_closed: WindowClosedCallback,
        on_terminate: AppTerminateCallback,
        on_frontmost_app: FrontmostAppCallback,
        is_hidden: IsHiddenCallback,
    ) -> Self {
        let mut known_windows = HashMap::new();
        for cg in get_cg_window_list() {
            known_windows.insert(cg.wid, cg.pid);
        }
        let known_pids = current_regular_pids();
        let known_frontmost_pid = frontmost_regular_application_pid();

        tracing::debug!(
            windows = known_windows.len(),
            pids = known_pids.len(),
            frontmost = known_frontmost_pid,
            "workspace polling initialized"
        );

        Self {
            known_windows,
            known_pids,
            known_frontmost_pid,
            on_new_window,
            on_window_closed,
            on_terminate,
            on_frontmost_app,
            is_hidden,
        }
    }

    /// Detect new windows and terminated apps.
    pub fn poll(&mut self) {
        let cg_windows = get_cg_window_list();
        let mut current_windows: HashMap<u32, i32> = HashMap::new();

        for cg in &cg_windows {
            current_windows.insert(cg.wid, cg.pid);

            // New window we haven't seen?
            if !self.known_windows.contains_key(&cg.wid) {
                tracing::info!(wid = cg.wid, pid = cg.pid, owner = %cg.owner, "new window detected");
                (self.on_new_window)(cg.pid, cg.owner.clone(), cg.wid);
            }
        }

        let current_pids = current_regular_pids();
        let current_frontmost_pid = frontmost_regular_application_pid();
        self.poll_with_snapshot(current_windows, current_pids, current_frontmost_pid);
    }

    fn poll_with_snapshot(
        &mut self,
        current_windows: HashMap<u32, i32>,
        current_pids: HashSet<i32>,
        current_frontmost_pid: Option<i32>,
    ) {
        // Detect windows that disappeared (but not ones we intentionally hid)
        for (wid, pid) in &self.known_windows {
            if !current_windows.contains_key(wid) && !(self.is_hidden)(*wid) {
                tracing::info!(wid, pid, "window disappeared");
                (self.on_window_closed)(*wid, *pid);
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

        if current_frontmost_pid != self.known_frontmost_pid {
            if let Some(pid) = current_frontmost_pid {
                tracing::debug!(pid, "frontmost app changed");
                (self.on_frontmost_app)(pid);
            }
            self.known_frontmost_pid = current_frontmost_pid;
        }

        self.known_windows = current_windows;
        self.known_pids = current_pids;
    }
}

fn current_regular_pids() -> HashSet<i32> {
    let mut current_pids = HashSet::new();
    let workspace = NSWorkspace::sharedWorkspace();
    let apps = workspace.runningApplications();
    let count = apps.count();
    for i in 0..count {
        let app = apps.objectAtIndex(i);
        if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
            current_pids.insert(app.processIdentifier());
        }
    }
    current_pids
}

fn frontmost_regular_application_pid() -> Option<i32> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    (app.activationPolicy() == NSApplicationActivationPolicy::Regular)
        .then(|| app.processIdentifier())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn frontmost_app_change_emits_only_when_pid_changes() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let event_sink = events.clone();
        let mut observer = WorkspacePollingObserver {
            known_windows: HashMap::new(),
            known_pids: HashSet::from([1, 2]),
            known_frontmost_pid: Some(1),
            on_new_window: Box::new(|_, _, _| {}),
            on_window_closed: Box::new(|_, _| {}),
            on_terminate: Box::new(|_| {}),
            on_frontmost_app: Box::new(move |pid| event_sink.borrow_mut().push(pid)),
            is_hidden: Box::new(|_| false),
        };

        observer.poll_with_snapshot(HashMap::new(), HashSet::from([1, 2]), Some(1));
        observer.poll_with_snapshot(HashMap::new(), HashSet::from([1, 2]), Some(2));
        observer.poll_with_snapshot(HashMap::new(), HashSet::from([1, 2]), Some(2));

        assert_eq!(&*events.borrow(), &[2]);
    }
}
