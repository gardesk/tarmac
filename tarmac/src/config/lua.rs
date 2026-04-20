use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, Result as LuaResult, Value, Variadic};
use regex::RegexBuilder;

use super::settings::Settings;
use crate::core::input::{Action, Key, Modifiers};
use crate::core::tree::Direction;
use crate::core::workspace::{
    MonitorAssignment, WorkspaceDefinition, WorkspaceId, WorkspaceLayout, WorkspaceTarget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMatchMode {
    Contains,
    Exact,
    Regex,
}

impl RuleMatchMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "contains" => Some(Self::Contains),
            "exact" => Some(Self::Exact),
            "regex" => Some(Self::Regex),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Exact => "exact",
            Self::Regex => "regex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulePattern {
    pub value: String,
    pub mode: RuleMatchMode,
}

impl RulePattern {
    pub fn new(value: impl Into<String>, mode: RuleMatchMode) -> Self {
        Self {
            value: value.into(),
            mode,
        }
    }

    pub fn from_legacy(value: impl Into<String>, default_mode: RuleMatchMode) -> Self {
        let value = value.into();
        if value.starts_with('/') && value.ends_with('/') && value.len() > 2 {
            Self::new(value[1..value.len() - 1].to_string(), RuleMatchMode::Regex)
        } else {
            Self::new(value, default_mode)
        }
    }

    pub fn matches(&self, haystack: &str) -> bool {
        match self.mode {
            RuleMatchMode::Contains => haystack.to_lowercase().contains(&self.value.to_lowercase()),
            RuleMatchMode::Exact => haystack.eq_ignore_ascii_case(&self.value),
            RuleMatchMode::Regex => match RegexBuilder::new(&self.value)
                .case_insensitive(true)
                .build()
            {
                Ok(regex) => regex.is_match(haystack),
                Err(err) => {
                    tracing::warn!(pattern = self.value, %err, "invalid regex in window rule");
                    false
                }
            },
        }
    }

    pub fn legacy_display(&self) -> String {
        match self.mode {
            RuleMatchMode::Regex => format!("/{}/", self.value),
            RuleMatchMode::Contains | RuleMatchMode::Exact => self.value.clone(),
        }
    }
}

/// A window rule parsed from Lua config.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowRule {
    pub id: Option<String>,
    pub name: Option<String>,
    pub enabled: bool,
    pub app_name: Option<RulePattern>,
    pub app_bundle: Option<RulePattern>,
    pub title: Option<RulePattern>,
    pub floating: Option<bool>,
    /// Workspace assignment: numeric ("1"-"10") or special ("special:terminal").
    pub workspace: Option<String>,
    pub geometry: Option<(f64, f64, f64, f64)>, // x, y, width, height
}

impl WindowRule {
    pub fn new() -> Self {
        Self {
            id: None,
            name: None,
            enabled: true,
            app_name: None,
            app_bundle: None,
            title: None,
            floating: None,
            workspace: None,
            geometry: None,
        }
    }

    pub fn effective_id(&self, index: usize) -> String {
        self.id
            .as_ref()
            .filter(|id| !id.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("rule_{:03}", index + 1))
    }

    pub fn effective_name(&self, index: usize) -> String {
        self.name
            .as_ref()
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Rule {}", index + 1))
    }

    pub fn matches_window(&self, app_name: &str, app_bundle: &str, title: &str) -> bool {
        self.enabled
            && self
                .app_name
                .as_ref()
                .is_none_or(|pattern| pattern.matches(app_name))
            && self
                .app_bundle
                .as_ref()
                .is_none_or(|pattern| pattern.matches(app_bundle))
            && self
                .title
                .as_ref()
                .is_none_or(|pattern| pattern.matches(title))
    }
}

impl Default for WindowRule {
    fn default() -> Self {
        Self::new()
    }
}

/// A keybind parsed from Lua config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaKeybind {
    pub modifiers: Modifiers,
    pub key: Key,
    pub action: Action,
}

