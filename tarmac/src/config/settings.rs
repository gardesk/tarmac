use crate::core::input::Modifiers;

/// Runtime settings loaded from Lua config.
#[derive(Debug, Clone)]
pub struct Settings {
    pub gap_inner: f64,
    pub gap_outer: f64,
    pub focus_follows_mouse: bool,
    pub mouse_follows_focus: bool,
    pub mod_key: Modifiers,
    pub terminal_command: String,
    /// Height of an external bar (e.g. sketchybar) to reserve at top of each display.
    pub bar_height: f64,
    /// Border width in pixels (0 = disabled).
    pub border_width: f64,
    /// Focused window border color as hex (#RRGGBB).
    pub border_color_focused: String,
    /// Unfocused window border color as hex (#RRGGBB).
    pub border_color_unfocused: String,
    /// Border corner radius in pixels.
    pub border_radius: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gap_inner: 0.0,
            gap_outer: 0.0,
            focus_follows_mouse: true,
            mouse_follows_focus: true,
            mod_key: Modifiers::COMMAND,
            terminal_command: "open -na WezTerm".to_string(),
            bar_height: 0.0,
            border_width: 0.0,
            border_color_focused: "#5294e2".to_string(),
            border_color_unfocused: "#2d2d2d".to_string(),
            border_radius: 10.0,
        }
    }
}

impl Settings {
    pub fn set(&mut self, key: &str, value: &str) {
        match key {
            "gap_inner" => {
                if let Ok(v) = value.parse() {
                    self.gap_inner = v;
                }
            }
            "gap_outer" => {
                if let Ok(v) = value.parse() {
                    self.gap_outer = v;
                }
            }
            "focus_follows_mouse" => {
                self.focus_follows_mouse = value == "true";
            }
            "mouse_follows_focus" => {
                self.mouse_follows_focus = value == "true";
            }
            "mod_key" => {
                self.mod_key = match value {
                    "command" | "cmd" => Modifiers::COMMAND,
                    "option" | "alt" => Modifiers::OPTION,
                    "control" | "ctrl" => Modifiers::CONTROL,
                    _ => {
                        tracing::warn!(key, value, "unknown mod_key value");
                        self.mod_key
                    }
                };
            }
            "terminal" => {
                self.terminal_command = value.to_string();
            }
            "bar_height" => {
                if let Ok(v) = value.parse() {
                    self.bar_height = v;
                }
            }
            "border_width" => {
                if let Ok(v) = value.parse() {
                    self.border_width = v;
                }
            }
            "border_color_focused" => {
                self.border_color_focused = value.to_string();
            }
            "border_color_unfocused" => {
                self.border_color_unfocused = value.to_string();
            }
            "border_radius" => {
                if let Ok(v) = value.parse() {
                    self.border_radius = v;
                }
            }
            _ => {
                tracing::trace!(key, value, "unknown setting (ignored)");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings() {
        let s = Settings::default();
        assert_eq!(s.gap_inner, 0.0);
        assert!(s.focus_follows_mouse);
        assert_eq!(s.mod_key, Modifiers::COMMAND);
    }

    #[test]
    fn set_gap() {
        let mut s = Settings::default();
        s.set("gap_inner", "12");
        assert_eq!(s.gap_inner, 12.0);
    }

    #[test]
    fn set_mod_key() {
        let mut s = Settings::default();
        s.set("mod_key", "option");
        assert_eq!(s.mod_key, Modifiers::OPTION);
    }

    #[test]
    fn set_unknown_ignored() {
        let mut s = Settings::default();
        s.set("nonexistent", "value");
        // No panic, no change
        assert_eq!(s.gap_inner, 0.0);
    }
}
