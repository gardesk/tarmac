use std::path::{Path, PathBuf};

use super::lua::{
    LuaConfig, LuaKeybind, SpecialWorkspaceConfig, WindowRule, format_action, format_key_spec,
    load_config, load_config_from_source, lua_number, lua_string,
};
use super::settings::Settings;
use crate::core::input::Modifiers;
use crate::core::workspace::{WorkspaceDefinition, WorkspaceId, WorkspaceKind};

pub const MANAGED_BEGIN: &str = "-- BEGIN TARMAC SETTINGS";
pub const MANAGED_END: &str = "-- END TARMAC SETTINGS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Managed,
    Lua,
    Default,
}

impl ConfigSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Lua => "lua",
            Self::Default => "default",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Managed => "Managed",
            Self::Lua => "Lua",
            Self::Default => "Default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindRow {
    pub id: String,
    pub shortcut: String,
    pub action: String,
    pub source: ConfigSource,
    pub editable: bool,
    pub keybind: LuaKeybind,
}

impl KeybindRow {
    pub fn source_label(&self) -> &'static str {
        self.source.label()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuleRow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub source: ConfigSource,
    pub editable: bool,
    pub rule: WindowRule,
}

impl RuleRow {
    pub fn when_summary(&self) -> String {
        super::lua::format_rule_match(&self.rule)
    }

    pub fn then_summary(&self) -> String {
        super::lua::format_rule_action(&self.rule)
    }

    pub fn source_label(&self) -> &'static str {
        self.source.label()
    }
}

#[derive(Debug, Clone)]
pub struct ManagedConfig {
    pub settings: Settings,
    pub keybinds: Vec<LuaKeybind>,
    pub rules: Vec<WindowRule>,
    pub special_configs: Vec<SpecialWorkspaceConfig>,
    pub workspace_defs: Vec<WorkspaceDefinition>,
}

impl ManagedConfig {
    pub fn from_effective(config: &LuaConfig) -> Self {
        Self {
            settings: config.settings.clone(),
            keybinds: Vec::new(),
            rules: Vec::new(),
            special_configs: Vec::new(),
            workspace_defs: Vec::new(),
        }
    }

    pub fn from_config(config: &LuaConfig) -> Self {
        Self {
            settings: config.settings.clone(),
            keybinds: config.keybinds.clone(),
            rules: config.rules.clone(),
            special_configs: config.special_configs.clone(),
            workspace_defs: config.workspace_defs.clone(),
        }
    }
}

pub struct ManagedConfigDocument {
    pub path: PathBuf,
    prefix: String,
    suffix: String,
    pub managed: ManagedConfig,
    pub effective: LuaConfig,
}

impl ManagedConfigDocument {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let effective = load_config(path);
        let (prefix, managed_source, suffix) = split_managed_block(&content);
        let managed = if let Some(managed_source) = managed_source {
            ManagedConfig::from_config(&load_config_from_source(
                &managed_source,
                &format!("{}#managed", path.display()),
            ))
        } else {
            ManagedConfig::from_effective(&effective)
        };