/// Configuration for a special (scratchpad) workspace overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecialWorkspaceConfig {
    pub name: String,
    /// "center", "top", "bottom"
    pub position: String,
    /// Width as fraction of screen (0.0 - 1.0)
    pub width: f64,
    /// Height as fraction of screen (0.0 - 1.0)
    pub height: f64,
}

impl SpecialWorkspaceConfig {
    pub fn default_for(name: &str) -> Self {
        Self {
            name: name.to_string(),
            position: "center".to_string(),
            width: 0.7,
            height: 0.7,
        }
    }
}

/// A registered event callback (holds a Lua registry key for the function).
pub struct EventCallback {
    pub event: String,
    pub func_key: mlua::RegistryKey,
}

/// Result of loading a Lua config. Holds the Lua state for event callbacks.
pub struct LuaConfig {
    pub settings: Settings,
    pub keybinds: Vec<LuaKeybind>,
    pub rules: Vec<WindowRule>,
    pub special_configs: Vec<SpecialWorkspaceConfig>,
    pub workspace_defs: Vec<WorkspaceDefinition>,
    pub lua: Option<Lua>,
    pub callbacks: Vec<EventCallback>,
}

fn default_lua_config(settings: Settings) -> LuaConfig {
    LuaConfig {
        settings: settings.clone(),
        keybinds: default_keybinds(&settings),
        rules: Vec::new(),
        special_configs: Vec::new(),
        workspace_defs: default_workspace_definitions(),
        lua: None,
        callbacks: Vec::new(),
    }
}

/// Load and execute a Lua config file, returning settings, keybinds, and rules.
pub fn load_config(path: &std::path::Path) -> LuaConfig {
    if !path.exists() {
        tracing::warn!(?path, "no config file found, using defaults");
        return default_lua_config(Settings::default());
    }

    match std::fs::read_to_string(path) {
        Ok(source) => load_config_from_source(&source, &path.to_string_lossy()),
        Err(e) => {
            tracing::error!(err = %e, "failed to read config file");
            default_lua_config(Settings::default())
        }
    }
}

pub fn load_config_from_source(source: &str, chunk_name: &str) -> LuaConfig {
    let settings = Rc::new(RefCell::new(Settings::default()));
    let keybinds: Rc<RefCell<Vec<LuaKeybind>>> = Rc::new(RefCell::new(Vec::new()));
    let rules: Rc<RefCell<Vec<WindowRule>>> = Rc::new(RefCell::new(Vec::new()));
    let callbacks: Rc<RefCell<Vec<EventCallback>>> = Rc::new(RefCell::new(Vec::new()));
    let special_configs: Rc<RefCell<Vec<SpecialWorkspaceConfig>>> =
        Rc::new(RefCell::new(Vec::new()));
    let workspace_defs: Rc<RefCell<Vec<WorkspaceDefinition>>> =
        Rc::new(RefCell::new(default_workspace_definitions()));

    let lua = Lua::new();

    if let Err(e) = register_gar_api(
        &lua,
        Rc::clone(&settings),
        Rc::clone(&keybinds),
        Rc::clone(&rules),
        Rc::clone(&callbacks),
        Rc::clone(&special_configs),
        Rc::clone(&workspace_defs),
    ) {
        tracing::error!(err = %e, "failed to register gar API");
        return default_lua_config(settings.borrow().clone());
    }

    if let Err(e) = lua.load(source).set_name(chunk_name).exec() {
        tracing::error!(err = %e, chunk_name, "lua config error");
    } else {
        tracing::debug!(chunk_name, "lua chunk loaded");
    }

    let s = settings.borrow().clone();
    let mut binds = keybinds.borrow().clone();

    // If no keybinds were defined in config, use defaults
    if binds.is_empty() {
        binds = default_keybinds(&s);
    }

    let r = rules.borrow().clone();
    let sc = special_configs.borrow().clone();
    let mut wd = workspace_defs.borrow().clone();
    wd.sort_by_key(workspace_sort_key);
    wd.dedup_by(|a, b| a.id == b.id);
    let cbs = callbacks.borrow_mut().drain(..).collect::<Vec<_>>();
    tracing::debug!(rules = r.len(), callbacks = cbs.len(), "lua config parsed");

    LuaConfig {
        settings: s,
        keybinds: binds,
        rules: r,
        special_configs: sc,
        workspace_defs: wd,
        lua: Some(lua),
        callbacks: cbs,
    }
}

