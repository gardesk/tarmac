use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use super::lua::{
    LuaConfig, LuaKeybind, SpecialWorkspaceConfig, WindowRule, format_action, format_key_spec,
    load_config, load_config_from_source, lua_number, lua_string,
};
use super::settings::Settings;
use crate::core::input::{Key, Modifiers};
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

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRow {
    pub id: String,
    pub kind: WorkspaceKind,
    pub source: ConfigSource,
    pub editable: bool,
    pub definition: WorkspaceDefinition,
    pub special: Option<SpecialWorkspaceConfig>,
}

impl WorkspaceRow {
    pub fn source_label(&self) -> &'static str {
        self.source.label()
    }

    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            WorkspaceKind::Numbered => "Numbered",
            WorkspaceKind::Lettered => "Lettered",
            WorkspaceKind::Special => "Special",
        }
    }

    pub fn monitor_summary(&self) -> String {
        self.definition
            .prefs
            .monitor
            .as_ref()
            .map(|monitor| format!("Display {}", monitor.display_id))
            .unwrap_or_else(|| "No preference".to_string())
    }

    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "Layout {}",
            self.definition
                .prefs
                .default_layout
                .as_str()
                .to_ascii_uppercase()
        )];

        if self.definition.prefs.gap_inner.is_some() || self.definition.prefs.gap_outer.is_some() {
            let gap_inner = self
                .definition
                .prefs
                .gap_inner
                .map(lua_number)
                .unwrap_or_else(|| "default".to_string());
            let gap_outer = self
                .definition
                .prefs
                .gap_outer
                .map(lua_number)
                .unwrap_or_else(|| "default".to_string());
            parts.push(format!("Gaps {gap_inner}/{gap_outer}"));
        }

        if let Some(special) = &self.special {
            parts.push(format!(
                "{} {}×{}",
                special.position,
                lua_number(special.width),
                lua_number(special.height)
            ));
        }

        parts.join(" · ")
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

    pub fn resolved_keybind_rows(&self, mod_key: Modifiers) -> Vec<KeybindRow> {
        let mut external = self.external_keybind_rows(mod_key);
        // Always include defaults so they serve as a fallback layer.
        // When the managed block defines binds, load_config_from_source skips
        // appending defaults to the effective config, which means external_rows
        // can be empty. Without this, editing even one keybind via the settings
        // UI causes all other (default) keybinds to vanish after reload.
        let defaults = super::lua::default_keybinds(&self.effective.settings);
        external.extend(build_keybind_rows(
            &defaults,
            ConfigSource::Default,
            mod_key,
        ));
        resolve_keybind_rows(self.managed_keybind_rows(mod_key), external)
    }

    pub fn resolved_keybinds(&self, mod_key: Modifiers) -> Vec<LuaKeybind> {
        self.resolved_keybind_rows(mod_key)
            .into_iter()
            .map(|row| row.keybind)
            .collect()
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

    pub fn resolved_workspace_defs_and_specials(
        &self,
    ) -> (Vec<WorkspaceDefinition>, Vec<SpecialWorkspaceConfig>) {
        let managed_defs = self
            .managed
            .workspace_defs
            .iter()
            .cloned()
            .map(|def| (def.id.clone(), def))
            .collect::<std::collections::HashMap<_, _>>();
        let effective_defs = self
            .effective
            .workspace_defs
            .iter()
            .cloned()
            .map(|def| (def.id.clone(), def))
            .collect::<std::collections::HashMap<_, _>>();
        let managed_specials = self
            .managed
            .special_configs
            .iter()
            .cloned()
            .map(|special| (special.name.clone(), special))
            .collect::<std::collections::HashMap<_, _>>();
        let effective_specials = self
            .effective
            .special_configs
            .iter()
            .cloned()
            .map(|special| (special.name.clone(), special))
            .collect::<std::collections::HashMap<_, _>>();

        let mut defs = Vec::new();
        for default_def in super::lua::default_workspace_definitions() {
            let resolved = managed_defs
                .get(&default_def.id)
                .cloned()
                .or_else(|| effective_defs.get(&default_def.id).cloned())
                .unwrap_or(default_def);
            defs.push(resolved);
        }

        let mut extra_ids = Vec::new();
        let mut seen_extra_ids = HashSet::new();
        for id in managed_defs.keys().chain(effective_defs.keys()) {
            match id {
                WorkspaceId::Numbered(1..=10) => {}
                _ => {
                    if seen_extra_ids.insert(id.clone()) {
                        extra_ids.push(id.clone());
                    }
                }
            }
        }
        for name in managed_specials.keys().chain(effective_specials.keys()) {
            let id = WorkspaceId::Special(name.clone());
            if seen_extra_ids.insert(id.clone()) {
                extra_ids.push(id);
            }
        }

        extra_ids.sort_by_key(workspace_id_sort_key);
        for id in extra_ids {
            let resolved = managed_defs
                .get(&id)
                .cloned()
                .or_else(|| effective_defs.get(&id).cloned())
                .unwrap_or_else(|| WorkspaceDefinition::new(id));
            defs.push(resolved);
        }

        let mut special_names = BTreeSet::new();
        for def in &defs {
            if let WorkspaceId::Special(name) = &def.id {
                special_names.insert(name.clone());
            }
        }
        for name in managed_specials.keys().chain(effective_specials.keys()) {
            special_names.insert(name.clone());
        }

        let mut specials = special_names
            .into_iter()
            .filter_map(|name| {
                managed_specials
                    .get(&name)
                    .cloned()
                    .or_else(|| effective_specials.get(&name).cloned())
            })
            .collect::<Vec<_>>();
        specials.sort_by(|a, b| a.name.cmp(&b.name));

        (defs, specials)
    }

    pub fn workspace_rows(&self) -> Vec<WorkspaceRow> {
        let managed_defs = self
            .managed
            .workspace_defs
            .iter()
            .cloned()
            .map(|def| (def.id.clone(), def))
            .collect::<std::collections::HashMap<_, _>>();
        let effective_defs = self
            .effective
            .workspace_defs
            .iter()
            .cloned()
            .map(|def| (def.id.clone(), def))
            .collect::<std::collections::HashMap<_, _>>();
        let managed_specials = self
            .managed
            .special_configs
            .iter()
            .cloned()
            .map(|special| (special.name.clone(), special))
            .collect::<std::collections::HashMap<_, _>>();
        let effective_specials = self
            .effective
            .special_configs
            .iter()
            .cloned()
            .map(|special| (special.name.clone(), special))
            .collect::<std::collections::HashMap<_, _>>();
        let (resolved_defs, _) = self.resolved_workspace_defs_and_specials();

        let mut rows = resolved_defs
            .into_iter()
            .map(|definition| {
                let source = workspace_row_source(
                    &definition.id,
                    &definition,
                    &managed_defs,
                    &effective_defs,
                    &managed_specials,
                    &effective_specials,
                );
                let special = match &definition.id {
                    WorkspaceId::Special(name) => managed_specials
                        .get(name)
                        .cloned()
                        .or_else(|| effective_specials.get(name).cloned())
                        .or_else(|| Some(SpecialWorkspaceConfig::default_for(name))),
                    _ => None,
                };

                WorkspaceRow {
                    id: definition.id.to_string(),
                    kind: definition.kind,
                    source,
                    editable: source == ConfigSource::Managed,
                    definition,
                    special,
                }
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| workspace_id_sort_key(&row.definition.id));
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

fn keybind_signature(keybind: &LuaKeybind) -> (Modifiers, Key) {
    (keybind.modifiers, keybind.key)
}

fn dedupe_keybind_rows_keep_last(rows: Vec<KeybindRow>) -> Vec<KeybindRow> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for row in rows.into_iter().rev() {
        if seen.insert(keybind_signature(&row.keybind)) {
            deduped.push(row);
        }
    }

    deduped.reverse();
    deduped
}

fn resolve_keybind_rows(
    managed_rows: Vec<KeybindRow>,
    external_rows: Vec<KeybindRow>,
) -> Vec<KeybindRow> {
    let managed_rows = dedupe_keybind_rows_keep_last(managed_rows);
    let mut lua_rows = Vec::new();
    let mut default_rows = Vec::new();

    for row in external_rows {
        match row.source {
            ConfigSource::Lua => lua_rows.push(row),
            ConfigSource::Default => default_rows.push(row),
            ConfigSource::Managed => {}
        }
    }

    let lua_rows = dedupe_keybind_rows_keep_last(lua_rows);
    let default_rows = dedupe_keybind_rows_keep_last(default_rows);

    let mut seen = HashSet::new();
    let mut resolved = Vec::new();

    for row in managed_rows.into_iter().chain(lua_rows).chain(default_rows) {
        if seen.insert(keybind_signature(&row.keybind)) {
            resolved.push(row);
        }
    }

    resolved
}

fn workspace_row_source(
    id: &WorkspaceId,
    resolved: &WorkspaceDefinition,
    managed_defs: &std::collections::HashMap<WorkspaceId, WorkspaceDefinition>,
    effective_defs: &std::collections::HashMap<WorkspaceId, WorkspaceDefinition>,
    managed_specials: &std::collections::HashMap<String, SpecialWorkspaceConfig>,
    effective_specials: &std::collections::HashMap<String, SpecialWorkspaceConfig>,
) -> ConfigSource {
    if managed_defs.contains_key(id)
        || matches!(id, WorkspaceId::Special(name) if managed_specials.contains_key(name))
    {
        return ConfigSource::Managed;
    }

    match id {
        WorkspaceId::Numbered(1..=10) => {
            let default = WorkspaceDefinition::new(id.clone());
            if effective_defs.get(id).is_some_and(|def| def != &default) {
                ConfigSource::Lua
            } else {
                ConfigSource::Default
            }
        }
        WorkspaceId::Numbered(_) => ConfigSource::Lua,
        WorkspaceId::Special(name) => {
            if effective_defs.contains_key(id) || effective_specials.contains_key(name) {
                ConfigSource::Lua
            } else if resolved == &WorkspaceDefinition::new(id.clone()) {
                ConfigSource::Default
            } else {
                ConfigSource::Lua
            }
        }
        WorkspaceId::Lettered(_) => ConfigSource::Lua,
    }
}

fn workspace_id_sort_key(id: &WorkspaceId) -> (u8, String) {
    match id {
        WorkspaceId::Numbered(num) => (0, format!("{num:02}")),
        WorkspaceId::Lettered(ch) => (1, ch.to_string()),
        WorkspaceId::Special(name) => (2, name.clone()),
    }
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
    use crate::core::workspace::{WorkspaceLayout, WorkspaceTarget};

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
    fn resolved_keybind_rows_prefer_managed_then_lua_then_default() {
        let settings = Settings::default();
        let duplicate = LuaKeybind {
            modifiers: Modifiers::COMMAND,
            key: Key::Return,
            action: Action::SpawnTerminal,
        };
        let managed_override = LuaKeybind {
            modifiers: Modifiers::COMMAND,
            key: Key::Return,
            action: Action::Reload,
        };
        let default_only = super::super::lua::default_keybinds(&settings)[1].clone();
        let lua_only = LuaKeybind {
            modifiers: Modifiers::OPTION,
            key: Key::W,
            action: Action::Exit,
        };

        let doc = ManagedConfigDocument {
            path: PathBuf::new(),
            prefix: String::new(),
            suffix: String::new(),
            managed: ManagedConfig {
                settings: settings.clone(),
                keybinds: vec![managed_override.clone()],
                rules: Vec::new(),
                special_configs: Vec::new(),
                workspace_defs: Vec::new(),
            },
            effective: LuaConfig {
                settings,
                keybinds: vec![
                    managed_override,
                    duplicate,
                    default_only.clone(),
                    lua_only.clone(),
                ],
                rules: Vec::new(),
                special_configs: Vec::new(),
                workspace_defs: Vec::new(),
                lua: None,
                callbacks: Vec::new(),
            },
        };

        let rows = doc.resolved_keybind_rows(Modifiers::COMMAND);

        // Managed override takes priority
        assert_eq!(rows[0].source, ConfigSource::Managed);
        assert_eq!(rows[0].action, "reload");
        // Lua-only bind is preserved
        assert!(rows.iter().any(|row| row.keybind == lua_only));
        // Default-only bind is preserved
        assert!(rows.iter().any(|row| row.keybind == default_only));
        // Managed signature (Cmd+Return) is NOT duplicated by default/lua layer
        assert!(!rows.iter().any(|row| {
            row.source != ConfigSource::Managed
                && row.keybind.modifiers == Modifiers::COMMAND
                && row.keybind.key == Key::Return
        }));
        // All default keybinds are present as fallbacks
        let all_defaults = super::super::lua::default_keybinds(&doc.effective.settings);
        for default_kb in &all_defaults {
            let sig = keybind_signature(default_kb);
            assert!(
                rows.iter()
                    .any(|row| keybind_signature(&row.keybind) == sig),
                "missing default keybind: {:?}+{:?}",
                default_kb.modifiers,
                default_kb.key,
            );
        }
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

    #[test]
    fn workspace_rows_prefer_managed_numbered_override() {
        let managed_def = WorkspaceDefinition {
            id: WorkspaceId::Numbered(3),
            kind: WorkspaceKind::Numbered,
            prefs: crate::core::workspace::WorkspacePrefs {
                monitor: None,
                default_layout: WorkspaceLayout::Bsp,
                gap_inner: Some(20.0),
                gap_outer: None,
            },
        };
        let lua_def = WorkspaceDefinition {
            id: WorkspaceId::Numbered(3),
            kind: WorkspaceKind::Numbered,
            prefs: crate::core::workspace::WorkspacePrefs {
                monitor: None,
                default_layout: WorkspaceLayout::Bsp,
                gap_inner: Some(8.0),
                gap_outer: None,
            },
        };
        let doc = ManagedConfigDocument {
            path: PathBuf::new(),
            prefix: String::new(),
            suffix: String::new(),
            managed: ManagedConfig {
                settings: Settings::default(),
                keybinds: Vec::new(),
                rules: Vec::new(),
                special_configs: Vec::new(),
                workspace_defs: vec![managed_def.clone()],
            },
            effective: LuaConfig {
                settings: Settings::default(),
                keybinds: Vec::new(),
                rules: Vec::new(),
                special_configs: Vec::new(),
                workspace_defs: {
                    let mut defs = super::super::lua::default_workspace_definitions();
                    defs.retain(|def| def.id != WorkspaceId::Numbered(3));
                    defs.push(lua_def);
                    defs
                },
                lua: None,
                callbacks: Vec::new(),
            },
        };

        let rows = doc.workspace_rows();
        let row = rows
            .iter()
            .find(|row| row.id == "3")
            .expect("workspace 3 row missing");
        assert_eq!(row.source, ConfigSource::Managed);
        assert_eq!(row.definition, managed_def);
        assert_eq!(rows.iter().filter(|row| row.id == "3").count(), 1);
    }

    #[test]
    fn workspace_rows_merge_special_workspace_prefs_and_overlay() {
        let special_def = WorkspaceDefinition {
            id: WorkspaceId::Special("term".to_string()),
            kind: WorkspaceKind::Special,
            prefs: crate::core::workspace::WorkspacePrefs {
                monitor: Some(crate::core::workspace::MonitorAssignment { display_id: 42 }),
                default_layout: WorkspaceLayout::Bsp,
                gap_inner: Some(12.0),
                gap_outer: Some(18.0),
            },
        };
        let special_overlay = SpecialWorkspaceConfig {
            name: "term".to_string(),
            position: "top".to_string(),
            width: 0.8,
            height: 0.5,
        };
        let doc = ManagedConfigDocument {
            path: PathBuf::new(),
            prefix: String::new(),
            suffix: String::new(),
            managed: ManagedConfig {
                settings: Settings::default(),
                keybinds: Vec::new(),
                rules: Vec::new(),
                special_configs: vec![special_overlay.clone()],
                workspace_defs: vec![special_def.clone()],
            },
            effective: LuaConfig {
                settings: Settings::default(),
                keybinds: Vec::new(),
                rules: Vec::new(),
                special_configs: vec![special_overlay.clone()],
                workspace_defs: {
                    let mut defs = super::super::lua::default_workspace_definitions();
                    defs.push(special_def.clone());
                    defs
                },
                lua: None,
                callbacks: Vec::new(),
            },
        };

        let rows = doc.workspace_rows();
        let row = rows
            .iter()
            .find(|row| row.id == "special:term")
            .expect("special workspace row missing");
        assert_eq!(row.source, ConfigSource::Managed);
        assert_eq!(row.definition, special_def);
        assert_eq!(row.special.as_ref(), Some(&special_overlay));
    }

    #[test]
    fn resolved_workspace_defs_and_specials_prefer_managed_special_overrides() {
        let managed_special = SpecialWorkspaceConfig {
            name: "web".to_string(),
            position: "bottom".to_string(),
            width: 0.9,
            height: 0.4,
        };
        let lua_special = SpecialWorkspaceConfig {
            name: "web".to_string(),
            position: "center".to_string(),
            width: 0.7,
            height: 0.7,
        };
        let doc = ManagedConfigDocument {
            path: PathBuf::new(),
            prefix: String::new(),
            suffix: String::new(),
            managed: ManagedConfig {
                settings: Settings::default(),
                keybinds: Vec::new(),
                rules: Vec::new(),
                special_configs: vec![managed_special.clone()],
                workspace_defs: Vec::new(),
            },
            effective: LuaConfig {
                settings: Settings::default(),
                keybinds: Vec::new(),
                rules: Vec::new(),
                special_configs: vec![lua_special],
                workspace_defs: super::super::lua::default_workspace_definitions(),
                lua: None,
                callbacks: Vec::new(),
            },
        };

        let (_, specials) = doc.resolved_workspace_defs_and_specials();
        assert_eq!(specials, vec![managed_special]);
    }
}
