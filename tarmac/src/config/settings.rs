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
    /// HX-02: when true, `Action::Resize` adjusts the *deepest* matching
    /// ancestor split (boundary closest to the focused window moves first).
    /// When false, adjusts the *shallowest* matching ancestor (pre-HX-02
    /// behavior; boundary closest to the screen edge moves first).
    pub smart_resizing: bool,
    /// HX-03: when true, `Action::Swap(Direction)` at a workspace edge
    /// falls through to moving the focused window onto the geographically
    /// adjacent monitor in that direction. Applies to all four
    /// directions (pre-HX-03 was Left/Right only). When false the action
    /// no-ops at the edge.
    pub window_direction_monitor_fallback: bool,
    /// HX-05: split axis policy for newly-opened windows.
    ///   "smart"      — aspect ratio decides (default; Hyprland behavior).
    ///   "horizontal" — always stack top/bottom.
    ///   "vertical"   — always side-by-side.
    pub split_axis: String,
    /// HX-05: child-ordering policy when a new window splits a tile.
    ///   "follow_mouse" — cursor side wins (default; Hyprland force_split=0).
    ///   "left"         — new window becomes the first child.
    ///   "right"        — new window becomes the second child.
    pub force_split: String,
    /// HX-05: aspect-ratio threshold for "smart" split axis. Higher
    /// values bias toward Horizontal (stacked); lower toward Vertical.
    /// Mirrors Hyprland's `dwindle:split_width_multiplier`.
    pub split_width_multiplier: f64,
    /// HX-05: when true, inserting into an existing Internal preserves
    /// its split direction rather than re-deriving from aspect ratio.
    pub preserve_split: bool,
    /// HX-05: when true, `Action::Preselect(Direction)` persists across
    /// multiple inserts. When false (default) the hint is single-use.
    pub permanent_direction_override: bool,
    /// HX-05: where the focal point for new-window insertion comes from.
    ///   "cursor"           — current cursor location.
    ///   "active"           — focused window's center.
    ///   "cursor_or_active" — cursor if inside focused workspace, else
    ///                        active. (Default; matches Hyprland.)
    pub focal_point_source: String,
    /// HX-08: pixel delta a Mod+drag must travel before it's treated as
    /// a drag (versus a click). Below the threshold, drag scaffolding
    /// stays dormant and the gesture is effectively a click. Mirrors
    /// Hyprland's `binds:drag_threshold`. Default 5.0.
    pub drag_threshold: f64,
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
            smart_resizing: true,
            window_direction_monitor_fallback: true,
            split_axis: "smart".to_string(),
            force_split: "follow_mouse".to_string(),
            split_width_multiplier: 1.0,
            preserve_split: false,
            permanent_direction_override: false,
            focal_point_source: "cursor_or_active".to_string(),
            drag_threshold: 5.0,
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
            "smart_resizing" => {
                self.smart_resizing = value == "true";
            }
            "window_direction_monitor_fallback" => {
                self.window_direction_monitor_fallback = value == "true";
            }
            "split_axis" => match value {
                "smart" | "horizontal" | "vertical" => {
                    self.split_axis = value.to_string();
                }
                _ => {
                    tracing::warn!(
                        value,
                        "unknown split_axis (expected smart|horizontal|vertical)"
                    );
                }
            },
            "force_split" => match value {
                "follow_mouse" | "left" | "right" => {
                    self.force_split = value.to_string();
                }
                _ => {
                    tracing::warn!(
                        value,
                        "unknown force_split (expected follow_mouse|left|right)"
                    );
                }
            },
            "split_width_multiplier" => {
                if let Ok(v) = value.parse::<f64>()
                    && v > 0.0
                {
                    self.split_width_multiplier = v;
                }
            }
            "preserve_split" => {
                self.preserve_split = value == "true";
            }
            "permanent_direction_override" => {
                self.permanent_direction_override = value == "true";
            }
            "focal_point_source" => match value {
                "cursor" | "active" | "cursor_or_active" => {
                    self.focal_point_source = value.to_string();
                }
                _ => {
                    tracing::warn!(
                        value,
                        "unknown focal_point_source (expected cursor|active|cursor_or_active)"
                    );
                }
            },
            "drag_threshold" => {
                if let Ok(v) = value.parse::<f64>()
                    && v >= 0.0
                {
                    self.drag_threshold = v;
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
