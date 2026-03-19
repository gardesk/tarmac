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