fn register_gar_api(
    lua: &Lua,
    settings: Rc<RefCell<Settings>>,
    keybinds: Rc<RefCell<Vec<LuaKeybind>>>,
    rules: Rc<RefCell<Vec<WindowRule>>>,
    callbacks: Rc<RefCell<Vec<EventCallback>>>,
    special_configs: Rc<RefCell<Vec<SpecialWorkspaceConfig>>>,
    workspace_defs: Rc<RefCell<Vec<WorkspaceDefinition>>>,
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
    // gar.rule({ id = "rule_01", name = "...", enabled = true, match = {...}, actions = {...} })
    let rules_clone = Rc::clone(&rules);
    gar.set(
        "rule",
        lua.create_function(move |_, args: Variadic<Value>| {
            let rule = parse_rule_args(args)?;
            tracing::debug!(?rule, "gar.rule");
            rules_clone.borrow_mut().push(rule);
            Ok(())
        })?,
    )?;

    // gar.on("event_name", function(...) end)
    // Stores the Lua function in the registry for later invocation.
    let callbacks_clone = Rc::clone(&callbacks);
    gar.set(
        "on",
        lua.create_function(move |lua_ctx, (event, func): (String, mlua::Function)| {
            let key = lua_ctx.create_registry_value(func)?;
            tracing::debug!(event, "gar.on callback registered");
            callbacks_clone.borrow_mut().push(EventCallback {
                event,
                func_key: key,
            });
            Ok(())
        })?,
    )?;

    // gar.special_workspace("name", { position = "center", width = 0.8, height = 0.5 })
    let specials_clone = Rc::clone(&special_configs);
    gar.set(
        "special_workspace",
        lua.create_function(move |_, (name, opts): (String, Option<mlua::Table>)| {
            let mut cfg = SpecialWorkspaceConfig::default_for(&name);
            if let Some(t) = opts {
                if let Ok(p) = t.get::<String>("position") {
                    cfg.position = p;
                }
                if let Ok(w) = t.get::<f64>("width") {
                    cfg.width = w.clamp(0.1, 1.0);
                }
                if let Ok(h) = t.get::<f64>("height") {
                    cfg.height = h.clamp(0.1, 1.0);
                }
            }
            tracing::debug!(
                name = cfg.name,
                pos = cfg.position,
                w = cfg.width,
                h = cfg.height,
                "gar.special_workspace"
            );
            specials_clone.borrow_mut().push(cfg);
            Ok(())
        })?,
    )?;

    let workspace_defs_clone = Rc::clone(&workspace_defs);
    gar.set(
        "workspace",
        lua.create_function(move |_, (id, opts): (Value, Option<mlua::Table>)| {
            let id = match id {
                Value::Integer(num) if num > 0 => WorkspaceId::Numbered(num as u8),
                Value::Number(num) if num > 0.0 => WorkspaceId::Numbered(num as u8),
                Value::String(s) => WorkspaceId::parse(&s.to_string_lossy())
                    .ok_or_else(|| mlua::Error::runtime("invalid workspace id"))?,
                _ => return Err(mlua::Error::runtime("invalid workspace id")),
            };

            let mut def = WorkspaceDefinition::new(id);
            if let Some(opts) = opts {
                if let Ok(display_id) = opts.get::<u32>("monitor") {
                    def.prefs.monitor = Some(MonitorAssignment { display_id });
                }
                if let Ok(layout) = opts.get::<String>("layout")
                    && let Some(layout) = WorkspaceLayout::parse(&layout)
                {
                    def.prefs.default_layout = layout;
                }
                if let Ok(gap_inner) = opts.get::<f64>("gap_inner") {
                    def.prefs.gap_inner = Some(gap_inner.max(0.0));
                }
                if let Ok(gap_outer) = opts.get::<f64>("gap_outer") {
                    def.prefs.gap_outer = Some(gap_outer.max(0.0));
                }
            }

            tracing::debug!(workspace = %def.id, "gar.workspace");
            let mut defs = workspace_defs_clone.borrow_mut();
            if let Some(existing) = defs.iter_mut().find(|existing| existing.id == def.id) {
                *existing = def;
            } else {
                defs.push(def);
            }
            Ok(())
        })?,
    )?;

    lua.globals().set("gar", gar)?;
    Ok(())
}

