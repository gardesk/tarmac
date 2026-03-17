use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, Result as LuaResult, Value};

use super::settings::Settings;
use crate::core::input::{Action, Key, Modifiers};
use crate::core::tree::Direction;

/// A window rule parsed from Lua config.
#[derive(Debug, Clone)]
pub struct WindowRule {
    pub app_name: Option<String>,
    pub title: Option<String>,
    pub floating: Option<bool>,
    pub workspace: Option<u8>,
}

/// A keybind parsed from Lua config.
#[derive(Debug, Clone)]
pub struct LuaKeybind {
    pub modifiers: Modifiers,
    pub key: Key,
    pub action: Action,
}

/// Result of loading a Lua config.
pub struct LuaConfig {
    pub settings: Settings,
    pub keybinds: Vec<LuaKeybind>,
    pub rules: Vec<WindowRule>,
}

/// Load and execute a Lua config file, returning settings, keybinds, and rules.
pub fn load_config(path: &std::path::Path) -> LuaConfig {
    let settings = Rc::new(RefCell::new(Settings::default()));
    let keybinds: Rc<RefCell<Vec<LuaKeybind>>> = Rc::new(RefCell::new(Vec::new()));
    let rules: Rc<RefCell<Vec<WindowRule>>> = Rc::new(RefCell::new(Vec::new()));

    if !path.exists() {
        tracing::warn!(?path, "no config file found, using defaults");
        let s = settings.borrow().clone();
        return LuaConfig {
            settings: s,
            keybinds: default_keybinds(&Settings::default()),
            rules: Vec::new(),
        };
    }

    let lua = Lua::new();

    // Register gar table
    if let Err(e) = register_gar_api(
        &lua,
        Rc::clone(&settings),
        Rc::clone(&keybinds),
        Rc::clone(&rules),
    ) {
        tracing::error!(err = %e, "failed to register gar API");
        let s = settings.borrow().clone();
        return LuaConfig {
            settings: s,
            keybinds: default_keybinds(&Settings::default()),
            rules: Vec::new(),
        };
    }

    // Load and execute
    match std::fs::read_to_string(path) {
        Ok(source) => {
            if let Err(e) = lua.load(&source).set_name(path.to_string_lossy()).exec() {
                tracing::error!(err = %e, "lua config error");
            } else {
                tracing::info!(?path, "config loaded");
            }
        }
        Err(e) => {
            tracing::error!(err = %e, "failed to read config file");
        }
    }

    let s = settings.borrow().clone();
    let mut binds = keybinds.borrow().clone();

    // If no keybinds were defined in config, use defaults
    if binds.is_empty() {
        binds = default_keybinds(&s);
    }

    let r = rules.borrow().clone();
    tracing::info!(rules = r.len(), "window rules loaded");

    LuaConfig {
        settings: s,
        keybinds: binds,
        rules: r,
    }
}