        Ok(Self {
            path: path.to_path_buf(),
            prefix,
            suffix,
            managed,
            effective,
        })
    }

    pub fn reload_effective(&mut self) {
        self.effective = load_config(&self.path);
    }

    pub fn write(&self) -> Result<(), String> {
        let mut output = String::new();
        output.push_str(&self.prefix);
        if !output.ends_with('\n') && !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&serialize_managed(&self.managed));
        if !self.suffix.is_empty() {
            if !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&self.suffix);
        }
        std::fs::write(&self.path, output).map_err(|e| e.to_string())
    }

    pub fn managed_rule_rows(&self) -> Vec<RuleRow> {
        build_rule_rows(&self.managed.rules, ConfigSource::Managed)
    }

    pub fn managed_keybind_rows(&self, mod_key: Modifiers) -> Vec<KeybindRow> {
        build_keybind_rows(&self.managed.keybinds, ConfigSource::Managed, mod_key)
    }

    pub fn external_keybind_rows(&self, mod_key: Modifiers) -> Vec<KeybindRow> {
        let defaults = super::lua::default_keybinds(&self.effective.settings);
        self.effective
            .keybinds
            .iter()
            .filter(|keybind| !self.managed.keybinds.contains(*keybind))
            .enumerate()
            .map(|(index, keybind)| {
                let source = if defaults.contains(keybind) {
                    ConfigSource::Default
                } else {
                    ConfigSource::Lua
                };
                KeybindRow {
                    id: format!("{}:{index}", source.as_str()),
                    shortcut: format_key_spec(keybind.modifiers, keybind.key, mod_key),
                    action: format_action(&keybind.action),
                    source,
                    editable: false,
                    keybind: keybind.clone(),
                }
            })
            .collect()
    }

    pub fn keybind_rows(&self, mod_key: Modifiers) -> Vec<KeybindRow> {
        let mut rows = self.managed_keybind_rows(mod_key);
        rows.extend(self.external_keybind_rows(mod_key));
        rows
    }

    pub fn external_rule_rows(&self) -> Vec<RuleRow> {
        let rules = self
            .effective
            .rules
            .iter()
            .filter(|rule| !self.managed.rules.contains(*rule))
            .cloned()
            .collect::<Vec<_>>();
        build_rule_rows(&rules, ConfigSource::Lua)
    }

    pub fn rule_rows(&self) -> Vec<RuleRow> {
        let mut rows = self.managed_rule_rows();
        rows.extend(self.external_rule_rows());
        rows
    }
}

fn build_rule_rows(rules: &[WindowRule], source: ConfigSource) -> Vec<RuleRow> {
    rules
        .iter()
        .enumerate()
        .map(|(index, rule)| RuleRow {
            id: rule.effective_id(index),
            name: rule.effective_name(index),
            enabled: rule.enabled,
            source,
            editable: source == ConfigSource::Managed,
            rule: rule.clone(),
        })
        .collect()
}

fn build_keybind_rows(
    keybinds: &[LuaKeybind],
    source: ConfigSource,
    mod_key: Modifiers,
) -> Vec<KeybindRow> {
    keybinds
        .iter()
        .enumerate()
        .map(|(index, keybind)| KeybindRow {
            id: format!("{}:{index}", source.as_str()),
            shortcut: format_key_spec(keybind.modifiers, keybind.key, mod_key),
            action: format_action(&keybind.action),
            source,
            editable: source == ConfigSource::Managed,
            keybind: keybind.clone(),
        })
        .collect()
}

fn split_managed_block(content: &str) -> (String, Option<String>, String) {
    let Some(begin) = content.find(MANAGED_BEGIN) else {
        return (content.to_string(), None, String::new());
    };
    let Some(end_relative) = content[begin..].find(MANAGED_END) else {
        return (content.to_string(), None, String::new());
    };
    let end = begin + end_relative;
    let after_end = content[end..]
        .find('\n')
        .map(|offset| end + offset + 1)
        .unwrap_or(content.len());

    let prefix = content[..begin].to_string();
    let managed = content[begin..after_end].to_string();
    let suffix = content[after_end..].to_string();
    (prefix, Some(managed), suffix)
}

pub fn serialize_managed(managed: &ManagedConfig) -> String {
    let mut out = String::new();
    out.push_str(MANAGED_BEGIN);
    out.push('\n');
    out.push_str("-- This block is managed by the tarmac Settings window.\n");
    out.push('\n');

    write_settings(&mut out, &managed.settings);
    write_workspace_defs(&mut out, &managed.workspace_defs);
    write_special_workspaces(&mut out, &managed.special_configs);
    write_keybinds(&mut out, &managed.keybinds, managed.settings.mod_key);
    write_rules(&mut out, &managed.rules);

    out.push_str(MANAGED_END);
    out.push('\n');
    out
}

pub fn serialize_rules(rules: &[WindowRule]) -> String {
    let mut out = String::new();
    write_rules(&mut out, rules);
    out
}