fn parse_rule_args(args: Variadic<Value>) -> LuaResult<WindowRule> {
    match args.as_slice() {
        [Value::Table(rule_table)] => parse_structured_rule(rule_table.clone()),
        [Value::Table(match_table), Value::Table(actions_table)] => {
            parse_rule_tables(None, match_table.clone(), actions_table.clone())
        }
        _ => Err(mlua::Error::runtime(
            "gar.rule expects (match, actions) or ({ id, name, enabled, match, actions })",
        )),
    }
}

#[derive(Debug, Clone)]
struct ParsedRuleMetadata {
    id: Option<String>,
    name: Option<String>,
    enabled: bool,
}

impl Default for ParsedRuleMetadata {
    fn default() -> Self {
        Self {
            id: None,
            name: None,
            enabled: true,
        }
    }
}

fn parse_structured_rule(rule_table: mlua::Table) -> LuaResult<WindowRule> {
    let metadata = ParsedRuleMetadata {
        id: parse_optional_string_value(rule_table.get::<Value>("id")?),
        name: parse_optional_string_value(rule_table.get::<Value>("name")?),
        enabled: parse_optional_bool(rule_table.get::<Value>("enabled")?).unwrap_or(true),
    };

    let match_table = match rule_table.get::<Value>("match")? {
        Value::Table(table) => table,
        Value::Nil => rule_table.clone(),
        _ => return Err(mlua::Error::runtime("rule.match must be a table")),
    };
    let actions_table = match rule_table.get::<Value>("actions")? {
        Value::Table(table) => table,
        Value::Nil => rule_table,
        _ => return Err(mlua::Error::runtime("rule.actions must be a table")),
    };

    parse_rule_tables(Some(metadata), match_table, actions_table)
}

fn parse_rule_tables(
    metadata: Option<ParsedRuleMetadata>,
    match_table: mlua::Table,
    actions_table: mlua::Table,
) -> LuaResult<WindowRule> {
    let metadata = metadata.unwrap_or_default();
    let app_name = parse_match_pattern(
        match_table.get::<Value>("app_name")?,
        RuleMatchMode::Contains,
    )?
    .or_else(|| {
        parse_match_pattern(
            match_table.get::<Value>("class").unwrap_or(Value::Nil),
            RuleMatchMode::Contains,
        )
        .ok()
        .flatten()
    })
    .or_else(|| {
        parse_match_pattern(
            match_table.get::<Value>("app").unwrap_or(Value::Nil),
            RuleMatchMode::Contains,
        )
        .ok()
        .flatten()
    });
    let app_bundle = parse_match_pattern(
        match_table.get::<Value>("app_bundle")?,
        RuleMatchMode::Contains,
    )?
    .or_else(|| {
        parse_match_pattern(
            match_table.get::<Value>("bundle_id").unwrap_or(Value::Nil),
            RuleMatchMode::Contains,
        )
        .ok()
        .flatten()
    });
    let title = parse_match_pattern(match_table.get::<Value>("title")?, RuleMatchMode::Contains)?;

    let floating = parse_optional_bool(actions_table.get::<Value>("floating")?)
        .or_else(|| parse_optional_bool(actions_table.get::<Value>("float").unwrap_or(Value::Nil)));
    let workspace = parse_workspace_value(actions_table.get::<Value>("workspace")?)?;
    let geometry = parse_geometry_value(&actions_table)?;

    Ok(WindowRule {
        id: metadata.id,
        name: metadata.name,
        enabled: metadata.enabled,
        app_name,
        app_bundle,
        title,
        floating,
        workspace,
        geometry,
    })
}