fn register_gar_api(
    lua: &Lua,
    settings: Rc<RefCell<Settings>>,
    keybinds: Rc<RefCell<Vec<LuaKeybind>>>,
    rules: Rc<RefCell<Vec<WindowRule>>>,
) -> LuaResult<()> {
    let gar = lua.create_table()?;

    // gar.set(key, value)
    let settings_clone = Rc::clone(&settings);
    gar.set(
        "set",
        lua.create_function(move |_, (key, value): (String, Value)| {
            let val_str = match &value {
                Value::String(s) => s.to_string_lossy().to_string(),
                Value::Number(n) => n.to_string(),
                Value::Integer(i) => i.to_string(),
                Value::Boolean(b) => b.to_string(),
                _ => format!("{:?}", value),
            };
            settings_clone.borrow_mut().set(&key, &val_str);
            tracing::debug!(key, val = val_str, "gar.set");
            Ok(())
        })?,
    )?;

    // gar.bind(keys, action_string)
    let keybinds_clone = Rc::clone(&keybinds);
    let settings_for_bind = Rc::clone(&settings);
    gar.set(
        "bind",
        lua.create_function(move |_, (keys, action): (String, String)| {
            let mod_key = settings_for_bind.borrow().mod_key;
            match parse_keybind_and_action(&keys, &action, mod_key) {
                Ok(kb) => {
                    tracing::debug!(keys, action, "gar.bind");
                    keybinds_clone.borrow_mut().push(kb);
                }
                Err(e) => {
                    tracing::warn!(keys, action, err = e, "failed to parse keybind");
                }
            }
            Ok(())
        })?,
    )?;

    // gar.exec(command)
    gar.set(
        "exec",
        lua.create_function(|_, command: String| {
            tracing::debug!(command, "gar.exec");
            std::process::Command::new("/bin/sh")
                .args(["-c", &command])
                .spawn()
                .ok();
            Ok(())
        })?,
    )?;

    // gar.exec_once(command)
    gar.set(
        "exec_once",
        lua.create_function(|_, command: String| {
            let name = command
                .split_whitespace()
                .next()
                .unwrap_or(&command)
                .to_string();
            let output = std::process::Command::new("pgrep")
                .arg("-x")
                .arg(&name)
                .output();
            let running = output.is_ok_and(|o| o.status.success());
            if !running {
                tracing::debug!(command, "gar.exec_once (starting)");
                std::process::Command::new("/bin/sh")
                    .args(["-c", &command])
                    .spawn()
                    .ok();
            } else {
                tracing::debug!(command, "gar.exec_once (already running)");
            }
            Ok(())
        })?,
    )?;

    // gar.rule({ app_name = "Firefox" }, { workspace = 2, floating = true })
    let rules_clone = Rc::clone(&rules);
    gar.set(
        "rule",
        lua.create_function(
            move |_, (match_table, actions_table): (mlua::Table, mlua::Table)| {
                let app_name: Option<String> = match_table.get("app_name").ok();
                let title: Option<String> = match_table.get("title").ok();
                // Also accept "class" as alias for "app_name" (gar Linux compat)
                let app_name = app_name.or_else(|| match_table.get("class").ok());

                let floating: Option<bool> = actions_table.get("floating").ok();
                let workspace: Option<u8> = actions_table.get("workspace").ok();

                let rule = WindowRule {
                    app_name,
                    title,
                    floating,
                    workspace,
                };
                tracing::debug!(?rule, "gar.rule");
                rules_clone.borrow_mut().push(rule);
                Ok(())
            },
        )?,
    )?;

    lua.globals().set("gar", gar)?;
    Ok(())
}

fn parse_keybind_and_action(
    keys: &str,
    action: &str,
    mod_key: Modifiers,
) -> Result<LuaKeybind, &'static str> {
    let (modifiers, key) = parse_key_spec(keys, mod_key)?;
    let action = parse_action(action)?;
    Ok(LuaKeybind {
        modifiers,
        key,
        action,
    })
}

fn parse_key_spec(spec: &str, mod_key: Modifiers) -> Result<(Modifiers, Key), &'static str> {
    let parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    let mut modifiers = Modifiers::empty();
    let mut key = None;

    for part in parts {
        match part.to_lowercase().as_str() {
            "mod" => modifiers |= mod_key,
            "shift" => modifiers |= Modifiers::SHIFT,
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "option" | "alt" => modifiers |= Modifiers::OPTION,
            "command" | "cmd" => modifiers |= Modifiers::COMMAND,
            k => {
                key = Some(parse_key_name(k)?);
            }
        }
    }

    Ok((modifiers, key.ok_or("no key in keybind")?))
}