fn write_settings(out: &mut String, settings: &Settings) {
    out.push_str("-- General\n");
    out.push_str(&format!(
        "gar.set(\"mod_key\", {})\n",
        lua_string(match settings.mod_key.bits() {
            bits if bits == crate::core::input::Modifiers::OPTION.bits() => "option",
            bits if bits == crate::core::input::Modifiers::CONTROL.bits() => "control",
            _ => "command",
        })
    ));
    out.push_str(&format!(
        "gar.set(\"gap_inner\", {})\n",
        lua_number(settings.gap_inner)
    ));
    out.push_str(&format!(
        "gar.set(\"gap_outer\", {})\n",
        lua_number(settings.gap_outer)
    ));
    out.push_str(&format!(
        "gar.set(\"focus_follows_mouse\", {})\n",
        if settings.focus_follows_mouse {
            "true"
        } else {
            "false"
        }
    ));
    out.push_str(&format!(
        "gar.set(\"mouse_follows_focus\", {})\n",
        if settings.mouse_follows_focus {
            "true"
        } else {
            "false"
        }
    ));
    out.push_str(&format!(
        "gar.set(\"terminal\", {})\n",
        lua_string(&settings.terminal_command)
    ));
    out.push_str(&format!(
        "gar.set(\"bar_height\", {})\n",
        lua_number(settings.bar_height)
    ));
    out.push_str(&format!(
        "gar.set(\"border_width\", {})\n",
        lua_number(settings.border_width)
    ));
    out.push_str(&format!(
        "gar.set(\"border_color_focused\", {})\n",
        lua_string(&settings.border_color_focused)
    ));
    out.push_str(&format!(
        "gar.set(\"border_color_unfocused\", {})\n",
        lua_string(&settings.border_color_unfocused)
    ));
    out.push_str(&format!(
        "gar.set(\"border_radius\", {})\n\n",
        lua_number(settings.border_radius)
    ));
}

fn write_workspace_defs(out: &mut String, defs: &[WorkspaceDefinition]) {
    let mut defs = defs.to_vec();
    defs.sort_by(|a, b| a.id.to_string().cmp(&b.id.to_string()));

    let mut wrote_any = false;
    for def in defs {
        let is_default_numbered = matches!(def.id, WorkspaceId::Numbered(1..=10))
            && def.kind == WorkspaceKind::Numbered
            && def.prefs.monitor.is_none()
            && def.prefs.gap_inner.is_none()
            && def.prefs.gap_outer.is_none();
        if is_default_numbered {
            continue;
        }

        if !wrote_any {
            out.push_str("-- Workspaces\n");
            wrote_any = true;
        }

        let id_literal = match &def.id {
            WorkspaceId::Numbered(num) => num.to_string(),
            WorkspaceId::Lettered(ch) => lua_string(&ch.to_string()),
            WorkspaceId::Special(name) => lua_string(&format!("special:{name}")),
        };

        let mut parts = vec![format!(
            "layout = {}",
            lua_string(def.prefs.default_layout.as_str())
        )];
        if let Some(monitor) = &def.prefs.monitor {
            parts.push(format!("monitor = {}", monitor.display_id));
        }
        if let Some(gap_inner) = def.prefs.gap_inner {
            parts.push(format!("gap_inner = {}", lua_number(gap_inner)));
        }
        if let Some(gap_outer) = def.prefs.gap_outer {
            parts.push(format!("gap_outer = {}", lua_number(gap_outer)));
        }
        out.push_str(&format!(
            "gar.workspace({}, {{ {} }})\n",
            id_literal,
            parts.join(", ")
        ));
    }
    if wrote_any {
        out.push('\n');
    }
}