fn parse_match_pattern(
    value: Value,
    default_mode: RuleMatchMode,
) -> LuaResult<Option<RulePattern>> {
    match value {
        Value::Nil => Ok(None),
        Value::String(text) => Ok(Some(RulePattern::from_legacy(
            text.to_string_lossy().to_string(),
            default_mode,
        ))),
        Value::Integer(number) => Ok(Some(RulePattern::new(number.to_string(), default_mode))),
        Value::Number(number) => Ok(Some(RulePattern::new(number.to_string(), default_mode))),
        Value::Table(table) => {
            let raw_value = parse_optional_string_value(table.get::<Value>("value")?)
                .ok_or_else(|| mlua::Error::runtime("rule match table requires value"))?;
            let mode = parse_optional_string_value(table.get::<Value>("mode")?)
                .and_then(|mode| RuleMatchMode::parse(&mode))
                .unwrap_or(default_mode);
            Ok(Some(RulePattern::new(raw_value, mode)))
        }
        _ => Err(mlua::Error::runtime("invalid rule match value")),
    }
}

fn parse_workspace_value(value: Value) -> LuaResult<Option<String>> {
    match value {
        Value::Nil => Ok(None),
        Value::String(text) => Ok(Some(text.to_string_lossy().to_string())),
        Value::Integer(number) if number > 0 => Ok(Some(number.to_string())),
        Value::Number(number) if number > 0.0 => Ok(Some((number as u8).to_string())),
        _ => Err(mlua::Error::runtime("invalid workspace value for rule")),
    }
}

fn parse_geometry_value(actions_table: &mlua::Table) -> LuaResult<Option<(f64, f64, f64, f64)>> {
    if let Value::Table(geometry) = actions_table.get::<Value>("geometry")? {
        return Ok(Some((
            geometry.get("x").unwrap_or(100.0),
            geometry.get("y").unwrap_or(100.0),
            geometry.get("width").unwrap_or(800.0),
            geometry.get("height").unwrap_or(600.0),
        )));
    }

    let x = parse_optional_number(actions_table.get::<Value>("x")?);
    let y = parse_optional_number(actions_table.get::<Value>("y")?);
    let width = parse_optional_number(actions_table.get::<Value>("width")?);
    let height = parse_optional_number(actions_table.get::<Value>("height")?);

    if x.is_some() || y.is_some() || width.is_some() || height.is_some() {
        Ok(Some((
            x.unwrap_or(100.0),
            y.unwrap_or(100.0),
            width.unwrap_or(800.0),
            height.unwrap_or(600.0),
        )))
    } else {
        Ok(None)
    }
}

fn parse_optional_bool(value: Value) -> Option<bool> {
    match value {
        Value::Boolean(value) => Some(value),
        _ => None,
    }
}

fn parse_optional_number(value: Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(value as f64),
        Value::Number(value) => Some(value),
        _ => None,
    }
}