fn parse_key_name(name: &str) -> Result<Key, &'static str> {
    match name {
        "a" => Ok(Key::A),
        "b" => Ok(Key::B),
        "c" => Ok(Key::C),
        "d" => Ok(Key::D),
        "e" => Ok(Key::E),
        "f" => Ok(Key::F),
        "g" => Ok(Key::G),
        "h" => Ok(Key::H),
        "i" => Ok(Key::I),
        "j" => Ok(Key::J),
        "k" => Ok(Key::K),
        "l" => Ok(Key::L),
        "m" => Ok(Key::M),
        "n" => Ok(Key::N),
        "o" => Ok(Key::O),
        "p" => Ok(Key::P),
        "q" => Ok(Key::Q),
        "r" => Ok(Key::R),
        "s" => Ok(Key::S),
        "t" => Ok(Key::T),
        "u" => Ok(Key::U),
        "v" => Ok(Key::V),
        "w" => Ok(Key::W),
        "x" => Ok(Key::X),
        "y" => Ok(Key::Y),
        "z" => Ok(Key::Z),
        "0" => Ok(Key::Num0),
        "1" => Ok(Key::Num1),
        "2" => Ok(Key::Num2),
        "3" => Ok(Key::Num3),
        "4" => Ok(Key::Num4),
        "5" => Ok(Key::Num5),
        "6" => Ok(Key::Num6),
        "7" => Ok(Key::Num7),
        "8" => Ok(Key::Num8),
        "9" => Ok(Key::Num9),
        "return" | "enter" => Ok(Key::Return),
        "space" => Ok(Key::Space),
        "tab" => Ok(Key::Tab),
        "escape" | "esc" => Ok(Key::Escape),
        "delete" | "backspace" => Ok(Key::Delete),
        "grave" | "`" => Ok(Key::Grave),
        "minus" | "-" => Ok(Key::Minus),
        "equal" | "=" => Ok(Key::Equal),
        "left" => Ok(Key::Left),
        "right" => Ok(Key::Right),
        "up" => Ok(Key::Up),
        "down" => Ok(Key::Down),
        _ => Err("unknown key name"),
    }
}

fn parse_action(action: &str) -> Result<Action, &'static str> {
    let parts: Vec<&str> = action.splitn(2, ' ').collect();
    match parts[0] {
        "focus" => {
            let dir = parse_direction(parts.get(1).copied().unwrap_or(""))?;
            Ok(Action::Focus(dir))
        }
        "swap" => {
            let dir = parse_direction(parts.get(1).copied().unwrap_or(""))?;
            Ok(Action::Swap(dir))
        }
        "resize" => {
            let dir = parse_direction(parts.get(1).copied().unwrap_or(""))?;
            Ok(Action::Resize(dir))
        }
        "workspace" => {
            let num: u8 = parts
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or("workspace requires a number")?;
            Ok(Action::Workspace(num))
        }
        "move_to_workspace" => {
            let num: u8 = parts
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or("move_to_workspace requires a number")?;
            Ok(Action::MoveToWorkspace(num))
        }
        "spawn_terminal" => Ok(Action::SpawnTerminal),
        "close" => Ok(Action::CloseWindow),
        "equalize" => Ok(Action::Equalize),
        "toggle_float" => Ok(Action::ToggleFloat),
        "workspace_next" => Ok(Action::WorkspaceNext),
        "workspace_prev" => Ok(Action::WorkspacePrev),
        "focus_monitor_next" => Ok(Action::FocusMonitorNext),
        "focus_monitor_prev" => Ok(Action::FocusMonitorPrev),
        "move_to_monitor_next" => Ok(Action::MoveToMonitorNext),
        "move_to_monitor_prev" => Ok(Action::MoveToMonitorPrev),
        "reload" => Ok(Action::Reload),
        "exit" => Ok(Action::Exit),
        _ => Err("unknown action"),
    }
}

fn parse_direction(s: &str) -> Result<Direction, &'static str> {
    match s {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        _ => Err("invalid direction"),
    }
}