fn write_special_workspaces(out: &mut String, specials: &[SpecialWorkspaceConfig]) {
    if specials.is_empty() {
        return;
    }

    out.push_str("-- Special Workspaces\n");
    let mut specials = specials.to_vec();
    specials.sort_by(|a, b| a.name.cmp(&b.name));
    for special in specials {
        out.push_str(&format!(
            "gar.special_workspace({}, {{ position = {}, width = {}, height = {} }})\n",
            lua_string(&special.name),
            lua_string(&special.position),
            lua_number(special.width),
            lua_number(special.height)
        ));
    }
    out.push('\n');
}

fn write_keybinds(
    out: &mut String,
    keybinds: &[LuaKeybind],
    mod_key: crate::core::input::Modifiers,
) {
    if keybinds.is_empty() {
        return;
    }

    out.push_str("-- Keybindings\n");
    for keybind in keybinds {
        out.push_str(&format!(
            "gar.bind({}, {})\n",
            lua_string(&format_key_spec(keybind.modifiers, keybind.key, mod_key)),
            lua_string(&format_action(&keybind.action))
        ));
    }
    out.push('\n');
}

fn write_rules(out: &mut String, rules: &[WindowRule]) {
    if rules.is_empty() {
        return;
    }

    out.push_str("-- Rules\n");
    for (index, rule) in rules.iter().enumerate() {
        out.push_str("gar.rule({\n");
        out.push_str(&format!(
            "    id = {},\n",
            lua_string(&rule.effective_id(index))
        ));
        out.push_str(&format!(
            "    name = {},\n",
            lua_string(&rule.effective_name(index))
        ));
        out.push_str(&format!(
            "    enabled = {},\n",
            if rule.enabled { "true" } else { "false" }
        ));
        out.push_str("    match = {\n");
        write_rule_pattern(out, "app_name", rule.app_name.as_ref());
        write_rule_pattern(out, "app_bundle", rule.app_bundle.as_ref());
        write_rule_pattern(out, "title", rule.title.as_ref());
        out.push_str("    },\n");
        out.push_str("    actions = {\n");
        if let Some(floating) = rule.floating {
            out.push_str(&format!(
                "        floating = {},\n",
                if floating { "true" } else { "false" }
            ));
        }
        if let Some(workspace) = &rule.workspace {
            if let Ok(num) = workspace.parse::<u8>() {
                out.push_str(&format!("        workspace = {num},\n"));
            } else {
                out.push_str(&format!("        workspace = {},\n", lua_string(workspace)));
            }
        }
        if let Some((x, y, width, height)) = rule.geometry {
            out.push_str(&format!(
                "        geometry = {{ x = {}, y = {}, width = {}, height = {} }},\n",
                lua_number(x),
                lua_number(y),
                lua_number(width),
                lua_number(height)
            ));
        }
        out.push_str("    },\n");
        out.push_str("})\n");
    }
    out.push('\n');
}