fn parse_optional_string_value(value: Value) -> Option<String> {
    match value {
        Value::Nil => None,
        Value::String(value) => {
            let value = value.to_string_lossy().to_string();
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        }
        Value::Integer(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Boolean(value) => Some(value.to_string()),
        _ => None,
    }
}

impl LuaConfig {
    /// Fire all callbacks registered for a given event.
    pub fn fire_event(&self, event: &str, args: &[&str]) {
        let Some(lua) = &self.lua else { return };
        for cb in &self.callbacks {
            if cb.event == event {
                match lua.registry_value::<mlua::Function>(&cb.func_key) {
                    Ok(func) => {
                        // Build args as Lua strings
                        let lua_args: Vec<mlua::Value> = args
                            .iter()
                            .filter_map(|a| lua.create_string(a).ok().map(mlua::Value::String))
                            .collect();
                        if let Err(e) = func.call::<()>(mlua::MultiValue::from_iter(lua_args)) {
                            tracing::warn!(event, err = %e, "callback error");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(event, err = %e, "failed to retrieve callback");
                    }
                }
            }
        }
    }
}

pub fn parse_keybind(
    keys: &str,
    action: &str,
    mod_key: Modifiers,
) -> Result<LuaKeybind, &'static str> {
    parse_keybind_and_action(keys, action, mod_key)
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
        "comma" | "," => Ok(Key::Comma),
        "period" | "." => Ok(Key::Period),
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
            let target = parts
                .get(1)
                .and_then(|s| WorkspaceTarget::parse(s))
                .ok_or("workspace requires a valid target")?;
            Ok(Action::Workspace(target))
        }
        "move_to_workspace" => {
            let target = parts
                .get(1)
                .and_then(|s| WorkspaceTarget::parse(s))
                .ok_or("move_to_workspace requires a valid target")?;
            Ok(Action::MoveToWorkspace(target))
        }
        "spawn_terminal" => Ok(Action::SpawnTerminal),
        "close" => Ok(Action::CloseWindow),
        "equalize" => Ok(Action::Equalize),
        "toggle_float" => Ok(Action::ToggleFloat),
        "unstack" => Ok(Action::Unstack),
        "promote_stack" => Ok(Action::PromoteStack),
        "workspace_next" => Ok(Action::WorkspaceNext),
        "workspace_prev" => Ok(Action::WorkspacePrev),
        "focus_monitor_next" => Ok(Action::FocusMonitorNext),
        "focus_monitor_prev" => Ok(Action::FocusMonitorPrev),
        "move_to_monitor_next" => Ok(Action::MoveToMonitorNext),
        "move_to_monitor_prev" => Ok(Action::MoveToMonitorPrev),
        "reload" => Ok(Action::Reload),
        "exit" => Ok(Action::Exit),
        "toggle_special" => {
            let name = parts.get(1).ok_or("toggle_special requires a name")?;
            Ok(Action::ToggleSpecial(name.to_string()))
        }
        "move_to_special" => {
            let name = parts.get(1).ok_or("move_to_special requires a name")?;
            Ok(Action::MoveToSpecial(name.to_string()))
        }
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
        LuaKeybind {
            modifiers: ms,
            key: Key::U,
            action: Action::Unstack,
        },
        LuaKeybind {
            modifiers: Modifiers::OPTION | Modifiers::SHIFT,
            key: Key::Period,
            action: Action::PromoteStack,
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
            action: Action::Workspace(WorkspaceTarget::Numbered(i)),
        });
        binds.push(LuaKeybind {
            modifiers: ms,
            key,
            action: Action::MoveToWorkspace(WorkspaceTarget::Numbered(i)),
        });
    }
    binds.push(LuaKeybind {
        modifiers: m,
        key: Key::Num0,
        action: Action::Workspace(WorkspaceTarget::Numbered(10)),
    });
    binds.push(LuaKeybind {
        modifiers: ms,
        key: Key::Num0,
        action: Action::MoveToWorkspace(WorkspaceTarget::Numbered(10)),
    });

    binds
}

pub fn default_workspace_definitions() -> Vec<WorkspaceDefinition> {
    (1..=10)
        .map(|num| WorkspaceDefinition::new(WorkspaceId::Numbered(num)))
        .collect()
}

fn workspace_sort_key(def: &WorkspaceDefinition) -> (u8, String) {
    match &def.id {
        WorkspaceId::Numbered(num) => (0, format!("{num:02}")),
        WorkspaceId::Lettered(ch) => (1, ch.to_string()),
        WorkspaceId::Special(name) => (2, name.clone()),
    }
}