/// Generate default keybinds based on settings.
pub fn default_keybinds(settings: &Settings) -> Vec<LuaKeybind> {
    let m = settings.mod_key;
    let ms = settings.mod_key | Modifiers::SHIFT;
    let mc = settings.mod_key | Modifiers::CONTROL;

    let mut binds = vec![
        LuaKeybind {
            modifiers: m,
            key: Key::Return,
            action: Action::SpawnTerminal,
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::Q,
            action: Action::CloseWindow,
        },
        LuaKeybind {
            modifiers: m,
            key: Key::E,
            action: Action::Equalize,
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::Space,
            action: Action::ToggleFloat,
        },
        // Focus
        LuaKeybind {
            modifiers: m,
            key: Key::H,
            action: Action::Focus(Direction::Left),
        },
        LuaKeybind {
            modifiers: m,
            key: Key::J,
            action: Action::Focus(Direction::Down),
        },
        LuaKeybind {
            modifiers: m,
            key: Key::K,
            action: Action::Focus(Direction::Up),
        },
        LuaKeybind {
            modifiers: m,
            key: Key::L,
            action: Action::Focus(Direction::Right),
        },
        LuaKeybind {
            modifiers: m,
            key: Key::Left,
            action: Action::Focus(Direction::Left),
        },
        LuaKeybind {
            modifiers: m,
            key: Key::Down,
            action: Action::Focus(Direction::Down),
        },
        LuaKeybind {
            modifiers: m,
            key: Key::Up,
            action: Action::Focus(Direction::Up),
        },
        LuaKeybind {
            modifiers: m,
            key: Key::Right,
            action: Action::Focus(Direction::Right),
        },
        // Swap
        LuaKeybind {
            modifiers: ms,
            key: Key::H,
            action: Action::Swap(Direction::Left),
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::J,
            action: Action::Swap(Direction::Down),
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::K,
            action: Action::Swap(Direction::Up),
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::L,
            action: Action::Swap(Direction::Right),
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::Left,
            action: Action::Swap(Direction::Left),
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::Down,
            action: Action::Swap(Direction::Down),
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::Up,
            action: Action::Swap(Direction::Up),
        },
        LuaKeybind {
            modifiers: ms,
            key: Key::Right,
            action: Action::Swap(Direction::Right),
        },
        // Resize
        LuaKeybind {
            modifiers: mc,
            key: Key::H,
            action: Action::Resize(Direction::Left),
        },
        LuaKeybind {
            modifiers: mc,
            key: Key::J,
            action: Action::Resize(Direction::Down),
        },
        LuaKeybind {
            modifiers: mc,
            key: Key::K,
            action: Action::Resize(Direction::Up),
        },
        LuaKeybind {
            modifiers: mc,
            key: Key::L,
            action: Action::Resize(Direction::Right),
        },
    ];

    // Workspaces
    for i in 1..=9u8 {
        let key = match i {
            1 => Key::Num1,
            2 => Key::Num2,
            3 => Key::Num3,
            4 => Key::Num4,
            5 => Key::Num5,
            6 => Key::Num6,
            7 => Key::Num7,
            8 => Key::Num8,
            9 => Key::Num9,
            _ => unreachable!(),
        };
        binds.push(LuaKeybind {
            modifiers: m,
            key,
            action: Action::Workspace(i),
        });
        binds.push(LuaKeybind {
            modifiers: ms,
            key,
            action: Action::MoveToWorkspace(i),
        });
    }
    binds.push(LuaKeybind {
        modifiers: m,
        key: Key::Num0,
        action: Action::Workspace(10),
    });
    binds.push(LuaKeybind {
        modifiers: ms,
        key: Key::Num0,
        action: Action::MoveToWorkspace(10),
    });

    binds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_spec_simple() {
        let (mods, key) = parse_key_spec("mod+h", Modifiers::COMMAND).unwrap();
        assert_eq!(mods, Modifiers::COMMAND);
        assert_eq!(key, Key::H);
    }

    #[test]
    fn parse_key_spec_shift() {
        let (mods, key) = parse_key_spec("mod+shift+q", Modifiers::COMMAND).unwrap();
        assert_eq!(mods, Modifiers::COMMAND | Modifiers::SHIFT);
        assert_eq!(key, Key::Q);
    }

    #[test]
    fn parse_key_spec_number() {
        let (mods, key) = parse_key_spec("mod+3", Modifiers::OPTION).unwrap();
        assert_eq!(mods, Modifiers::OPTION);
        assert_eq!(key, Key::Num3);
    }

    #[test]
    fn parse_action_focus() {
        let action = parse_action("focus left").unwrap();
        assert_eq!(action, Action::Focus(Direction::Left));
    }

    #[test]
    fn parse_action_workspace() {
        let action = parse_action("workspace 5").unwrap();
        assert_eq!(action, Action::Workspace(5));
    }

    #[test]
    fn parse_action_simple() {
        assert_eq!(parse_action("equalize").unwrap(), Action::Equalize);
        assert_eq!(parse_action("close").unwrap(), Action::CloseWindow);
        assert_eq!(parse_action("toggle_float").unwrap(), Action::ToggleFloat);
    }

    #[test]
    fn default_keybinds_count() {
        let binds = default_keybinds(&Settings::default());
        // 4 basic + 12 focus + 12 swap + 4 resize + 20 workspaces = 52
        assert!(binds.len() >= 40);
    }
}
