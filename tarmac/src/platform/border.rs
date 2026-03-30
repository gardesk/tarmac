//! Border management via ers subprocess.
//! ers is a standalone border renderer that handles its own window events,
//! focus detection, and rendering. Tarmac just spawns and manages the process.

use std::process::{Child, Command};

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
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f64 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f64 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f64 / 255.0;
        let a = if hex.len() >= 8 {
            u8::from_str_radix(&hex[6..8], 16).unwrap_or(255) as f64 / 255.0
        } else {
            1.0
        };
        Self { r, g, b, a }
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
}

impl BorderManager {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            border_width: 0.0,
            focused_color: BorderColor::from_hex("#5294e2"),
            unfocused_color: BorderColor::from_hex("#2d2d2d"),
            radius: 10.0,
            child: None,
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
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "ers".to_string());

        let cmd = format!(
            "{} --active-only --width {} --radius {} --color '{}' --inactive '{}'",
            ers_bin,
            self.border_width,
            self.radius,
            self.focused_color.hex_string(),
            self.unfocused_color.hex_string(),
        );
        tracing::debug!(cmd, "spawning ers");
        match Command::new("/bin/sh").args(["-c", &cmd]).spawn() {
            Ok(child) => {
                self.child = Some(child);
            }
            Err(e) => {
                tracing::warn!(err = %e, "failed to spawn ers");
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

    /// Restart ers with current settings (used on config reload).
    pub fn restart(&mut self) {
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