pub fn key_name(key: Key) -> &'static str {
    match key {
        Key::A => "a",
        Key::B => "b",
        Key::C => "c",
        Key::D => "d",
        Key::E => "e",
        Key::F => "f",
        Key::G => "g",
        Key::H => "h",
        Key::I => "i",
        Key::J => "j",
        Key::K => "k",
        Key::L => "l",
        Key::M => "m",
        Key::N => "n",
        Key::O => "o",
        Key::P => "p",
        Key::Q => "q",
        Key::R => "r",
        Key::S => "s",
        Key::T => "t",
        Key::U => "u",
        Key::V => "v",
        Key::W => "w",
        Key::X => "x",
        Key::Y => "y",
        Key::Z => "z",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::Return => "return",
        Key::Space => "space",
        Key::Tab => "tab",
        Key::Escape => "escape",
        Key::Delete => "delete",
        Key::Grave => "grave",
        Key::Minus => "minus",
        Key::Equal => "equal",
        Key::LeftBracket => "left_bracket",
        Key::RightBracket => "right_bracket",
        Key::Semicolon => "semicolon",
        Key::Quote => "quote",
        Key::Comma => "comma",
        Key::Period => "period",
        Key::Slash => "slash",
        Key::Backslash => "backslash",
        Key::Left => "left",
        Key::Right => "right",
        Key::Up => "up",
        Key::Down => "down",
        Key::F1 => "f1",
        Key::F2 => "f2",
        Key::F3 => "f3",
        Key::F4 => "f4",
        Key::F5 => "f5",
        Key::F6 => "f6",
        Key::F7 => "f7",
        Key::F8 => "f8",
        Key::F9 => "f9",
        Key::F10 => "f10",
        Key::F11 => "f11",
        Key::F12 => "f12",
    }
}

pub fn format_key_spec(modifiers: Modifiers, key: Key, mod_key: Modifiers) -> String {
    let mut parts = Vec::new();
    if modifiers.contains(mod_key) {
        parts.push("mod");
    }
    if modifiers.contains(Modifiers::SHIFT) {
        parts.push("shift");
    }
    if modifiers.contains(Modifiers::CONTROL) && mod_key != Modifiers::CONTROL {
        parts.push("ctrl");
    }
    if modifiers.contains(Modifiers::OPTION) && mod_key != Modifiers::OPTION {
        parts.push("option");
    }
    if modifiers.contains(Modifiers::COMMAND) && mod_key != Modifiers::COMMAND {
        parts.push("command");
    }
    parts.push(key_name(key));
    parts.join("+")
}

pub fn format_action(action: &Action) -> String {
    match action {
        Action::SpawnTerminal => "spawn_terminal".to_string(),
        Action::CloseWindow => "close".to_string(),
        Action::Focus(dir) => format!("focus {}", direction_name(*dir)),
        Action::Swap(dir) => format!("swap {}", direction_name(*dir)),
        Action::Resize(dir) => format!("resize {}", direction_name(*dir)),
        Action::Equalize => "equalize".to_string(),
        Action::Workspace(target) => format!("workspace {target}"),
        Action::MoveToWorkspace(target) => format!("move_to_workspace {target}"),
        Action::WorkspaceNext => "workspace_next".to_string(),
        Action::WorkspacePrev => "workspace_prev".to_string(),
        Action::ToggleFloat => "toggle_float".to_string(),
        Action::Unstack => "unstack".to_string(),
        Action::PromoteStack => "promote_stack".to_string(),
        Action::ToggleSpecial(name) => format!("toggle_special {name}"),
        Action::MoveToSpecial(name) => format!("move_to_special {name}"),
        Action::FocusMonitorNext => "focus_monitor_next".to_string(),
        Action::FocusMonitorPrev => "focus_monitor_prev".to_string(),
        Action::MoveToMonitorNext => "move_to_monitor_next".to_string(),
        Action::MoveToMonitorPrev => "move_to_monitor_prev".to_string(),
        Action::Reload => "reload".to_string(),
        Action::Exit => "exit".to_string(),
    }
}

pub fn format_rule_match(rule: &WindowRule) -> String {
    let mut parts = Vec::new();
    if let Some(app_name) = &rule.app_name {
        parts.push(format!("app_name={}", app_name.legacy_display()));
    }
    if let Some(app_bundle) = &rule.app_bundle {
        parts.push(format!("app_bundle={}", app_bundle.legacy_display()));
    }
    if let Some(title) = &rule.title {
        parts.push(format!("title={}", title.legacy_display()));
    }
    if parts.is_empty() {
        "*".to_string()
    } else {
        parts.join(", ")
    }
}