fn write_rule_pattern(
    out: &mut String,
    key: &str,
    pattern: Option<&crate::config::lua::RulePattern>,
) {
    let Some(pattern) = pattern else { return };
    out.push_str(&format!(
        "        {} = {{ value = {}, mode = {} }},\n",
        key,
        lua_string(&pattern.value),
        lua_string(pattern.mode.as_str())
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::lua::{RuleMatchMode, RulePattern};
    use crate::core::input::{Action, Key, Modifiers};
    use crate::core::workspace::WorkspaceTarget;

    #[test]
    fn splits_and_rewrites_managed_block() {
        let content =
            "print('hi')\n-- BEGIN TARMAC SETTINGS\nold\n-- END TARMAC SETTINGS\nprint('bye')\n";
        let (prefix, managed, suffix) = split_managed_block(content);
        assert_eq!(prefix, "print('hi')\n");
        assert!(managed.unwrap().contains("old"));
        assert_eq!(suffix, "print('bye')\n");
    }

    #[test]
    fn serializes_keybinds_and_workspaces() {
        let mut managed = ManagedConfig {
            settings: Settings::default(),
            keybinds: vec![LuaKeybind {
                modifiers: Modifiers::COMMAND,
                key: Key::W,
                action: Action::Workspace(WorkspaceTarget::Lettered('W')),
            }],
            rules: Vec::new(),
            special_configs: Vec::new(),
            workspace_defs: vec![WorkspaceDefinition {
                id: WorkspaceId::Lettered('W'),
                kind: WorkspaceKind::Lettered,
                prefs: Default::default(),
            }],
        };
        managed.settings.mod_key = Modifiers::COMMAND;
        let text = serialize_managed(&managed);
        assert!(text.contains("gar.bind(\"mod+w\", \"workspace W\")"));
        assert!(text.contains("gar.workspace(\"W\","));
    }

    #[test]
    fn serializes_structured_rules() {
        let managed = ManagedConfig {
            settings: Settings::default(),
            keybinds: Vec::new(),
            rules: vec![WindowRule {
                id: Some("rule_browser".to_string()),
                name: Some("Browser".to_string()),
                enabled: false,
                app_name: Some(RulePattern::new("Safari", RuleMatchMode::Exact)),
                app_bundle: None,
                title: Some(RulePattern::new("Profile .*", RuleMatchMode::Regex)),
                floating: Some(true),
                workspace: Some("special:web".to_string()),
                geometry: Some((10.0, 20.0, 1100.0, 800.0)),
            }],
            special_configs: Vec::new(),
            workspace_defs: Vec::new(),
        };

        let text = serialize_managed(&managed);
        assert!(text.contains("id = \"rule_browser\""));
        assert!(text.contains("name = \"Browser\""));
        assert!(text.contains("enabled = false"));
        assert!(text.contains("app_name = { value = \"Safari\", mode = \"exact\" }"));
        assert!(text.contains("title = { value = \"Profile .*\", mode = \"regex\" }"));
        assert!(text.contains("workspace = \"special:web\""));
    }

    #[test]
    fn keybind_rows_preserve_sources_and_formatting() {
        let settings = Settings::default();
        let managed_keybind = LuaKeybind {
            modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
            key: Key::Period,
            action: Action::Reload,
        };
        let default_keybind = super::super::lua::default_keybinds(&settings)[0].clone();
        let lua_keybind = LuaKeybind {
            modifiers: Modifiers::OPTION | Modifiers::SHIFT,
            key: Key::Period,
            action: Action::Exit,
        };
        let doc = ManagedConfigDocument {
            path: PathBuf::new(),
            prefix: String::new(),
            suffix: String::new(),
            managed: ManagedConfig {
                settings: settings.clone(),
                keybinds: vec![managed_keybind.clone()],
                rules: Vec::new(),
                special_configs: Vec::new(),
                workspace_defs: Vec::new(),
            },
            effective: LuaConfig {
                settings,
                keybinds: vec![managed_keybind, default_keybind, lua_keybind],
                rules: Vec::new(),
                special_configs: Vec::new(),
                workspace_defs: Vec::new(),
                lua: None,
                callbacks: Vec::new(),
            },
        };

        let rows = doc.keybind_rows(Modifiers::COMMAND);

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].source, ConfigSource::Managed);
        assert_eq!(rows[0].shortcut, "mod+shift+period");
        assert_eq!(rows[0].action, "reload");
        assert_eq!(rows[1].source, ConfigSource::Default);
        assert_eq!(rows[2].source, ConfigSource::Lua);
        assert!(!rows[2].editable);
    }

    #[test]
    fn serialize_rules_round_trips_through_lua_loader() {
        let rules = vec![WindowRule {
            id: Some("rule_browser".to_string()),
            name: Some("Browser".to_string()),
            enabled: false,
            app_name: Some(RulePattern::new("Safari", RuleMatchMode::Exact)),
            app_bundle: None,
            title: Some(RulePattern::new("Profile .*", RuleMatchMode::Regex)),
            floating: Some(true),
            workspace: Some("special:web".to_string()),
            geometry: Some((10.0, 20.0, 1100.0, 800.0)),
        }];

        let serialized = serialize_rules(&rules);
        let parsed = load_config_from_source(&serialized, "rules-roundtrip").rules;
        assert_eq!(parsed, rules);
    }
}
