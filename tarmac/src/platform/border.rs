//! Border management via ers subprocess.
//! ers is a standalone border renderer that handles its own window events,
//! focus detection, and rendering. Tarmac just spawns and manages the process.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// RGBA color for border configuration.
#[derive(Debug, Clone, Copy)]
pub struct BorderColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl BorderColor {
    /// Parse a hex color string (#RRGGBB or #RRGGBBAA).
    pub fn from_hex(hex: &str) -> Self {
        let hex = hex.trim_start_matches('#');
        match Self::try_from_hex(hex) {
            Some(color) => color,
            None => {
                tracing::warn!(input = hex, "invalid border color, falling back to black");
                Self {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }
            }
        }
    }

    fn try_from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 6 && hex.len() != 8 {
            return None;
        }

        let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()? as f64 / 255.0;
        let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()? as f64 / 255.0;
        let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()? as f64 / 255.0;
        let a = if hex.len() == 8 {
            u8::from_str_radix(hex.get(6..8)?, 16).ok()? as f64 / 255.0
        } else {
            1.0
        };
        Some(Self { r, g, b, a })
    }

    fn hex_string(self) -> String {
        let r = (self.r * 255.0) as u8;
        let g = (self.g * 255.0) as u8;
        let b = (self.b * 255.0) as u8;
        let a = (self.a * 255.0) as u8;
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }
}

/// Manages the ers border renderer subprocess.
pub struct BorderManager {
    pub border_width: f64,
    pub focused_color: BorderColor,
    pub unfocused_color: BorderColor,
    pub radius: f64,
    child: Option<Child>,
    /// Timestamps of recent automatic respawns (oldest first). Used to
    /// rate-limit the watchdog so a permanently-broken ers backs off
    /// instead of fork-bombing the system.
    recent_restarts: Vec<Instant>,
    /// When set, suppress further respawn attempts until this time.
    backoff_until: Option<Instant>,
    /// Last time `health_check` ran try_wait on the child. Throttles the
    /// poll to ~1Hz from the 50ms run-loop tick.
    last_health_check: Option<Instant>,
}

const RESTART_WINDOW: Duration = Duration::from_secs(60);
const RESTART_BUDGET: usize = 5;
const BACKOFF_DURATION: Duration = Duration::from_secs(300);
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(1);

impl BorderManager {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            border_width: 0.0,
            focused_color: BorderColor::from_hex("#5294e2"),
            unfocused_color: BorderColor::from_hex("#2d2d2d"),
            radius: 10.0,
            child: None,
            recent_restarts: Vec::new(),
            backoff_until: None,
            last_health_check: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.border_width > 0.0
    }

    /// Spawn ers with current settings. Kills any existing instance first.
    /// Looks for ers next to the tarmac binary first, then falls back to PATH.
    pub fn spawn(&mut self) {
        self.kill();
        if !self.is_enabled() {
            return;
        }

        let ers_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("ers")))
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("ers"));

        tracing::debug!(
            ers_bin = ?ers_bin,
            border_width = self.border_width,
            radius = self.radius,
            focused = %self.focused_color.hex_string(),
            unfocused = %self.unfocused_color.hex_string(),
            "spawning ers"
        );

        match Command::new(&ers_bin)
            .arg("--active-only")
            .arg("--width")
            .arg(self.border_width.to_string())
            .arg("--radius")
            .arg(self.radius.to_string())
            .arg("--color")
            .arg(self.focused_color.hex_string())
            .arg("--inactive")
            .arg(self.unfocused_color.hex_string())
            .spawn()
        {
            Ok(child) => {
                self.child = Some(child);
            }
            Err(e) => {
                tracing::warn!(err = %e, ers_bin = ?ers_bin, "failed to spawn ers");
            }
        }
    }

    /// Kill the managed ers process if running.
    pub fn kill(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
    }

    /// Restart ers with current settings (used on config reload). Resets
    /// the watchdog's backoff so a deliberate user reload always tries
    /// to spawn fresh.
    pub fn restart(&mut self) {
        self.recent_restarts.clear();
        self.backoff_until = None;
        self.spawn();
    }

    /// Periodically reap the ers child and respawn it if it died, so a
    /// crash inside the renderer doesn't strand tarmac with no borders.
    /// Throttled internally so callers can invoke from the run-loop tick.
    /// Restarts are budgeted: more than `RESTART_BUDGET` restarts inside
    /// `RESTART_WINDOW` triggers a `BACKOFF_DURATION` cooldown, after
    /// which the watchdog tries once more.
    pub fn health_check(&mut self) {
        if !self.is_enabled() {
            return;
        }
        let now = Instant::now();
        if let Some(prev) = self.last_health_check
            && now.duration_since(prev) < HEALTH_CHECK_INTERVAL
        {
            return;
        }
        self.last_health_check = Some(now);

        let died = match self.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => Some(status),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(err = %e, "ers try_wait failed");
                    None
                }
            },
            None => None,
        };

        let Some(status) = died else {
            return;
        };
        // Drop the dead handle so kill()/spawn() don't try to wait on it
        // again.
        self.child = None;

        if let Some(until) = self.backoff_until
            && now < until
        {
            tracing::warn!(
                ?status,
                remaining_secs = (until - now).as_secs(),
                "ers exited; respawn suppressed by backoff"
            );
            return;
        }
        self.backoff_until = None;

        // Sliding-window rate limit.
        let cutoff = now - RESTART_WINDOW;
        self.recent_restarts.retain(|t| *t >= cutoff);
        if self.recent_restarts.len() >= RESTART_BUDGET {
            tracing::error!(
                ?status,
                budget = RESTART_BUDGET,
                window_secs = RESTART_WINDOW.as_secs(),
                "ers crashed too many times; backing off"
            );
            self.backoff_until = Some(now + BACKOFF_DURATION);
            self.recent_restarts.clear();
            return;
        }

        tracing::warn!(?status, "ers exited unexpectedly; respawning");
        self.recent_restarts.push(now);
        self.spawn();
    }

    // Stub methods for compatibility with existing state.rs calls.
    // ers handles all of these independently.
    pub fn update_border(&mut self, _wid: u32, _rect: crate::core::tree::Rect, _focused: bool) {}
    pub fn remove_border(&mut self, _wid: u32) {}
    pub fn update_focus(
        &mut self,
        _old: Option<u32>,
        _new: Option<u32>,
        _get_rect: impl Fn(u32) -> Option<crate::core::tree::Rect>,
    ) {
    }
}

impl Drop for BorderManager {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::BorderColor;

    #[test]
    fn malformed_hex_falls_back_without_panicking() {
        let color = BorderColor::from_hex("#fff");
        assert_eq!(color.r, 0.0);
        assert_eq!(color.g, 0.0);
        assert_eq!(color.b, 0.0);
        assert_eq!(color.a, 1.0);
    }

    #[test]
    fn parses_rgba_hex() {
        let color = BorderColor::from_hex("#11223380");
        assert_eq!(color.hex_string(), "#11223380");
    }
}