pub fn format_rule_action(rule: &WindowRule) -> String {
    let mut parts = Vec::new();
    if let Some(true) = rule.floating {
        parts.push("floating=true".to_string());
    }
    if let Some(workspace) = &rule.workspace {
        parts.push(format!("workspace={workspace}"));
    }
    if let Some((x, y, width, height)) = rule.geometry {
        parts.push(format!("geometry={x:.0},{y:.0},{width:.0},{height:.0}"));
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

pub fn lua_number(v: f64) -> String {
    let i = v as i64;
    if (v - i as f64).abs() < 0.01 {
        i.to_string()
    } else {
        format!("{v:.2}")
    }
}

pub fn lua_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
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
        assert_eq!(action, Action::Workspace(WorkspaceTarget::Numbered(5)));
    }

    #[test]
    fn parse_action_lettered_workspace() {
        let action = parse_action("workspace w").unwrap();
        assert_eq!(action, Action::Workspace(WorkspaceTarget::Lettered('W')));
    }

    #[test]
    fn parse_action_simple() {
        assert_eq!(parse_action("equalize").unwrap(), Action::Equalize);
        assert_eq!(parse_action("close").unwrap(), Action::CloseWindow);
        assert_eq!(parse_action("toggle_float").unwrap(), Action::ToggleFloat);
        assert_eq!(parse_action("promote_stack").unwrap(), Action::PromoteStack);
    }

    #[test]
    fn format_action_round_trip() {
        let action = Action::MoveToWorkspace(WorkspaceTarget::Lettered('C'));
        assert_eq!(format_action(&action), "move_to_workspace C");
    }

    #[test]
    fn parse_structured_rule_with_modes_and_metadata() {
        let config = load_config_from_source(
            r#"
                gar.rule({
                    id = "rule_browser",
                    name = "Browser",
                    enabled = false,
                    match = {
                        app_name = { value = "Safari", mode = "exact" },
                        title = { value = "Profile .*", mode = "regex" },
                    },
                    actions = {
                        floating = true,
                        workspace = "special:web",
                        geometry = { x = 10, y = 20, width = 1100, height = 800 },
                    },
                })
            "#,
            "structured-rule",
        );

        let rule = config.rules.first().expect("structured rule missing");
        assert_eq!(rule.id.as_deref(), Some("rule_browser"));
        assert_eq!(rule.name.as_deref(), Some("Browser"));
        assert!(!rule.enabled);
        assert_eq!(
            rule.app_name,
            Some(RulePattern::new("Safari", RuleMatchMode::Exact))
        );
        assert_eq!(
            rule.title,
            Some(RulePattern::new("Profile .*", RuleMatchMode::Regex))
        );
        assert_eq!(rule.workspace.as_deref(), Some("special:web"));
        assert_eq!(rule.geometry, Some((10.0, 20.0, 1100.0, 800.0)));
    }

    #[test]
    fn parse_legacy_rule_infers_regex_mode() {
        let config = load_config_from_source(
            r#"gar.rule({ title = "/Preferences.*/" }, { floating = true })"#,
            "legacy-rule",
        );

        let rule = config.rules.first().expect("legacy rule missing");
        assert_eq!(
            rule.title,
            Some(RulePattern::new("Preferences.*", RuleMatchMode::Regex))
        );
        assert_eq!(rule.floating, Some(true));
    }

    #[test]
    fn rule_pattern_matches_respect_mode() {
        assert!(RulePattern::new("Safari", RuleMatchMode::Exact).matches("safari"));
        assert!(RulePattern::new("Saf", RuleMatchMode::Contains).matches("Safari"));
        assert!(RulePattern::new("^pro.*$", RuleMatchMode::Regex).matches("Profile"));
        assert!(!RulePattern::new("Safari", RuleMatchMode::Exact).matches("Safari Tech"));
    }

    #[test]
    fn default_keybinds_count() {
        let binds = default_keybinds(&Settings::default());
        // 4 basic + 12 focus + 12 swap + 4 resize + 20 workspaces = 52
        assert!(binds.len() >= 40);
    }
}
