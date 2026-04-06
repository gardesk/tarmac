use std::cell::RefCell;
use std::sync::mpsc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColorWell, NSImage, NSPopUpButton, NSScrollView, NSSlider,
    NSSplitView, NSSplitViewDividerStyle, NSTabViewController, NSTabViewControllerTabStyle,
    NSTabViewItem, NSTableColumn, NSTableView, NSTableViewRowSizeStyle, NSTableViewStyle,
    NSTextField, NSView, NSViewController, NSWindow, NSWindowStyleMask, NSWindowToolbarStyle,
};
use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
use objc2_foundation::{NSIndexSet, NSInteger, NSNotification, NSObject, NSString};

use crate::config::document::{KeybindRow, RuleRow, WorkspaceRow};
use crate::config::lua::{RuleMatchMode, RulePattern, SpecialWorkspaceConfig, WindowRule};
use crate::core::workspace::{
    MonitorAssignment, WorkspaceDefinition, WorkspaceKind, WorkspaceLayout,
};

const WIN_W: f64 = 920.0;
const WIN_H: f64 = 660.0;

const RULE_COLUMN_ENABLED: &str = "enabled";
const RULE_COLUMN_NAME: &str = "name";
const RULE_COLUMN_WHEN: &str = "when";
const RULE_COLUMN_THEN: &str = "then";
const RULE_COLUMN_SOURCE: &str = "source";
const KEYBIND_COLUMN_SHORTCUT: &str = "shortcut";
const KEYBIND_COLUMN_ACTION: &str = "action";
const KEYBIND_COLUMN_SOURCE: &str = "source";
const WORKSPACE_COLUMN_ID: &str = "id";
const WORKSPACE_COLUMN_KIND: &str = "kind";
const WORKSPACE_COLUMN_MONITOR: &str = "monitor";
const WORKSPACE_COLUMN_SUMMARY: &str = "summary";
const WORKSPACE_COLUMN_SOURCE: &str = "source";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindDraft {
    pub shortcut: String,
    pub action: String,
}

impl KeybindDraft {
    fn from_row(row: &KeybindRow) -> Self {
        Self {
            shortcut: row.shortcut.clone(),
            action: row.action.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDisplayOption {
    pub display_id: u32,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceDraft {
    pub id: String,
    pub kind: WorkspaceKind,
    pub monitor_display_id: Option<u32>,
    pub layout: WorkspaceLayout,
    pub gap_inner: Option<f64>,
    pub gap_outer: Option<f64>,
    pub overlay_position: String,
    pub overlay_width: f64,
    pub overlay_height: f64,
}

impl WorkspaceDraft {
    fn from_row(row: &WorkspaceRow) -> Self {
        let special = row
            .special
            .clone()
            .unwrap_or_else(|| SpecialWorkspaceConfig::default_for(&row.id));
        Self {
            id: row.id.clone(),
            kind: row.kind,
            monitor_display_id: row.definition.prefs.monitor.as_ref().map(|m| m.display_id),
            layout: row.definition.prefs.default_layout,
            gap_inner: row.definition.prefs.gap_inner,
            gap_outer: row.definition.prefs.gap_outer,
            overlay_position: special.position,
            overlay_width: special.width,
            overlay_height: special.height,
        }
    }

    pub fn to_definition(&self) -> WorkspaceDefinition {
        let mut definition = WorkspaceDefinition::new(
            crate::core::workspace::WorkspaceId::parse(&self.id)
                .unwrap_or_else(|| crate::core::workspace::WorkspaceId::Special(self.id.clone())),
        );
        definition.kind = self.kind;
        definition.prefs.monitor = self
            .monitor_display_id
            .map(|display_id| MonitorAssignment { display_id });
        definition.prefs.default_layout = self.layout;
        definition.prefs.gap_inner = self.gap_inner;
        definition.prefs.gap_outer = self.gap_outer;
        definition
    }

    pub fn special_config(&self) -> Option<SpecialWorkspaceConfig> {
        if self.kind != WorkspaceKind::Special {
            return None;
        }
        Some(SpecialWorkspaceConfig {
            name: self
                .id
                .strip_prefix("special:")
                .unwrap_or(&self.id)
                .to_string(),
            position: self.overlay_position.clone(),
            width: self.overlay_width,
            height: self.overlay_height,
        })
    }
}

#[derive(Debug)]
pub enum SettingsAction {
    GapInner(f64),
    GapOuter(f64),
    BarHeight(f64),
    BorderWidth(f64),
    BorderRadius(f64),
    BorderColorFocused(String),
    BorderColorUnfocused(String),
    FocusFollowsMouse(bool),
    MouseFollowsFocus(bool),
    ModKey(String),
    SelectKeybind(String),
    AddKeybind,
    DeleteKeybind(String),
    CopyKeybindToManaged(String),
    UpdateKeybindDraft(KeybindDraft),
    ApplyKeybind(String),
    ResetManagedKeybinds,
    SelectRule(String),
    AddRule,
    DuplicateRule(String),
    DeleteRule(String),
    MoveRuleUp(String),
    MoveRuleDown(String),
    UpdateRuleDraft(WindowRule),
    ApplyRule(String),
    ToggleRuleEnabled(String, bool),
    SelectWorkspace(String),
    AddLetteredWorkspace(String),
    AddSpecialWorkspace(String),
    CopyWorkspaceToManaged(String),
    DeleteWorkspace(String),
    UpdateWorkspaceDraft(WorkspaceDraft),
    ApplyWorkspace(String),
}

pub struct SettingsSnapshot {
    pub gap_inner: f64,
    pub gap_outer: f64,
    pub bar_height: f64,
    pub border_width: f64,
    pub border_radius: f64,
    pub border_color_focused: String,
    pub border_color_unfocused: String,
    pub focus_follows_mouse: bool,
    pub mouse_follows_focus: bool,
    pub mod_key: String,
    pub keybinds: Vec<KeybindRow>,
    pub selected_keybind_id: Option<String>,
    pub rules: Vec<RuleRow>,
    pub selected_rule_id: Option<String>,
    pub workspaces: Vec<WorkspaceRow>,
    pub selected_workspace_id: Option<String>,
    pub displays: Vec<WorkspaceDisplayOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeybindInspectorState {
    editable: bool,
    source_label: String,
    can_delete: bool,
    can_copy_to_managed: bool,
    can_apply: bool,
    draft: Option<KeybindDraft>,
}

#[derive(Debug, Clone, PartialEq)]
struct RuleInspectorState {
    selected_rule_id: Option<String>,
    editable: bool,
    source_label: String,
    can_duplicate: bool,
    can_delete: bool,
    can_move_up: bool,
    can_move_down: bool,
    can_apply: bool,
    geometry_enabled: bool,
    rule: Option<WindowRule>,
}

#[derive(Debug, Clone, PartialEq)]
struct WorkspaceInspectorState {
    editable: bool,
    source_label: String,
    kind_label: String,
    can_copy_to_managed: bool,
    can_delete: bool,
    delete_label: String,
    can_apply: bool,
    is_special: bool,
    draft: Option<WorkspaceDraft>,
}

struct KeybindingsUiRefs {
    table: Retained<NSTableView>,
    placeholder_label: Retained<NSTextField>,
    source_value: Retained<NSTextField>,
    shortcut_field: Retained<NSTextField>,
    action_field: Retained<NSTextField>,
    delete_button: Retained<NSButton>,
    copy_button: Retained<NSButton>,
    apply_button: Retained<NSButton>,
}

struct RulesUiRefs {
    table: Retained<NSTableView>,
    placeholder_label: Retained<NSTextField>,
    source_value: Retained<NSTextField>,
    name_field: Retained<NSTextField>,
    enabled_checkbox: Retained<NSButton>,
    app_name_field: Retained<NSTextField>,
    app_name_mode_popup: Retained<NSPopUpButton>,
    bundle_id_field: Retained<NSTextField>,
    bundle_id_mode_popup: Retained<NSPopUpButton>,
    title_field: Retained<NSTextField>,
    title_mode_popup: Retained<NSPopUpButton>,
    floating_checkbox: Retained<NSButton>,
    workspace_field: Retained<NSTextField>,
    custom_geometry_checkbox: Retained<NSButton>,
    geometry_x_field: Retained<NSTextField>,
    geometry_y_field: Retained<NSTextField>,
    geometry_width_field: Retained<NSTextField>,
    geometry_height_field: Retained<NSTextField>,
    duplicate_button: Retained<NSButton>,
    delete_button: Retained<NSButton>,
    move_up_button: Retained<NSButton>,
    move_down_button: Retained<NSButton>,
    apply_button: Retained<NSButton>,
}

struct WorkspacesUiRefs {
    table: Retained<NSTableView>,
    placeholder_label: Retained<NSTextField>,
    source_value: Retained<NSTextField>,
    kind_value: Retained<NSTextField>,
    workspace_id_value: Retained<NSTextField>,
    monitor_popup: Retained<NSPopUpButton>,
    layout_popup: Retained<NSPopUpButton>,
    gap_inner_field: Retained<NSTextField>,
    gap_outer_field: Retained<NSTextField>,
    overlay_position_popup: Retained<NSPopUpButton>,
    overlay_width_field: Retained<NSTextField>,
    overlay_height_field: Retained<NSTextField>,
    overlay_section_label: Retained<NSTextField>,
    copy_button: Retained<NSButton>,
    delete_button: Retained<NSButton>,
    apply_button: Retained<NSButton>,
}

struct SettingsHandlerIvars {
    tx: mpsc::Sender<SettingsAction>,
    keybind_rows: RefCell<Vec<KeybindRow>>,
    selected_keybind_id: RefCell<Option<String>>,
    draft_keybind: RefCell<Option<KeybindDraft>>,
    suppress_keybind_selection_change: RefCell<bool>,
    keybindings_ui: RefCell<Option<KeybindingsUiRefs>>,
    rule_rows: RefCell<Vec<RuleRow>>,
    selected_rule_id: RefCell<Option<String>>,
    draft_rule: RefCell<Option<WindowRule>>,
    suppress_rule_selection_change: RefCell<bool>,
    rules_ui: RefCell<Option<RulesUiRefs>>,
    workspace_rows: RefCell<Vec<WorkspaceRow>>,
    selected_workspace_id: RefCell<Option<String>>,
    draft_workspace: RefCell<Option<WorkspaceDraft>>,
    workspace_displays: RefCell<Vec<WorkspaceDisplayOption>>,
    suppress_workspace_selection_change: RefCell<bool>,
    workspaces_ui: RefCell<Option<WorkspacesUiRefs>>,
}

impl SettingsHandler {
    fn new(mtm: MainThreadMarker, tx: mpsc::Sender<SettingsAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(SettingsHandlerIvars {
            tx,
            keybind_rows: RefCell::new(Vec::new()),
            selected_keybind_id: RefCell::new(None),
            draft_keybind: RefCell::new(None),
            suppress_keybind_selection_change: RefCell::new(false),
            keybindings_ui: RefCell::new(None),
            rule_rows: RefCell::new(Vec::new()),
            selected_rule_id: RefCell::new(None),
            draft_rule: RefCell::new(None),
            suppress_rule_selection_change: RefCell::new(false),
            rules_ui: RefCell::new(None),
            workspace_rows: RefCell::new(Vec::new()),
            selected_workspace_id: RefCell::new(None),
            draft_workspace: RefCell::new(None),
            workspace_displays: RefCell::new(Vec::new()),
            suppress_workspace_selection_change: RefCell::new(false),
            workspaces_ui: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, action: SettingsAction) {
        let _ = self.ivars().tx.send(action);
    }

    fn set_keybindings_ui(&self, ui: KeybindingsUiRefs) {
        *self.ivars().keybindings_ui.borrow_mut() = Some(ui);
    }

    fn set_rules_ui(&self, ui: RulesUiRefs) {
        *self.ivars().rules_ui.borrow_mut() = Some(ui);
    }

    fn set_workspaces_ui(&self, ui: WorkspacesUiRefs) {
        *self.ivars().workspaces_ui.borrow_mut() = Some(ui);
    }

    fn load_keybinds(&self, rows: Vec<KeybindRow>, selected_keybind_id: Option<String>) {
        let resolved_selection = if let Some(selected) = selected_keybind_id {
            rows.iter().find(|row| row.id == selected).map(|_| selected)
        } else {
            None
        };
        let draft = resolved_selection
            .as_deref()
            .and_then(|id| rows.iter().find(|row| row.id == id))
            .and_then(|row| row.editable.then(|| KeybindDraft::from_row(row)));

        *self.ivars().keybind_rows.borrow_mut() = rows;
        *self.ivars().selected_keybind_id.borrow_mut() = resolved_selection;
        *self.ivars().draft_keybind.borrow_mut() = draft;

        self.reload_keybind_table();
        self.refresh_keybind_inspector();
    }

    fn load_rules(&self, rows: Vec<RuleRow>, selected_rule_id: Option<String>) {
        let resolved_selection = if let Some(selected) = selected_rule_id {
            rows.iter().find(|row| row.id == selected).map(|_| selected)
        } else {
            None
        };
        let draft = resolved_selection
            .as_deref()
            .and_then(|id| rows.iter().find(|row| row.id == id))
            .and_then(|row| row.editable.then(|| row.rule.clone()));

        *self.ivars().rule_rows.borrow_mut() = rows;
        *self.ivars().selected_rule_id.borrow_mut() = resolved_selection;
        *self.ivars().draft_rule.borrow_mut() = draft;

        self.reload_rule_table();
        self.refresh_rule_inspector();
    }

    fn load_workspaces(
        &self,
        rows: Vec<WorkspaceRow>,
        selected_workspace_id: Option<String>,
        displays: Vec<WorkspaceDisplayOption>,
    ) {
        let resolved_selection = if let Some(selected) = selected_workspace_id {
            rows.iter().find(|row| row.id == selected).map(|_| selected)
        } else {
            None
        };
        let draft = resolved_selection
            .as_deref()
            .and_then(|id| rows.iter().find(|row| row.id == id))
            .and_then(|row| row.editable.then(|| WorkspaceDraft::from_row(row)));

        *self.ivars().workspace_rows.borrow_mut() = rows;
        *self.ivars().selected_workspace_id.borrow_mut() = resolved_selection;
        *self.ivars().draft_workspace.borrow_mut() = draft;
        *self.ivars().workspace_displays.borrow_mut() = displays;

        self.reload_workspace_table();
        self.refresh_workspace_inspector();
    }

    fn selected_keybind_row(&self) -> Option<KeybindRow> {
        let selected = self.ivars().selected_keybind_id.borrow().clone()?;
        self.ivars()
            .keybind_rows
            .borrow()
            .iter()
            .find(|row| row.id == selected)
            .cloned()
    }

    fn selected_keybind_row_index(&self) -> Option<usize> {
        let selected = self.ivars().selected_keybind_id.borrow().clone()?;
        self.ivars()
            .keybind_rows
            .borrow()
            .iter()
            .position(|row| row.id == selected)
    }

    fn selected_rule_row(&self) -> Option<RuleRow> {
        let selected = self.ivars().selected_rule_id.borrow().clone()?;
        self.ivars()
            .rule_rows
            .borrow()
            .iter()
            .find(|row| row.id == selected)
            .cloned()
    }

    fn selected_rule_row_index(&self) -> Option<usize> {
        let selected = self.ivars().selected_rule_id.borrow().clone()?;
        self.ivars()
            .rule_rows
            .borrow()
            .iter()
            .position(|row| row.id == selected)
    }

    fn selected_workspace_row(&self) -> Option<WorkspaceRow> {
        let selected = self.ivars().selected_workspace_id.borrow().clone()?;
        self.ivars()
            .workspace_rows
            .borrow()
            .iter()
            .find(|row| row.id == selected)
            .cloned()
    }

    fn selected_workspace_row_index(&self) -> Option<usize> {
        let selected = self.ivars().selected_workspace_id.borrow().clone()?;
        self.ivars()
            .workspace_rows
            .borrow()
            .iter()
            .position(|row| row.id == selected)
    }

    fn current_keybind_inspector_state(&self) -> KeybindInspectorState {
        let rows = self.ivars().keybind_rows.borrow().clone();
        let selected_keybind_id = self.ivars().selected_keybind_id.borrow().clone();
        let draft_keybind = self.ivars().draft_keybind.borrow().clone();
        derive_keybind_inspector_state(
            &rows,
            selected_keybind_id.as_deref(),
            draft_keybind.as_ref(),
        )
    }

    fn current_rule_inspector_state(&self) -> RuleInspectorState {
        let rows = self.ivars().rule_rows.borrow().clone();
        let selected_rule_id = self.ivars().selected_rule_id.borrow().clone();
        let draft_rule = self.ivars().draft_rule.borrow().clone();
        derive_rule_inspector_state(&rows, selected_rule_id.as_deref(), draft_rule.as_ref())
    }

    fn current_workspace_inspector_state(&self) -> WorkspaceInspectorState {
        let rows = self.ivars().workspace_rows.borrow().clone();
        let selected_workspace_id = self.ivars().selected_workspace_id.borrow().clone();
        let draft_workspace = self.ivars().draft_workspace.borrow().clone();
        derive_workspace_inspector_state(
            &rows,
            selected_workspace_id.as_deref(),
            draft_workspace.as_ref(),
        )
    }

    fn reload_keybind_table(&self) {
        let selected_index = self.selected_keybind_row_index();
        let ui_borrow = self.ivars().keybindings_ui.borrow();
        let Some(ui) = ui_borrow.as_ref() else { return };
        ui.table.reloadData();

        *self.ivars().suppress_keybind_selection_change.borrow_mut() = true;
        if let Some(index) = selected_index {
            let indexes = NSIndexSet::indexSetWithIndex(index);
            ui.table
                .selectRowIndexes_byExtendingSelection(&indexes, false);
        } else {
            let empty = NSIndexSet::indexSet();
            ui.table
                .selectRowIndexes_byExtendingSelection(&empty, false);
        }
        *self.ivars().suppress_keybind_selection_change.borrow_mut() = false;
    }

    fn reload_rule_table(&self) {
        let selected_index = self.selected_rule_row_index();
        let ui_borrow = self.ivars().rules_ui.borrow();
        let Some(ui) = ui_borrow.as_ref() else { return };
        ui.table.reloadData();

        *self.ivars().suppress_rule_selection_change.borrow_mut() = true;
        if let Some(index) = selected_index {
            let indexes = NSIndexSet::indexSetWithIndex(index);
            ui.table
                .selectRowIndexes_byExtendingSelection(&indexes, false);
        } else {
            let empty = NSIndexSet::indexSet();
            ui.table
                .selectRowIndexes_byExtendingSelection(&empty, false);
        }
        *self.ivars().suppress_rule_selection_change.borrow_mut() = false;
    }

    fn reload_workspace_table(&self) {
        let selected_index = self.selected_workspace_row_index();
        let ui_borrow = self.ivars().workspaces_ui.borrow();
        let Some(ui) = ui_borrow.as_ref() else { return };
        ui.table.reloadData();

        *self
            .ivars()
            .suppress_workspace_selection_change
            .borrow_mut() = true;
        if let Some(index) = selected_index {
            let indexes = NSIndexSet::indexSetWithIndex(index);
            ui.table
                .selectRowIndexes_byExtendingSelection(&indexes, false);
        } else {
            let empty = NSIndexSet::indexSet();
            ui.table
                .selectRowIndexes_byExtendingSelection(&empty, false);
        }
        *self
            .ivars()
            .suppress_workspace_selection_change
            .borrow_mut() = false;
    }

    fn refresh_keybind_inspector(&self) {
        let state = self.current_keybind_inspector_state();
        let ui_borrow = self.ivars().keybindings_ui.borrow();
        let Some(ui) = ui_borrow.as_ref() else { return };
        apply_keybind_inspector_state(ui, &state);
    }

    fn refresh_rule_inspector(&self) {
        let state = self.current_rule_inspector_state();
        let ui_borrow = self.ivars().rules_ui.borrow();
        let Some(ui) = ui_borrow.as_ref() else { return };
        apply_rule_inspector_state(ui, &state);
    }

    fn refresh_workspace_inspector(&self) {
        let state = self.current_workspace_inspector_state();
        let displays = self.ivars().workspace_displays.borrow().clone();
        let ui_borrow = self.ivars().workspaces_ui.borrow();
        let Some(ui) = ui_borrow.as_ref() else { return };
        apply_workspace_inspector_state(ui, &state, &displays);
    }

    fn select_keybind_by_row_index(&self, row_index: usize, emit_action: bool) {
        let Some(row) = self.ivars().keybind_rows.borrow().get(row_index).cloned() else {
            return;
        };
        *self.ivars().selected_keybind_id.borrow_mut() = Some(row.id.clone());
        *self.ivars().draft_keybind.borrow_mut() =
            row.editable.then(|| KeybindDraft::from_row(&row));
        self.refresh_keybind_inspector();
        if emit_action {
            self.emit(SettingsAction::SelectKeybind(row.id));
        }
    }

    fn select_rule_by_row_index(&self, row_index: usize, emit_action: bool) {
        let Some(row) = self.ivars().rule_rows.borrow().get(row_index).cloned() else {
            return;
        };
        *self.ivars().selected_rule_id.borrow_mut() = Some(row.id.clone());
        *self.ivars().draft_rule.borrow_mut() = row.editable.then_some(row.rule.clone());
        self.refresh_rule_inspector();
        if emit_action {
            self.emit(SettingsAction::SelectRule(row.id));
        }
    }

    fn select_workspace_by_row_index(&self, row_index: usize, emit_action: bool) {
        let Some(row) = self.ivars().workspace_rows.borrow().get(row_index).cloned() else {
            return;
        };
        *self.ivars().selected_workspace_id.borrow_mut() = Some(row.id.clone());
        *self.ivars().draft_workspace.borrow_mut() =
            row.editable.then(|| WorkspaceDraft::from_row(&row));
        self.refresh_workspace_inspector();
        if emit_action {
            self.emit(SettingsAction::SelectWorkspace(row.id));
        }
    }

    fn sync_keybind_draft_from_controls(&self) -> Option<KeybindDraft> {
        let row = self.selected_keybind_row()?;
        if !row.editable {
            return None;
        }
        let ui_borrow = self.ivars().keybindings_ui.borrow();
        let ui = ui_borrow.as_ref()?;

        let draft = KeybindDraft {
            shortcut: ui
                .shortcut_field
                .stringValue()
                .to_string()
                .trim()
                .to_string(),
            action: ui.action_field.stringValue().to_string().trim().to_string(),
        };
        *self.ivars().draft_keybind.borrow_mut() = Some(draft.clone());
        Some(draft)
    }

    fn emit_current_keybind_draft(&self) {
        if let Some(draft) = self.sync_keybind_draft_from_controls() {
            self.emit(SettingsAction::UpdateKeybindDraft(draft));
            self.refresh_keybind_inspector();
        }
    }

    fn toggle_selected_rule_enabled(&self, enabled: bool) {
        let Some(selected_id) = self.ivars().selected_rule_id.borrow().clone() else {
            return;
        };
        {
            let mut rows = self.ivars().rule_rows.borrow_mut();
            if let Some(row) = rows
                .iter_mut()
                .find(|row| row.id == selected_id && row.editable)
            {
                row.enabled = enabled;
                row.rule.enabled = enabled;
            } else {
                return;
            }
        }
        if let Some(draft) = self.ivars().draft_rule.borrow_mut().as_mut() {
            draft.enabled = enabled;
        }
        self.refresh_rule_inspector();
        self.reload_rule_table();
        self.emit(SettingsAction::ToggleRuleEnabled(selected_id, enabled));
    }

    fn toggle_row_enabled(&self, row_index: usize, enabled: bool) {
        let Some(row) = self.ivars().rule_rows.borrow().get(row_index).cloned() else {
            return;
        };
        if !row.editable {
            return;
        }
        {
            let mut rows = self.ivars().rule_rows.borrow_mut();
            if let Some(existing) = rows.get_mut(row_index) {
                existing.enabled = enabled;
                existing.rule.enabled = enabled;
            }
        }
        if self
            .ivars()
            .selected_rule_id
            .borrow()
            .as_deref()
            .is_some_and(|selected| selected == row.id)
        {
            if let Some(draft) = self.ivars().draft_rule.borrow_mut().as_mut() {
                draft.enabled = enabled;
            }
            self.refresh_rule_inspector();
        }
        self.emit(SettingsAction::ToggleRuleEnabled(row.id, enabled));
    }

    fn sync_rule_draft_from_controls(&self) -> Option<WindowRule> {
        let row = self.selected_rule_row()?;
        if !row.editable {
            return None;
        }
        let ui_borrow = self.ivars().rules_ui.borrow();
        let ui = ui_borrow.as_ref()?;

        let mut draft = self
            .ivars()
            .draft_rule
            .borrow()
            .clone()
            .unwrap_or_else(|| row.rule.clone());
        draft.id = Some(row.id.clone());
        draft.enabled = ui.enabled_checkbox.state() == 1;
        draft.name = trimmed_optional_text(&ui.name_field);
        draft.app_name = read_rule_pattern(
            &ui.app_name_field,
            &ui.app_name_mode_popup,
            RuleMatchMode::Contains,
        );
        draft.app_bundle = read_rule_pattern(
            &ui.bundle_id_field,
            &ui.bundle_id_mode_popup,
            RuleMatchMode::Contains,
        );
        draft.title = read_rule_pattern(
            &ui.title_field,
            &ui.title_mode_popup,
            RuleMatchMode::Contains,
        );
        draft.floating = (ui.floating_checkbox.state() == 1).then_some(true);
        draft.workspace = trimmed_optional_text(&ui.workspace_field);
        if ui.custom_geometry_checkbox.state() == 1 {
            let fallback = draft.geometry.unwrap_or((0.0, 0.0, 1200.0, 800.0));
            draft.geometry = Some((
                parse_f64_field(&ui.geometry_x_field, fallback.0),
                parse_f64_field(&ui.geometry_y_field, fallback.1),
                parse_f64_field(&ui.geometry_width_field, fallback.2),
                parse_f64_field(&ui.geometry_height_field, fallback.3),
            ));
        } else {
            draft.geometry = None;
        }

        *self.ivars().draft_rule.borrow_mut() = Some(draft.clone());
        Some(draft)
    }

    fn emit_current_rule_draft(&self) {
        if let Some(draft) = self.sync_rule_draft_from_controls() {
            self.emit(SettingsAction::UpdateRuleDraft(draft));
            self.refresh_rule_inspector();
        }
    }

    fn sync_workspace_draft_from_controls(&self) -> Option<WorkspaceDraft> {
        let row = self.selected_workspace_row()?;
        if !row.editable {
            return None;
        }
        let ui_borrow = self.ivars().workspaces_ui.borrow();
        let ui = ui_borrow.as_ref()?;

        let mut draft = self
            .ivars()
            .draft_workspace
            .borrow()
            .clone()
            .unwrap_or_else(|| WorkspaceDraft::from_row(&row));
        draft.monitor_display_id = popup_selected_display_id(&ui.monitor_popup);
        draft.layout = WorkspaceLayout::Bsp;
        draft.gap_inner = parse_optional_f64_field(&ui.gap_inner_field);
        draft.gap_outer = parse_optional_f64_field(&ui.gap_outer_field);
        draft.overlay_position = popup_overlay_position(&ui.overlay_position_popup);
        draft.overlay_width = parse_f64_field(&ui.overlay_width_field, draft.overlay_width);
        draft.overlay_height = parse_f64_field(&ui.overlay_height_field, draft.overlay_height);

        *self.ivars().draft_workspace.borrow_mut() = Some(draft.clone());
        Some(draft)
    }

    fn emit_current_workspace_draft(&self) {
        if let Some(draft) = self.sync_workspace_draft_from_controls() {
            self.emit(SettingsAction::UpdateWorkspaceDraft(draft));
            self.refresh_workspace_inspector();
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TarmacSettingsHandler"]
    #[ivars = SettingsHandlerIvars]
    struct SettingsHandler;

    impl SettingsHandler {
        #[unsafe(method(onGapInnerChanged:))]
        fn on_gap_inner(&self, sender: Option<&NSSlider>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::GapInner(sender.doubleValue()));
            }
        }

        #[unsafe(method(onGapOuterChanged:))]
        fn on_gap_outer(&self, sender: Option<&NSSlider>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::GapOuter(sender.doubleValue()));
            }
        }

        #[unsafe(method(onBarHeightChanged:))]
        fn on_bar_height(&self, sender: Option<&NSSlider>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::BarHeight(sender.doubleValue()));
            }
        }

        #[unsafe(method(onBorderWidthChanged:))]
        fn on_border_width(&self, sender: Option<&NSSlider>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::BorderWidth(sender.doubleValue()));
            }
        }

        #[unsafe(method(onBorderRadiusChanged:))]
        fn on_border_radius(&self, sender: Option<&NSSlider>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::BorderRadius(sender.doubleValue()));
            }
        }

        #[unsafe(method(onBorderColorFocusedChanged:))]
        fn on_border_color_focused(&self, sender: Option<&NSColorWell>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::BorderColorFocused(color_well_to_hex(sender)));
            }
        }

        #[unsafe(method(onBorderColorUnfocusedChanged:))]
        fn on_border_color_unfocused(&self, sender: Option<&NSColorWell>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::BorderColorUnfocused(color_well_to_hex(sender)));
            }
        }

        #[unsafe(method(onFocusFollowsMouseChanged:))]
        fn on_focus_follows_mouse(&self, sender: Option<&NSButton>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::FocusFollowsMouse(sender.state() == 1));
            }
        }

        #[unsafe(method(onMouseFollowsFocusChanged:))]
        fn on_mouse_follows_focus(&self, sender: Option<&NSButton>) {
            if let Some(sender) = sender {
                self.emit(SettingsAction::MouseFollowsFocus(sender.state() == 1));
            }
        }

        #[unsafe(method(onModKeyChanged:))]
        fn on_mod_key_changed(&self, sender: Option<&NSPopUpButton>) {
            if let Some(sender) = sender {
                let value = match sender.indexOfSelectedItem() {
                    1 => "option",
                    2 => "control",
                    _ => "command",
                };
                self.emit(SettingsAction::ModKey(value.to_string()));
            }
        }

        #[unsafe(method(onKeybindAdd:))]
        fn on_keybind_add(&self, _sender: Option<&AnyObject>) {
            self.emit(SettingsAction::AddKeybind);
        }

        #[unsafe(method(onKeybindDelete:))]
        fn on_keybind_delete(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_keybind_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::DeleteKeybind(id));
        }

        #[unsafe(method(onKeybindCopyToManaged:))]
        fn on_keybind_copy_to_managed(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_keybind_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::CopyKeybindToManaged(id));
        }

        #[unsafe(method(onKeybindApply:))]
        fn on_keybind_apply(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_keybind_id.borrow().clone() else {
                return;
            };
            if let Some(draft) = self.sync_keybind_draft_from_controls() {
                self.emit(SettingsAction::UpdateKeybindDraft(draft));
                self.emit(SettingsAction::ApplyKeybind(id));
            }
        }

        #[unsafe(method(onKeybindDraftChanged:))]
        fn on_keybind_draft_changed(&self, _sender: Option<&AnyObject>) {
            self.emit_current_keybind_draft();
        }

        #[unsafe(method(onResetManagedKeybinds:))]
        fn on_reset_managed_keybinds(&self, _sender: Option<&AnyObject>) {
            self.emit(SettingsAction::ResetManagedKeybinds);
        }

        #[unsafe(method(onWorkspaceAddLettered:))]
        fn on_workspace_add_lettered(&self, _sender: Option<&AnyObject>) {
            let existing = self
                .ivars()
                .workspace_rows
                .borrow()
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>();
            if let Some(letter) = prompt_for_workspace_text(
                self.mtm(),
                "Add Lettered Workspace",
                "Enter a single unused letter.",
                "W",
                |value| validate_lettered_workspace_id(value, &existing),
            ) {
                self.emit(SettingsAction::AddLetteredWorkspace(letter));
            }
        }

        #[unsafe(method(onWorkspaceAddSpecial:))]
        fn on_workspace_add_special(&self, _sender: Option<&AnyObject>) {
            let existing = self
                .ivars()
                .workspace_rows
                .borrow()
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>();
            if let Some(name) = prompt_for_workspace_text(
                self.mtm(),
                "Add Special Workspace",
                "Enter a non-empty special workspace name.",
                "terminal",
                |value| validate_special_workspace_name(value, &existing),
            ) {
                self.emit(SettingsAction::AddSpecialWorkspace(name));
            }
        }

        #[unsafe(method(onWorkspaceCopyToManaged:))]
        fn on_workspace_copy_to_managed(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_workspace_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::CopyWorkspaceToManaged(id));
        }

        #[unsafe(method(onWorkspaceDelete:))]
        fn on_workspace_delete(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_workspace_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::DeleteWorkspace(id));
        }

        #[unsafe(method(onWorkspaceApply:))]
        fn on_workspace_apply(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_workspace_id.borrow().clone() else {
                return;
            };
            if let Some(draft) = self.sync_workspace_draft_from_controls() {
                self.emit(SettingsAction::UpdateWorkspaceDraft(draft));
                self.emit(SettingsAction::ApplyWorkspace(id));
            }
        }

        #[unsafe(method(onWorkspaceDraftChanged:))]
        fn on_workspace_draft_changed(&self, _sender: Option<&AnyObject>) {
            self.emit_current_workspace_draft();
        }

        #[unsafe(method(onRuleAdd:))]
        fn on_rule_add(&self, _sender: Option<&AnyObject>) {
            self.emit(SettingsAction::AddRule);
        }

        #[unsafe(method(onRuleDuplicate:))]
        fn on_rule_duplicate(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_rule_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::DuplicateRule(id));
        }

        #[unsafe(method(onRuleDelete:))]
        fn on_rule_delete(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_rule_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::DeleteRule(id));
        }

        #[unsafe(method(onRuleMoveUp:))]
        fn on_rule_move_up(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_rule_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::MoveRuleUp(id));
        }

        #[unsafe(method(onRuleMoveDown:))]
        fn on_rule_move_down(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_rule_id.borrow().clone() else {
                return;
            };
            self.emit(SettingsAction::MoveRuleDown(id));
        }

        #[unsafe(method(onRuleApply:))]
        fn on_rule_apply(&self, _sender: Option<&AnyObject>) {
            let Some(id) = self.ivars().selected_rule_id.borrow().clone() else {
                return;
            };
            if let Some(draft) = self.sync_rule_draft_from_controls() {
                self.emit(SettingsAction::UpdateRuleDraft(draft));
                self.emit(SettingsAction::ApplyRule(id));
            }
        }

        #[unsafe(method(onRuleDraftChanged:))]
        fn on_rule_draft_changed(&self, _sender: Option<&AnyObject>) {
            self.emit_current_rule_draft();
        }

        #[unsafe(method(onRuleInspectorEnabledChanged:))]
        fn on_rule_inspector_enabled_changed(&self, sender: Option<&NSButton>) {
            let Some(sender) = sender else { return };
            self.toggle_selected_rule_enabled(sender.state() == 1);
        }

        #[unsafe(method(onRuleRowEnabledToggled:))]
        fn on_rule_row_enabled_toggled(&self, sender: Option<&NSButton>) {
            let Some(sender) = sender else { return };
            self.toggle_row_enabled(sender.tag() as usize, sender.state() == 1);
        }

        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows_in_table_view(&self, _table_view: &NSTableView) -> NSInteger {
            let keybind_count = self.ivars().keybindings_ui.borrow().as_ref().map_or(0, |ui| {
                usize::from(std::ptr::eq(_table_view, &*ui.table))
            });
            if keybind_count == 1 {
                return self.ivars().keybind_rows.borrow().len() as NSInteger;
            }
            let rule_count = self.ivars().rules_ui.borrow().as_ref().map_or(0, |ui| {
                usize::from(std::ptr::eq(_table_view, &*ui.table))
            });
            if rule_count == 1 {
                return self.ivars().rule_rows.borrow().len() as NSInteger;
            }
            let workspace_count = self.ivars().workspaces_ui.borrow().as_ref().map_or(0, |ui| {
                usize::from(std::ptr::eq(_table_view, &*ui.table))
            });
            if workspace_count == 1 {
                self.ivars().workspace_rows.borrow().len() as NSInteger
            } else {
                0
            }
        }

        #[unsafe(method(tableView:viewForTableColumn:row:))]
        fn table_view_view_for_table_column_row(
            &self,
            table_view: &NSTableView,
            table_column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> *mut NSView {
            let mtm = self.mtm();
            let Some(row) = usize::try_from(row).ok() else {
                return std::ptr::null_mut();
            };
            let Some(column) = table_column else {
                return std::ptr::null_mut();
            };
            let identifier: Retained<NSString> = unsafe { msg_send![column, identifier] };
            let identifier = identifier.to_string();
            let width = column.width();
            let container_frame =
                CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width.max(48.0), 24.0));
            let container: Retained<NSView> =
                unsafe { msg_send![NSView::alloc(mtm), initWithFrame: container_frame] };

            if let Some(row_data) = self
                .ivars()
                .keybindings_ui
                .borrow()
                .as_ref()
                .filter(|ui| std::ptr::eq(table_view, &*ui.table))
                .and_then(|_| self.ivars().keybind_rows.borrow().get(row).cloned())
            {
                match identifier.as_str() {
                    KEYBIND_COLUMN_SHORTCUT => {
                        add_table_label(mtm, &container, &row_data.shortcut, width);
                    }
                    KEYBIND_COLUMN_ACTION => {
                        add_table_label(mtm, &container, &row_data.action, width);
                    }
                    KEYBIND_COLUMN_SOURCE => {
                        add_table_label(mtm, &container, row_data.source_label(), width);
                    }
                    _ => {}
                }
                return Retained::into_raw(container);
            }

            if let Some(row_data) = self
                .ivars()
                .workspaces_ui
                .borrow()
                .as_ref()
                .filter(|ui| std::ptr::eq(table_view, &*ui.table))
                .and_then(|_| self.ivars().workspace_rows.borrow().get(row).cloned())
            {
                match identifier.as_str() {
                    WORKSPACE_COLUMN_ID => add_table_label(mtm, &container, &row_data.id, width),
                    WORKSPACE_COLUMN_KIND => {
                        add_table_label(mtm, &container, row_data.kind_label(), width);
                    }
                    WORKSPACE_COLUMN_MONITOR => {
                        add_table_label(mtm, &container, &row_data.monitor_summary(), width);
                    }
                    WORKSPACE_COLUMN_SUMMARY => {
                        add_table_label(mtm, &container, &row_data.summary(), width);
                    }
                    WORKSPACE_COLUMN_SOURCE => {
                        add_table_label(mtm, &container, row_data.source_label(), width);
                    }
                    _ => {}
                }
                return Retained::into_raw(container);
            }

            let Some(row_data) = self.ivars().rule_rows.borrow().get(row).cloned() else {
                return std::ptr::null_mut();
            };
            match identifier.as_str() {
                RULE_COLUMN_ENABLED => {
                    let checkbox = add_rule_list_checkbox(
                        mtm,
                        &container,
                        self,
                        row,
                        row_data.enabled,
                        row_data.editable,
                    );
                    let _ = checkbox;
                }
                RULE_COLUMN_NAME => {
                    add_table_label(mtm, &container, &row_data.name, width);
                }
                RULE_COLUMN_WHEN => {
                    add_table_label(mtm, &container, &row_data.when_summary(), width);
                }
                RULE_COLUMN_THEN => {
                    add_table_label(mtm, &container, &row_data.then_summary(), width);
                }
                RULE_COLUMN_SOURCE => {
                    add_table_label(mtm, &container, row_data.source_label(), width);
                }
                _ => {}
            }

            Retained::into_raw(container)
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn table_view_selection_did_change(&self, _notification: &NSNotification) {
            if !*self.ivars().suppress_keybind_selection_change.borrow() {
                let selected_row = {
                    let ui_borrow = self.ivars().keybindings_ui.borrow();
                    match ui_borrow.as_ref() {
                        Some(ui) => ui.table.selectedRow(),
                        None => -1,
                    }
                };
                if selected_row >= 0 {
                    let row_index = selected_row as usize;
                    let should_select = self
                        .ivars()
                        .keybind_rows
                        .borrow()
                        .get(row_index)
                        .is_some_and(|row| {
                            self.ivars()
                                .selected_keybind_id
                                .borrow()
                                .as_deref()
                                != Some(row.id.as_str())
                        });
                    if should_select {
                        self.select_keybind_by_row_index(row_index, true);
                    }
                }
            }

            if !*self.ivars().suppress_workspace_selection_change.borrow() {
                let selected_row = {
                    let ui_borrow = self.ivars().workspaces_ui.borrow();
                    match ui_borrow.as_ref() {
                        Some(ui) => ui.table.selectedRow(),
                        None => -1,
                    }
                };
                if selected_row >= 0 {
                    let row_index = selected_row as usize;
                    let should_select = self
                        .ivars()
                        .workspace_rows
                        .borrow()
                        .get(row_index)
                        .is_some_and(|row| {
                            self.ivars()
                                .selected_workspace_id
                                .borrow()
                                .as_deref()
                                != Some(row.id.as_str())
                        });
                    if should_select {
                        self.select_workspace_by_row_index(row_index, true);
                    }
                }
            }

            if *self.ivars().suppress_rule_selection_change.borrow() {
                return;
            }
            let selected_row = {
                let ui_borrow = self.ivars().rules_ui.borrow();
                let Some(ui) = ui_borrow.as_ref() else { return };
                ui.table.selectedRow()
            };
            if selected_row < 0 {
                return;
            }
            self.select_rule_by_row_index(selected_row as usize, true);
        }
    }
);

pub struct SettingsWindow {
    window: Retained<NSWindow>,
    _tab_controller: Retained<NSTabViewController>,
    handler: Retained<SettingsHandler>,
    action_rx: mpsc::Receiver<SettingsAction>,
    gap_inner_slider: Retained<NSSlider>,
    gap_outer_slider: Retained<NSSlider>,
    bar_height_slider: Retained<NSSlider>,
    border_width_slider: Retained<NSSlider>,
    border_radius_slider: Retained<NSSlider>,
    focused_color_well: Retained<NSColorWell>,
    unfocused_color_well: Retained<NSColorWell>,
    ffm_checkbox: Retained<NSButton>,
    mff_checkbox: Retained<NSButton>,
    mod_key_popup: Retained<NSPopUpButton>,
    gap_inner_label: Retained<NSTextField>,
    gap_outer_label: Retained<NSTextField>,
    bar_height_label: Retained<NSTextField>,
    border_width_label: Retained<NSTextField>,
    border_radius_label: Retained<NSTextField>,
}

impl SettingsWindow {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let (tx, rx) = mpsc::channel();
        let handler = SettingsHandler::new(mtm, tx);

        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let frame = CGRect::new(CGPoint::new(200.0, 160.0), CGSize::new(WIN_W, WIN_H));
        let window: Retained<NSWindow> = unsafe {
            msg_send![
                NSWindow::alloc(mtm),
                initWithContentRect: frame,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        window.setTitle(&NSString::from_str("tarmac Settings"));
        window.center();
        unsafe { window.setReleasedWhenClosed(false) };
        window.setToolbarStyle(NSWindowToolbarStyle::Preference);
        window.setExcludedFromWindowsMenu(true);
        window.setFrameAutosaveName(&NSString::from_str("TarmacSettingsWindow"));

        let tab_controller = NSTabViewController::new(mtm);
        tab_controller.setTitle(Some(&NSString::from_str("tarmac Settings")));
        tab_controller.setTabStyle(NSTabViewControllerTabStyle::Toolbar);

        let general = build_general_view(mtm, &handler);
        let keybindings = build_keybindings_tab(mtm, &handler);
        handler.set_keybindings_ui(keybindings.ui);

        let rules = build_rules_tab(mtm, &handler);
        handler.set_rules_ui(rules.ui);

        let workspaces = build_workspaces_tab(mtm, &handler);
        handler.set_workspaces_ui(workspaces.ui);

        let about_view = build_about_view(mtm);

        add_tab(mtm, &tab_controller, "General", "gearshape", &general.root);
        add_tab(
            mtm,
            &tab_controller,
            "Keybindings",
            "keyboard",
            &keybindings.root,
        );
        add_tab(
            mtm,
            &tab_controller,
            "Rules",
            "line.3.horizontal.decrease.circle",
            &rules.root,
        );
        add_tab(
            mtm,
            &tab_controller,
            "Workspaces",
            "square.grid.2x2",
            &workspaces.root,
        );
        add_tab(mtm, &tab_controller, "About", "info.circle", &about_view);

        window.setContentViewController(Some(&tab_controller));

        Self {
            window,
            _tab_controller: tab_controller,
            handler,
            action_rx: rx,
            gap_inner_slider: general.gap_inner_slider,
            gap_outer_slider: general.gap_outer_slider,
            bar_height_slider: general.bar_height_slider,
            border_width_slider: general.border_width_slider,
            border_radius_slider: general.border_radius_slider,
            focused_color_well: general.focused_color_well,
            unfocused_color_well: general.unfocused_color_well,
            ffm_checkbox: general.ffm_checkbox,
            mff_checkbox: general.mff_checkbox,
            mod_key_popup: general.mod_key_popup,
            gap_inner_label: general.gap_inner_label,
            gap_outer_label: general.gap_outer_label,
            bar_height_label: general.bar_height_label,
            border_width_label: general.border_width_label,
            border_radius_label: general.border_radius_label,
        }
    }

    pub fn open_or_focus(&self) {
        use objc2_app_kit::NSApplication;

        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let app = NSApplication::sharedApplication(mtm);
        app.activate();
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    pub fn populate(&self, snapshot: &SettingsSnapshot) {
        self.gap_inner_slider.setDoubleValue(snapshot.gap_inner);
        self.gap_outer_slider.setDoubleValue(snapshot.gap_outer);
        self.bar_height_slider.setDoubleValue(snapshot.bar_height);
        self.border_width_slider
            .setDoubleValue(snapshot.border_width);
        self.border_radius_slider
            .setDoubleValue(snapshot.border_radius);
        set_value_label(&self.gap_inner_label, snapshot.gap_inner);
        set_value_label(&self.gap_outer_label, snapshot.gap_outer);
        set_value_label(&self.bar_height_label, snapshot.bar_height);
        set_value_label(&self.border_width_label, snapshot.border_width);
        set_value_label(&self.border_radius_label, snapshot.border_radius);
        set_color_well(&self.focused_color_well, &snapshot.border_color_focused);
        set_color_well(&self.unfocused_color_well, &snapshot.border_color_unfocused);
        self.ffm_checkbox
            .setState(if snapshot.focus_follows_mouse { 1 } else { 0 });
        self.mff_checkbox
            .setState(if snapshot.mouse_follows_focus { 1 } else { 0 });
        self.mod_key_popup
            .selectItemAtIndex(match snapshot.mod_key.as_str() {
                "option" => 1,
                "control" => 2,
                _ => 0,
            });
        self.handler.load_keybinds(
            snapshot.keybinds.clone(),
            snapshot.selected_keybind_id.clone(),
        );
        self.handler
            .load_rules(snapshot.rules.clone(), snapshot.selected_rule_id.clone());
        self.handler.load_workspaces(
            snapshot.workspaces.clone(),
            snapshot.selected_workspace_id.clone(),
            snapshot.displays.clone(),
        );
    }

    pub fn poll_actions(&self) -> Vec<SettingsAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.action_rx.try_recv() {
            actions.push(action);
        }
        actions
    }

    pub fn refresh_labels(&self) {
        set_value_label(&self.gap_inner_label, self.gap_inner_slider.doubleValue());
        set_value_label(&self.gap_outer_label, self.gap_outer_slider.doubleValue());
        set_value_label(&self.bar_height_label, self.bar_height_slider.doubleValue());
        set_value_label(
            &self.border_width_label,
            self.border_width_slider.doubleValue(),
        );
        set_value_label(
            &self.border_radius_label,
            self.border_radius_slider.doubleValue(),
        );
    }
}

struct GeneralTab {
    root: Retained<NSView>,
    gap_inner_slider: Retained<NSSlider>,
    gap_outer_slider: Retained<NSSlider>,
    bar_height_slider: Retained<NSSlider>,
    border_width_slider: Retained<NSSlider>,
    border_radius_slider: Retained<NSSlider>,
    focused_color_well: Retained<NSColorWell>,
    unfocused_color_well: Retained<NSColorWell>,
    ffm_checkbox: Retained<NSButton>,
    mff_checkbox: Retained<NSButton>,
    mod_key_popup: Retained<NSPopUpButton>,
    gap_inner_label: Retained<NSTextField>,
    gap_outer_label: Retained<NSTextField>,
    bar_height_label: Retained<NSTextField>,
    border_width_label: Retained<NSTextField>,
    border_radius_label: Retained<NSTextField>,
}

struct RulesTab {
    root: Retained<NSView>,
    ui: RulesUiRefs,
}

struct KeybindingsTab {
    root: Retained<NSView>,
    ui: KeybindingsUiRefs,
}

struct WorkspacesTab {
    root: Retained<NSView>,
    ui: WorkspacesUiRefs,
}

fn add_tab(
    mtm: MainThreadMarker,
    tab_controller: &NSTabViewController,
    title: &str,
    symbol: &str,
    root: &NSView,
) {
    let view_controller = NSViewController::new(mtm);
    view_controller.setTitle(Some(&NSString::from_str(title)));
    view_controller.setView(root);
    let item = NSTabViewItem::tabViewItemWithViewController(&view_controller);
    item.setLabel(&NSString::from_str(title));
    if let Some(image) = symbol_image(symbol) {
        item.setImage(Some(&image));
    }
    tab_controller.addTabViewItem(&item);
}

fn symbol_image(symbol: &str) -> Option<Retained<NSImage>> {
    let name = NSString::from_str(symbol);
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, None)?;
    image.setTemplate(true);
    Some(image)
}

fn build_general_view(mtm: MainThreadMarker, handler: &SettingsHandler) -> GeneralTab {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, WIN_H));
    let view: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    let lx = 28.0;
    let cx = 220.0;
    let cw = 260.0;
    let row = 34.0;
    let mut y = WIN_H - 60.0;

    add_section_label(mtm, &view, "Layout", lx, y);
    y -= row;
    let gap_inner_label = add_value_label(mtm, &view, "0", cx + cw + 12.0, y);
    let gap_inner_slider = add_slider(
        mtm,
        &view,
        handler,
        "Inner Gap",
        lx,
        cx,
        y,
        cw,
        0.0,
        50.0,
        0.0,
        sel!(onGapInnerChanged:),
    );
    y -= row;
    let gap_outer_label = add_value_label(mtm, &view, "0", cx + cw + 12.0, y);
    let gap_outer_slider = add_slider(
        mtm,
        &view,
        handler,
        "Outer Gap",
        lx,
        cx,
        y,
        cw,
        0.0,
        50.0,
        0.0,
        sel!(onGapOuterChanged:),
    );
    y -= row;
    let bar_height_label = add_value_label(mtm, &view, "0", cx + cw + 12.0, y);
    let bar_height_slider = add_slider(
        mtm,
        &view,
        handler,
        "Bar Height",
        lx,
        cx,
        y,
        cw,
        0.0,
        80.0,
        0.0,
        sel!(onBarHeightChanged:),
    );
    y -= row + 18.0;

    add_section_label(mtm, &view, "Borders", lx, y);
    y -= row;
    let border_width_label = add_value_label(mtm, &view, "0", cx + cw + 12.0, y);
    let border_width_slider = add_slider(
        mtm,
        &view,
        handler,
        "Width",
        lx,
        cx,
        y,
        cw,
        0.0,
        10.0,
        0.0,
        sel!(onBorderWidthChanged:),
    );
    y -= row;
    let border_radius_label = add_value_label(mtm, &view, "0", cx + cw + 12.0, y);
    let border_radius_slider = add_slider(
        mtm,
        &view,
        handler,
        "Radius",
        lx,
        cx,
        y,
        cw,
        0.0,
        30.0,
        10.0,
        sel!(onBorderRadiusChanged:),
    );
    y -= row;
    add_label(mtm, &view, "Focused Color", lx, y);
    let focused_color_well = add_color_well(
        mtm,
        &view,
        handler,
        cx,
        y,
        sel!(onBorderColorFocusedChanged:),
    );
    y -= row;
    add_label(mtm, &view, "Unfocused Color", lx, y);
    let unfocused_color_well = add_color_well(
        mtm,
        &view,
        handler,
        cx,
        y,
        sel!(onBorderColorUnfocusedChanged:),
    );
    y -= row + 18.0;

    add_section_label(mtm, &view, "Behavior", lx, y);
    y -= row;
    let ffm_checkbox = add_checkbox(
        mtm,
        &view,
        handler,
        "Focus follows mouse",
        lx,
        y,
        sel!(onFocusFollowsMouseChanged:),
    );
    y -= row;
    let mff_checkbox = add_checkbox(
        mtm,
        &view,
        handler,
        "Mouse follows focus",
        lx,
        y,
        sel!(onMouseFollowsFocusChanged:),
    );
    y -= row + 18.0;

    add_section_label(mtm, &view, "Modifier Key", lx, y);
    y -= row;
    let mod_key_popup = add_popup(
        mtm,
        &view,
        handler,
        lx,
        y,
        220.0,
        &["Command", "Option", "Control"],
        sel!(onModKeyChanged:),
    );

    GeneralTab {
        root: view,
        gap_inner_slider,
        gap_outer_slider,
        bar_height_slider,
        border_width_slider,
        border_radius_slider,
        focused_color_well,
        unfocused_color_well,
        ffm_checkbox,
        mff_checkbox,
        mod_key_popup,
        gap_inner_label,
        gap_outer_label,
        bar_height_label,
        border_width_label,
        border_radius_label,
    }
}

fn build_workspaces_tab(mtm: MainThreadMarker, handler: &SettingsHandler) -> WorkspacesTab {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, WIN_H));
    let root: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    let split: Retained<NSSplitView> =
        unsafe { msg_send![NSSplitView::alloc(mtm), initWithFrame: frame] };
    split.setVertical(true);
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    split.setAutosaveName(Some(&NSString::from_str("TarmacWorkspacesSplitView")));

    let left_width = 372.0;
    let left_frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(left_width, WIN_H));
    let right_frame = CGRect::new(
        CGPoint::new(left_width + 1.0, 0.0),
        CGSize::new(WIN_W - left_width - 1.0, WIN_H),
    );
    let left: Retained<NSView> =
        unsafe { msg_send![NSView::alloc(mtm), initWithFrame: left_frame] };
    let right: Retained<NSView> =
        unsafe { msg_send![NSView::alloc(mtm), initWithFrame: right_frame] };

    add_section_label(mtm, &left, "Workspaces", 24.0, WIN_H - 46.0);
    add_wrapped_label(
        mtm,
        &left,
        "Numbered workspaces 1 through 10 are always shown. Copy a default or Lua-authored workspace into managed settings before editing it here.",
        24.0,
        WIN_H - 76.0,
        left_width - 48.0,
        40.0,
    );

    let table_scroll_frame = CGRect::new(
        CGPoint::new(20.0, 96.0),
        CGSize::new(left_width - 40.0, WIN_H - 196.0),
    );
    let table_scroll: Retained<NSScrollView> =
        unsafe { msg_send![NSScrollView::alloc(mtm), initWithFrame: table_scroll_frame] };
    table_scroll.setHasVerticalScroller(true);
    table_scroll.setBorderType(objc2_app_kit::NSBorderType(2));

    let table_frame = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(left_width - 40.0, WIN_H - 196.0),
    );
    let table: Retained<NSTableView> =
        unsafe { msg_send![NSTableView::alloc(mtm), initWithFrame: table_frame] };
    table.setUsesAlternatingRowBackgroundColors(true);
    table.setAllowsEmptySelection(true);
    table.setColumnAutoresizingStyle(
        objc2_app_kit::NSTableViewColumnAutoresizingStyle::SequentialColumnAutoresizingStyle,
    );
    table.setStyle(NSTableViewStyle::Inset);
    table.setRowSizeStyle(NSTableViewRowSizeStyle::Medium);
    table.setRowHeight(28.0);
    table.setIntercellSpacing(CGSize::new(8.0, 4.0));

    add_workspace_table_column(mtm, &table, WORKSPACE_COLUMN_ID, "ID", 64.0);
    add_workspace_table_column(mtm, &table, WORKSPACE_COLUMN_KIND, "Kind", 84.0);
    add_workspace_table_column(mtm, &table, WORKSPACE_COLUMN_MONITOR, "Monitor", 112.0);
    add_workspace_table_column(mtm, &table, WORKSPACE_COLUMN_SUMMARY, "Summary", 210.0);
    add_workspace_table_column(mtm, &table, WORKSPACE_COLUMN_SOURCE, "Source", 72.0);

    unsafe {
        let _: () = msg_send![&*table, setDataSource: handler];
        let _: () = msg_send![&*table, setDelegate: handler];
    }

    table_scroll.setDocumentView(Some(&table));
    left.addSubview(&table_scroll);

    let _add_lettered_button = add_button(
        mtm,
        &left,
        handler,
        "Add Lettered…",
        20.0,
        34.0,
        114.0,
        sel!(onWorkspaceAddLettered:),
    );
    let _add_special_button = add_button(
        mtm,
        &left,
        handler,
        "Add Special…",
        142.0,
        34.0,
        108.0,
        sel!(onWorkspaceAddSpecial:),
    );

    add_section_label(mtm, &right, "Workspace Inspector", 28.0, WIN_H - 46.0);
    let placeholder_label = add_wrapped_label_field(
        mtm,
        &right,
        "Select a workspace to inspect. Managed rows can be edited here and applied back into the managed config block.",
        28.0,
        WIN_H - 122.0,
        460.0,
        44.0,
    );

    let mut y = WIN_H - 96.0;
    add_section_label(mtm, &right, "General", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Source", 28.0, y);
    let source_value = add_value_label(mtm, &right, "", 198.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Kind", 28.0, y);
    let kind_value = add_value_label(mtm, &right, "", 198.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Workspace ID", 28.0, y);
    let workspace_id_value = add_value_label(mtm, &right, "", 198.0, y);

    y -= 50.0;
    add_section_label(mtm, &right, "Placement", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Monitor", 28.0, y);
    let monitor_popup = add_popup(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        250.0,
        &["No preference"],
        sel!(onWorkspaceDraftChanged:),
    );
    y -= 36.0;
    add_label(mtm, &right, "Layout", 28.0, y);
    let layout_popup = add_popup(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        180.0,
        &["Bsp"],
        sel!(onWorkspaceDraftChanged:),
    );

    y -= 50.0;
    add_section_label(mtm, &right, "Gaps", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Inner Gap", 28.0, y);
    let gap_inner_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        96.0,
        "default",
        sel!(onWorkspaceDraftChanged:),
    );
    add_label(mtm, &right, "Outer Gap", 312.0, y);
    let gap_outer_field = add_input_field(
        mtm,
        &right,
        handler,
        392.0,
        y - 3.0,
        96.0,
        "default",
        sel!(onWorkspaceDraftChanged:),
    );

    y -= 54.0;
    let overlay_section_label = add_section_label_field(mtm, &right, "Special Overlay", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Position", 28.0, y);
    let overlay_position_popup = add_popup(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        140.0,
        &["Center", "Top", "Bottom"],
        sel!(onWorkspaceDraftChanged:),
    );
    y -= 36.0;
    add_label(mtm, &right, "Width", 28.0, y);
    let overlay_width_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        96.0,
        "0.7",
        sel!(onWorkspaceDraftChanged:),
    );
    add_label(mtm, &right, "Height", 312.0, y);
    let overlay_height_field = add_input_field(
        mtm,
        &right,
        handler,
        392.0,
        y - 3.0,
        96.0,
        "0.7",
        sel!(onWorkspaceDraftChanged:),
    );

    let copy_button = add_button(
        mtm,
        &right,
        handler,
        "Copy To Managed",
        right_frame.size.width - 338.0,
        34.0,
        142.0,
        sel!(onWorkspaceCopyToManaged:),
    );
    let delete_button = add_button(
        mtm,
        &right,
        handler,
        "Delete",
        right_frame.size.width - 188.0,
        34.0,
        82.0,
        sel!(onWorkspaceDelete:),
    );
    let apply_button = add_button(
        mtm,
        &right,
        handler,
        "Apply Workspace",
        right_frame.size.width - 144.0,
        34.0,
        120.0,
        sel!(onWorkspaceApply:),
    );

    split.addSubview(&left);
    split.addSubview(&right);
    split.adjustSubviews();
    split.setPosition_ofDividerAtIndex(left_width, 0);
    root.addSubview(&split);

    WorkspacesTab {
        root,
        ui: WorkspacesUiRefs {
            table,
            placeholder_label,
            source_value,
            kind_value,
            workspace_id_value,
            monitor_popup,
            layout_popup,
            gap_inner_field,
            gap_outer_field,
            overlay_position_popup,
            overlay_width_field,
            overlay_height_field,
            overlay_section_label,
            copy_button,
            delete_button,
            apply_button,
        },
    }
}

fn build_keybindings_tab(mtm: MainThreadMarker, handler: &SettingsHandler) -> KeybindingsTab {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, WIN_H));
    let root: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    let split: Retained<NSSplitView> =
        unsafe { msg_send![NSSplitView::alloc(mtm), initWithFrame: frame] };
    split.setVertical(true);
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    split.setAutosaveName(Some(&NSString::from_str("TarmacKeybindingsSplitView")));

    let left_width = 332.0;
    let left_frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(left_width, WIN_H));
    let right_frame = CGRect::new(
        CGPoint::new(left_width + 1.0, 0.0),
        CGSize::new(WIN_W - left_width - 1.0, WIN_H),
    );
    let left: Retained<NSView> =
        unsafe { msg_send![NSView::alloc(mtm), initWithFrame: left_frame] };
    let right: Retained<NSView> =
        unsafe { msg_send![NSView::alloc(mtm), initWithFrame: right_frame] };

    add_section_label(mtm, &left, "Keybindings", 24.0, WIN_H - 46.0);
    add_wrapped_label(
        mtm,
        &left,
        "Managed keybindings are editable. Lua-authored and default bindings stay visible here, but open read-only in the inspector.",
        24.0,
        WIN_H - 76.0,
        left_width - 48.0,
        38.0,
    );

    let table_scroll_frame = CGRect::new(
        CGPoint::new(20.0, 96.0),
        CGSize::new(left_width - 40.0, WIN_H - 176.0),
    );
    let table_scroll: Retained<NSScrollView> =
        unsafe { msg_send![NSScrollView::alloc(mtm), initWithFrame: table_scroll_frame] };
    table_scroll.setHasVerticalScroller(true);
    table_scroll.setBorderType(objc2_app_kit::NSBorderType(2));

    let table_frame = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(left_width - 40.0, WIN_H - 176.0),
    );
    let table: Retained<NSTableView> =
        unsafe { msg_send![NSTableView::alloc(mtm), initWithFrame: table_frame] };
    table.setUsesAlternatingRowBackgroundColors(true);
    table.setAllowsEmptySelection(true);
    table.setColumnAutoresizingStyle(
        objc2_app_kit::NSTableViewColumnAutoresizingStyle::SequentialColumnAutoresizingStyle,
    );
    table.setStyle(NSTableViewStyle::Inset);
    table.setRowSizeStyle(NSTableViewRowSizeStyle::Medium);
    table.setRowHeight(28.0);
    table.setIntercellSpacing(CGSize::new(8.0, 4.0));

    add_keybind_table_column(mtm, &table, KEYBIND_COLUMN_SHORTCUT, "Shortcut", 134.0);
    add_keybind_table_column(mtm, &table, KEYBIND_COLUMN_ACTION, "Action", 184.0);
    add_keybind_table_column(mtm, &table, KEYBIND_COLUMN_SOURCE, "Source", 72.0);

    unsafe {
        let _: () = msg_send![&*table, setDataSource: handler];
        let _: () = msg_send![&*table, setDelegate: handler];
    }

    table_scroll.setDocumentView(Some(&table));
    left.addSubview(&table_scroll);

    let add_keybind_button = add_button(
        mtm,
        &left,
        handler,
        "Add",
        20.0,
        34.0,
        62.0,
        sel!(onKeybindAdd:),
    );
    let delete_button = add_button(
        mtm,
        &left,
        handler,
        "Delete",
        90.0,
        34.0,
        72.0,
        sel!(onKeybindDelete:),
    );
    let reset_keybinds_button = add_button(
        mtm,
        &left,
        handler,
        "Reset Defaults",
        170.0,
        34.0,
        122.0,
        sel!(onResetManagedKeybinds:),
    );
    let _ = add_keybind_button;
    let _ = reset_keybinds_button;

    add_section_label(mtm, &right, "Keybinding Inspector", 28.0, WIN_H - 46.0);
    let placeholder_label = add_wrapped_label_field(
        mtm,
        &right,
        "Select a keybinding to inspect. Managed keybindings can be edited here and applied back into the managed config block.",
        28.0,
        WIN_H - 122.0,
        460.0,
        44.0,
    );

    let mut y = WIN_H - 96.0;
    add_section_label(mtm, &right, "General", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Source", 28.0, y);
    let source_value = add_value_label(mtm, &right, "", 198.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Shortcut", 28.0, y);
    let shortcut_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        250.0,
        "mod+shift+period",
        sel!(onKeybindDraftChanged:),
    );

    y -= 56.0;
    add_section_label(mtm, &right, "Action", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Command", 28.0, y);
    let action_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        250.0,
        "reload",
        sel!(onKeybindDraftChanged:),
    );
    add_wrapped_label(
        mtm,
        &right,
        "Use the same action strings as Lua, for example `focus left`, `workspace 3`, `move_to_workspace W`, or `toggle_special terminal`.",
        28.0,
        y - 54.0,
        460.0,
        44.0,
    );

    let copy_button = add_button(
        mtm,
        &right,
        handler,
        "Copy To Managed",
        right_frame.size.width - 332.0,
        34.0,
        142.0,
        sel!(onKeybindCopyToManaged:),
    );

    let apply_button = add_button(
        mtm,
        &right,
        handler,
        "Apply Keybinding",
        right_frame.size.width - 182.0,
        34.0,
        142.0,
        sel!(onKeybindApply:),
    );

    split.addSubview(&left);
    split.addSubview(&right);
    split.adjustSubviews();
    split.setPosition_ofDividerAtIndex(left_width, 0);
    root.addSubview(&split);

    KeybindingsTab {
        root,
        ui: KeybindingsUiRefs {
            table,
            placeholder_label,
            source_value,
            shortcut_field,
            action_field,
            delete_button,
            copy_button,
            apply_button,
        },
    }
}

fn build_rules_tab(mtm: MainThreadMarker, handler: &SettingsHandler) -> RulesTab {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, WIN_H));
    let root: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    let split: Retained<NSSplitView> =
        unsafe { msg_send![NSSplitView::alloc(mtm), initWithFrame: frame] };
    split.setVertical(true);
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    split.setAutosaveName(Some(&NSString::from_str("TarmacRulesSplitView")));

    let left_width = 344.0;
    let left_frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(left_width, WIN_H));
    let right_frame = CGRect::new(
        CGPoint::new(left_width + 1.0, 0.0),
        CGSize::new(WIN_W - left_width - 1.0, WIN_H),
    );
    let left: Retained<NSView> =
        unsafe { msg_send![NSView::alloc(mtm), initWithFrame: left_frame] };
    let right: Retained<NSView> =
        unsafe { msg_send![NSView::alloc(mtm), initWithFrame: right_frame] };

    add_section_label(mtm, &left, "Rules", 24.0, WIN_H - 46.0);
    add_wrapped_label(
        mtm,
        &left,
        "Managed rules are editable. Lua-authored rules stay visible here, but open read-only in the inspector.",
        24.0,
        WIN_H - 76.0,
        left_width - 48.0,
        38.0,
    );

    let table_scroll_frame = CGRect::new(
        CGPoint::new(20.0, 96.0),
        CGSize::new(left_width - 40.0, WIN_H - 176.0),
    );
    let table_scroll: Retained<NSScrollView> =
        unsafe { msg_send![NSScrollView::alloc(mtm), initWithFrame: table_scroll_frame] };
    table_scroll.setHasVerticalScroller(true);
    table_scroll.setBorderType(objc2_app_kit::NSBorderType(2));

    let table_frame = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(left_width - 40.0, WIN_H - 176.0),
    );
    let table: Retained<NSTableView> =
        unsafe { msg_send![NSTableView::alloc(mtm), initWithFrame: table_frame] };
    table.setUsesAlternatingRowBackgroundColors(true);
    table.setAllowsEmptySelection(true);
    table.setColumnAutoresizingStyle(
        objc2_app_kit::NSTableViewColumnAutoresizingStyle::SequentialColumnAutoresizingStyle,
    );
    table.setStyle(NSTableViewStyle::Inset);
    table.setRowSizeStyle(NSTableViewRowSizeStyle::Medium);
    table.setRowHeight(28.0);
    table.setIntercellSpacing(CGSize::new(8.0, 4.0));

    add_rule_table_column(mtm, &table, RULE_COLUMN_ENABLED, "", 34.0);
    add_rule_table_column(mtm, &table, RULE_COLUMN_NAME, "Name", 112.0);
    add_rule_table_column(mtm, &table, RULE_COLUMN_WHEN, "When", 220.0);
    add_rule_table_column(mtm, &table, RULE_COLUMN_THEN, "Then", 190.0);
    add_rule_table_column(mtm, &table, RULE_COLUMN_SOURCE, "Source", 72.0);

    unsafe {
        let _: () = msg_send![&*table, setDataSource: handler];
        let _: () = msg_send![&*table, setDelegate: handler];
    }

    table_scroll.setDocumentView(Some(&table));
    left.addSubview(&table_scroll);

    let add_rule_button = add_button(
        mtm,
        &left,
        handler,
        "Add",
        20.0,
        34.0,
        62.0,
        sel!(onRuleAdd:),
    );
    let duplicate_button = add_button(
        mtm,
        &left,
        handler,
        "Duplicate",
        90.0,
        34.0,
        88.0,
        sel!(onRuleDuplicate:),
    );
    let delete_button = add_button(
        mtm,
        &left,
        handler,
        "Delete",
        186.0,
        34.0,
        72.0,
        sel!(onRuleDelete:),
    );
    let move_up_button = add_button(
        mtm,
        &left,
        handler,
        "Move Up",
        20.0,
        68.0,
        80.0,
        sel!(onRuleMoveUp:),
    );
    let move_down_button = add_button(
        mtm,
        &left,
        handler,
        "Move Down",
        108.0,
        68.0,
        100.0,
        sel!(onRuleMoveDown:),
    );
    let _ = add_rule_button;

    add_section_label(mtm, &right, "Rule Inspector", 28.0, WIN_H - 46.0);
    let placeholder_label = add_wrapped_label_field(
        mtm,
        &right,
        "Select a rule to inspect. Managed rules can be edited here and applied back into the managed config block.",
        28.0,
        WIN_H - 122.0,
        460.0,
        44.0,
    );

    let mut y = WIN_H - 96.0;
    add_section_label(mtm, &right, "General", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Source", 28.0, y);
    let source_value = add_value_label(mtm, &right, "", 198.0, y);
    y -= 34.0;
    add_label(mtm, &right, "Name", 28.0, y);
    let name_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        250.0,
        "New Rule",
        sel!(onRuleDraftChanged:),
    );
    y -= 36.0;
    let enabled_checkbox = add_checkbox(
        mtm,
        &right,
        handler,
        "Enabled",
        28.0,
        y,
        sel!(onRuleInspectorEnabledChanged:),
    );

    y -= 50.0;
    add_section_label(mtm, &right, "Match", 28.0, y);
    y -= 34.0;
    add_label(mtm, &right, "App Name", 28.0, y);
    let app_name_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        180.0,
        "Safari",
        sel!(onRuleDraftChanged:),
    );
    let app_name_mode_popup = add_popup(
        mtm,
        &right,
        handler,
        388.0,
        y - 3.0,
        120.0,
        &["Contains", "Exact", "Regex"],
        sel!(onRuleDraftChanged:),
    );
    y -= 36.0;
    add_label(mtm, &right, "Bundle ID", 28.0, y);
    let bundle_id_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        180.0,
        "com.apple.Safari",
        sel!(onRuleDraftChanged:),
    );
    let bundle_id_mode_popup = add_popup(
        mtm,
        &right,
        handler,
        388.0,
        y - 3.0,
        120.0,
        &["Contains", "Exact", "Regex"],
        sel!(onRuleDraftChanged:),
    );
    y -= 36.0;
    add_label(mtm, &right, "Title", 28.0, y);
    let title_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        180.0,
        "Profile .*",
        sel!(onRuleDraftChanged:),
    );
    let title_mode_popup = add_popup(
        mtm,
        &right,
        handler,
        388.0,
        y - 3.0,
        120.0,
        &["Contains", "Exact", "Regex"],
        sel!(onRuleDraftChanged:),
    );

    y -= 50.0;
    add_section_label(mtm, &right, "Actions", 28.0, y);
    y -= 34.0;
    let floating_checkbox = add_checkbox(
        mtm,
        &right,
        handler,
        "Float matching windows",
        28.0,
        y,
        sel!(onRuleDraftChanged:),
    );
    y -= 36.0;
    add_label(mtm, &right, "Workspace", 28.0, y);
    let workspace_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        250.0,
        "1 or special:terminal",
        sel!(onRuleDraftChanged:),
    );
    y -= 42.0;
    let custom_geometry_checkbox = add_checkbox(
        mtm,
        &right,
        handler,
        "Use custom geometry",
        28.0,
        y,
        sel!(onRuleDraftChanged:),
    );
    y -= 38.0;
    add_label(mtm, &right, "X", 28.0, y);
    let geometry_x_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        72.0,
        "0",
        sel!(onRuleDraftChanged:),
    );
    add_label(mtm, &right, "Y", 286.0, y);
    let geometry_y_field = add_input_field(
        mtm,
        &right,
        handler,
        324.0,
        y - 3.0,
        72.0,
        "0",
        sel!(onRuleDraftChanged:),
    );
    y -= 36.0;
    add_label(mtm, &right, "Width", 28.0, y);
    let geometry_width_field = add_input_field(
        mtm,
        &right,
        handler,
        198.0,
        y - 3.0,
        72.0,
        "1200",
        sel!(onRuleDraftChanged:),
    );
    add_label(mtm, &right, "Height", 286.0, y);
    let geometry_height_field = add_input_field(
        mtm,
        &right,
        handler,
        324.0,
        y - 3.0,
        72.0,
        "800",
        sel!(onRuleDraftChanged:),
    );

    let apply_button = add_button(
        mtm,
        &right,
        handler,
        "Apply Rule",
        right_frame.size.width - 160.0,
        34.0,
        120.0,
        sel!(onRuleApply:),
    );

    split.addSubview(&left);
    split.addSubview(&right);
    split.adjustSubviews();
    split.setPosition_ofDividerAtIndex(left_width, 0);
    root.addSubview(&split);

    RulesTab {
        root,
        ui: RulesUiRefs {
            table,
            placeholder_label,
            source_value,
            name_field,
            enabled_checkbox,
            app_name_field,
            app_name_mode_popup,
            bundle_id_field,
            bundle_id_mode_popup,
            title_field,
            title_mode_popup,
            floating_checkbox,
            workspace_field,
            custom_geometry_checkbox,
            geometry_x_field,
            geometry_y_field,
            geometry_width_field,
            geometry_height_field,
            duplicate_button,
            delete_button,
            move_up_button,
            move_down_button,
            apply_button,
        },
    }
}

fn derive_keybind_inspector_state(
    rows: &[KeybindRow],
    selected_keybind_id: Option<&str>,
    draft_keybind: Option<&KeybindDraft>,
) -> KeybindInspectorState {
    let Some(selected_keybind_id) = selected_keybind_id else {
        return KeybindInspectorState {
            editable: false,
            source_label: String::new(),
            can_delete: false,
            can_copy_to_managed: false,
            can_apply: false,
            draft: None,
        };
    };

    let Some(row) = rows.iter().find(|row| row.id == selected_keybind_id) else {
        return KeybindInspectorState {
            editable: false,
            source_label: String::new(),
            can_delete: false,
            can_copy_to_managed: false,
            can_apply: false,
            draft: None,
        };
    };

    KeybindInspectorState {
        editable: row.editable,
        source_label: row.source_label().to_string(),
        can_delete: row.editable,
        can_copy_to_managed: !row.editable,
        can_apply: row.editable,
        draft: if row.editable {
            draft_keybind
                .cloned()
                .or_else(|| Some(KeybindDraft::from_row(row)))
        } else {
            Some(KeybindDraft::from_row(row))
        },
    }
}

fn apply_keybind_inspector_state(ui: &KeybindingsUiRefs, state: &KeybindInspectorState) {
    let show_placeholder = state.draft.is_none();
    ui.placeholder_label.setHidden(!show_placeholder);
    ui.source_value
        .setStringValue(&NSString::from_str(&state.source_label));
    ui.delete_button.setEnabled(state.can_delete);
    ui.copy_button.setEnabled(state.can_copy_to_managed);
    ui.apply_button.setEnabled(state.can_apply);

    let Some(draft) = state.draft.as_ref() else {
        set_text_field_value(&ui.shortcut_field, "");
        set_text_field_value(&ui.action_field, "");
        set_keybind_inspector_enabled(ui, false);
        return;
    };

    set_text_field_value(&ui.shortcut_field, &draft.shortcut);
    set_text_field_value(&ui.action_field, &draft.action);
    set_keybind_inspector_enabled(ui, state.editable);
}

fn set_keybind_inspector_enabled(ui: &KeybindingsUiRefs, editable: bool) {
    ui.shortcut_field.setEnabled(editable);
    ui.action_field.setEnabled(editable);
}

fn build_about_view(mtm: MainThreadMarker) -> Retained<NSView> {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, WIN_H));
    let view: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    add_centered_label(mtm, &view, "tarmac", WIN_H - 140.0, 28.0);
    add_centered_label(
        mtm,
        &view,
        &format!("Version {}", env!("CARGO_PKG_VERSION")),
        WIN_H - 178.0,
        18.0,
    );
    add_centered_label(
        mtm,
        &view,
        "Native macOS tiling window manager",
        WIN_H - 212.0,
        14.0,
    );
    add_centered_label(
        mtm,
        &view,
        "Repository: github.com/gardesk/garmac",
        WIN_H - 270.0,
        13.0,
    );
    add_centered_label(
        mtm,
        &view,
        "Config: ~/.config/tarmac/init.lua",
        WIN_H - 300.0,
        13.0,
    );
    add_centered_label(
        mtm,
        &view,
        "Updates: follow the repository releases and changelog",
        WIN_H - 360.0,
        13.0,
    );

    view
}

fn derive_rule_inspector_state(
    rows: &[RuleRow],
    selected_rule_id: Option<&str>,
    draft_rule: Option<&WindowRule>,
) -> RuleInspectorState {
    let Some(selected_rule_id) = selected_rule_id else {
        return RuleInspectorState {
            selected_rule_id: None,
            editable: false,
            source_label: String::new(),
            can_duplicate: false,
            can_delete: false,
            can_move_up: false,
            can_move_down: false,
            can_apply: false,
            geometry_enabled: false,
            rule: None,
        };
    };

    let Some(row) = rows.iter().find(|row| row.id == selected_rule_id) else {
        return RuleInspectorState {
            selected_rule_id: None,
            editable: false,
            source_label: String::new(),
            can_duplicate: false,
            can_delete: false,
            can_move_up: false,
            can_move_down: false,
            can_apply: false,
            geometry_enabled: false,
            rule: None,
        };
    };

    let rule = if row.editable {
        draft_rule.cloned().or_else(|| Some(row.rule.clone()))
    } else {
        Some(row.rule.clone())
    };
    let managed_ids = rows
        .iter()
        .filter(|row| row.editable)
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let managed_index = managed_ids.iter().position(|id| id == &row.id);
    let can_move_up = row.editable && managed_index.is_some_and(|index| index > 0);
    let can_move_down =
        row.editable && managed_index.is_some_and(|index| index + 1 < managed_ids.len());

    RuleInspectorState {
        selected_rule_id: Some(row.id.clone()),
        editable: row.editable,
        source_label: row.source_label().to_string(),
        can_duplicate: row.editable,
        can_delete: row.editable,
        can_move_up,
        can_move_down,
        can_apply: row.editable,
        geometry_enabled: row.editable && rule.as_ref().is_some_and(|rule| rule.geometry.is_some()),
        rule,
    }
}

fn apply_rule_inspector_state(ui: &RulesUiRefs, state: &RuleInspectorState) {
    let show_placeholder = state.rule.is_none();
    ui.placeholder_label.setHidden(!show_placeholder);
    ui.source_value
        .setStringValue(&NSString::from_str(&state.source_label));
    ui.duplicate_button.setEnabled(state.can_duplicate);
    ui.delete_button.setEnabled(state.can_delete);
    ui.move_up_button.setEnabled(state.can_move_up);
    ui.move_down_button.setEnabled(state.can_move_down);
    ui.apply_button.setEnabled(state.can_apply);

    let Some(rule) = state.rule.as_ref() else {
        set_text_field_value(&ui.name_field, "");
        set_text_field_value(&ui.app_name_field, "");
        set_text_field_value(&ui.bundle_id_field, "");
        set_text_field_value(&ui.title_field, "");
        set_text_field_value(&ui.workspace_field, "");
        set_text_field_value(&ui.geometry_x_field, "");
        set_text_field_value(&ui.geometry_y_field, "");
        set_text_field_value(&ui.geometry_width_field, "");
        set_text_field_value(&ui.geometry_height_field, "");
        ui.enabled_checkbox.setState(0);
        ui.floating_checkbox.setState(0);
        ui.custom_geometry_checkbox.setState(0);
        ui.app_name_mode_popup.selectItemAtIndex(0);
        ui.bundle_id_mode_popup.selectItemAtIndex(0);
        ui.title_mode_popup.selectItemAtIndex(0);
        set_rule_inspector_enabled(ui, false, false);
        return;
    };

    set_text_field_value(&ui.name_field, rule.name.as_deref().unwrap_or(""));
    ui.enabled_checkbox
        .setState(if rule.enabled { 1 } else { 0 });
    set_text_field_value(
        &ui.app_name_field,
        rule.app_name
            .as_ref()
            .map_or("", |pattern| pattern.value.as_str()),
    );
    ui.app_name_mode_popup
        .selectItemAtIndex(rule_pattern_mode_index(rule.app_name.as_ref()));
    set_text_field_value(
        &ui.bundle_id_field,
        rule.app_bundle
            .as_ref()
            .map_or("", |pattern| pattern.value.as_str()),
    );
    ui.bundle_id_mode_popup
        .selectItemAtIndex(rule_pattern_mode_index(rule.app_bundle.as_ref()));
    set_text_field_value(
        &ui.title_field,
        rule.title
            .as_ref()
            .map_or("", |pattern| pattern.value.as_str()),
    );
    ui.title_mode_popup
        .selectItemAtIndex(rule_pattern_mode_index(rule.title.as_ref()));
    ui.floating_checkbox
        .setState(if rule.floating.unwrap_or(false) { 1 } else { 0 });
    set_text_field_value(&ui.workspace_field, rule.workspace.as_deref().unwrap_or(""));
    ui.custom_geometry_checkbox
        .setState(if rule.geometry.is_some() { 1 } else { 0 });
    if let Some((x, y, width, height)) = rule.geometry {
        set_text_field_value(&ui.geometry_x_field, &format_number_field(x));
        set_text_field_value(&ui.geometry_y_field, &format_number_field(y));
        set_text_field_value(&ui.geometry_width_field, &format_number_field(width));
        set_text_field_value(&ui.geometry_height_field, &format_number_field(height));
    } else {
        set_text_field_value(&ui.geometry_x_field, "");
        set_text_field_value(&ui.geometry_y_field, "");
        set_text_field_value(&ui.geometry_width_field, "");
        set_text_field_value(&ui.geometry_height_field, "");
    }

    set_rule_inspector_enabled(ui, state.editable, state.geometry_enabled);
}

fn set_rule_inspector_enabled(ui: &RulesUiRefs, editable: bool, geometry_enabled: bool) {
    ui.name_field.setEnabled(editable);
    ui.enabled_checkbox.setEnabled(editable);
    ui.app_name_field.setEnabled(editable);
    ui.app_name_mode_popup.setEnabled(editable);
    ui.bundle_id_field.setEnabled(editable);
    ui.bundle_id_mode_popup.setEnabled(editable);
    ui.title_field.setEnabled(editable);
    ui.title_mode_popup.setEnabled(editable);
    ui.floating_checkbox.setEnabled(editable);
    ui.workspace_field.setEnabled(editable);
    ui.custom_geometry_checkbox.setEnabled(editable);
    ui.geometry_x_field.setEnabled(editable && geometry_enabled);
    ui.geometry_y_field.setEnabled(editable && geometry_enabled);
    ui.geometry_width_field
        .setEnabled(editable && geometry_enabled);
    ui.geometry_height_field
        .setEnabled(editable && geometry_enabled);
}

fn derive_workspace_inspector_state(
    rows: &[WorkspaceRow],
    selected_workspace_id: Option<&str>,
    draft_workspace: Option<&WorkspaceDraft>,
) -> WorkspaceInspectorState {
    let Some(selected_workspace_id) = selected_workspace_id else {
        return WorkspaceInspectorState {
            editable: false,
            source_label: String::new(),
            kind_label: String::new(),
            can_copy_to_managed: false,
            can_delete: false,
            delete_label: "Delete".to_string(),
            can_apply: false,
            is_special: false,
            draft: None,
        };
    };

    let Some(row) = rows.iter().find(|row| row.id == selected_workspace_id) else {
        return WorkspaceInspectorState {
            editable: false,
            source_label: String::new(),
            kind_label: String::new(),
            can_copy_to_managed: false,
            can_delete: false,
            delete_label: "Delete".to_string(),
            can_apply: false,
            is_special: false,
            draft: None,
        };
    };

    let is_special = row.kind == WorkspaceKind::Special;
    let delete_label = match row.kind {
        WorkspaceKind::Numbered if row.editable => "Reset Override".to_string(),
        _ => "Delete".to_string(),
    };

    WorkspaceInspectorState {
        editable: row.editable,
        source_label: row.source_label().to_string(),
        kind_label: row.kind_label().to_string(),
        can_copy_to_managed: !row.editable,
        can_delete: row.editable,
        delete_label,
        can_apply: row.editable,
        is_special,
        draft: if row.editable {
            draft_workspace
                .cloned()
                .or_else(|| Some(WorkspaceDraft::from_row(row)))
        } else {
            Some(WorkspaceDraft::from_row(row))
        },
    }
}

fn apply_workspace_inspector_state(
    ui: &WorkspacesUiRefs,
    state: &WorkspaceInspectorState,
    displays: &[WorkspaceDisplayOption],
) {
    let show_placeholder = state.draft.is_none();
    ui.placeholder_label.setHidden(!show_placeholder);
    ui.source_value
        .setStringValue(&NSString::from_str(&state.source_label));
    ui.kind_value
        .setStringValue(&NSString::from_str(&state.kind_label));
    ui.copy_button.setEnabled(state.can_copy_to_managed);
    ui.delete_button.setEnabled(state.can_delete);
    ui.delete_button
        .setTitle(&NSString::from_str(&state.delete_label));
    ui.apply_button.setEnabled(state.can_apply);

    let Some(draft) = state.draft.as_ref() else {
        ui.workspace_id_value
            .setStringValue(&NSString::from_str(""));
        rebuild_monitor_popup(&ui.monitor_popup, displays, None);
        ui.layout_popup.selectItemAtIndex(0);
        set_text_field_value(&ui.gap_inner_field, "");
        set_text_field_value(&ui.gap_outer_field, "");
        ui.overlay_position_popup.selectItemAtIndex(0);
        set_text_field_value(&ui.overlay_width_field, "");
        set_text_field_value(&ui.overlay_height_field, "");
        set_workspace_inspector_enabled(ui, false, false);
        return;
    };

    ui.workspace_id_value
        .setStringValue(&NSString::from_str(&draft.id));
    rebuild_monitor_popup(&ui.monitor_popup, displays, draft.monitor_display_id);
    ui.layout_popup.selectItemAtIndex(match draft.layout {
        WorkspaceLayout::Bsp => 0,
    });
    set_text_field_value(
        &ui.gap_inner_field,
        &draft.gap_inner.map(format_number_field).unwrap_or_default(),
    );
    set_text_field_value(
        &ui.gap_outer_field,
        &draft.gap_outer.map(format_number_field).unwrap_or_default(),
    );
    ui.overlay_position_popup
        .selectItemAtIndex(match draft.overlay_position.as_str() {
            "top" => 1,
            "bottom" => 2,
            _ => 0,
        });
    set_text_field_value(
        &ui.overlay_width_field,
        &format_number_field(draft.overlay_width),
    );
    set_text_field_value(
        &ui.overlay_height_field,
        &format_number_field(draft.overlay_height),
    );
    set_workspace_inspector_enabled(ui, state.editable, state.is_special);
}

fn set_workspace_inspector_enabled(ui: &WorkspacesUiRefs, editable: bool, is_special: bool) {
    ui.monitor_popup.setEnabled(editable);
    ui.layout_popup.setEnabled(editable);
    ui.gap_inner_field.setEnabled(editable);
    ui.gap_outer_field.setEnabled(editable);
    ui.overlay_position_popup.setEnabled(editable && is_special);
    ui.overlay_width_field.setEnabled(editable && is_special);
    ui.overlay_height_field.setEnabled(editable && is_special);
    ui.overlay_section_label.setHidden(!is_special);
    ui.overlay_position_popup.setHidden(!is_special);
    ui.overlay_width_field.setHidden(!is_special);
    ui.overlay_height_field.setHidden(!is_special);
}

fn add_keybind_table_column(
    mtm: MainThreadMarker,
    table: &NSTableView,
    identifier: &str,
    title: &str,
    width: CGFloat,
) {
    let identifier = NSString::from_str(identifier);
    let column: Retained<NSTableColumn> =
        unsafe { msg_send![NSTableColumn::alloc(mtm), initWithIdentifier: &*identifier] };
    column.setTitle(&NSString::from_str(title));
    column.setWidth(width);
    column.setMinWidth(width.min(180.0));
    table.addTableColumn(&column);
}

fn add_workspace_table_column(
    mtm: MainThreadMarker,
    table: &NSTableView,
    identifier: &str,
    title: &str,
    width: CGFloat,
) {
    let identifier = NSString::from_str(identifier);
    let column: Retained<NSTableColumn> =
        unsafe { msg_send![NSTableColumn::alloc(mtm), initWithIdentifier: &*identifier] };
    column.setTitle(&NSString::from_str(title));
    column.setWidth(width);
    column.setMinWidth(width.min(180.0));
    table.addTableColumn(&column);
}

fn add_rule_table_column(
    mtm: MainThreadMarker,
    table: &NSTableView,
    identifier: &str,
    title: &str,
    width: CGFloat,
) {
    let identifier = NSString::from_str(identifier);
    let column: Retained<NSTableColumn> =
        unsafe { msg_send![NSTableColumn::alloc(mtm), initWithIdentifier: &*identifier] };
    column.setTitle(&NSString::from_str(title));
    column.setWidth(width);
    column.setMinWidth(width.min(180.0));
    table.addTableColumn(&column);
}

fn add_rule_list_checkbox(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    row: usize,
    checked: bool,
    enabled: bool,
) -> Retained<NSButton> {
    let frame = CGRect::new(CGPoint::new(8.0, 2.0), CGSize::new(24.0, 20.0));
    let checkbox: Retained<NSButton> =
        unsafe { msg_send![NSButton::alloc(mtm), initWithFrame: frame] };
    checkbox.setButtonType(objc2_app_kit::NSButtonType::Switch);
    checkbox.setTitle(&NSString::from_str(""));
    checkbox.setState(if checked { 1 } else { 0 });
    checkbox.setEnabled(enabled);
    checkbox.setTag(row as NSInteger);
    unsafe {
        checkbox.setTarget(Some(handler));
        checkbox.setAction(Some(sel!(onRuleRowEnabledToggled:)));
    }
    parent.addSubview(&checkbox);
    checkbox
}

fn add_table_label(mtm: MainThreadMarker, parent: &NSView, text: &str, width: f64) {
    let label_frame = CGRect::new(
        CGPoint::new(4.0, 2.0),
        CGSize::new((width - 8.0).max(20.0), 20.0),
    );
    let label: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: label_frame] };
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label.setStringValue(&NSString::from_str(text));
    unsafe {
        let _: () = msg_send![&*label, setLineBreakMode: 4_isize];
    }
    parent.addSubview(&label);
}

fn add_centered_label(mtm: MainThreadMarker, parent: &NSView, text: &str, y: f64, size: f64) {
    let frame = CGRect::new(CGPoint::new(180.0, y), CGSize::new(WIN_W - 360.0, 28.0));
    let label: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    unsafe {
        let _: () = msg_send![&*label, setAlignment: 1_isize];
        let font: Retained<objc2_app_kit::NSFont> =
            msg_send![objc2_app_kit::NSFont::class(), systemFontOfSize: size];
        label.setFont(Some(&font));
    }
    parent.addSubview(&label);
}

fn add_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(180.0, 22.0));
    let label: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    parent.addSubview(&label);
}

fn add_wrapped_label(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) {
    let label = add_wrapped_label_field(mtm, parent, text, x, y, width, height);
    let _ = label;
}

fn add_wrapped_label_field(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Retained<NSTextField> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(width, height));
    let label: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    unsafe {
        let _: () = msg_send![&*label, setLineBreakMode: 0_isize];
    }
    parent.addSubview(&label);
    label
}

fn add_section_label_field(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
) -> Retained<NSTextField> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(260.0, 24.0));
    let label: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    unsafe {
        let font: Retained<objc2_app_kit::NSFont> =
            msg_send![objc2_app_kit::NSFont::class(), boldSystemFontOfSize: 13.0_f64];
        label.setFont(Some(&font));
    }
    parent.addSubview(&label);
    label
}

fn add_section_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
    let _ = add_section_label_field(mtm, parent, text, x, y);
}

fn add_value_label(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
) -> Retained<NSTextField> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(80.0, 22.0));
    let label: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] };
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label.setStringValue(&NSString::from_str(text));
    parent.addSubview(&label);
    label
}

#[allow(clippy::too_many_arguments)]
fn add_slider(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    label: &str,
    label_x: f64,
    slider_x: f64,
    y: f64,
    width: f64,
    min: f64,
    max: f64,
    value: f64,
    action: objc2::runtime::Sel,
) -> Retained<NSSlider> {
    add_label(mtm, parent, label, label_x, y);
    let frame = CGRect::new(CGPoint::new(slider_x, y - 2.0), CGSize::new(width, 24.0));
    let slider: Retained<NSSlider> =
        unsafe { msg_send![NSSlider::alloc(mtm), initWithFrame: frame] };
    slider.setMinValue(min);
    slider.setMaxValue(max);
    slider.setDoubleValue(value);
    unsafe {
        slider.setTarget(Some(handler));
        slider.setAction(Some(action));
    }
    parent.addSubview(&slider);
    slider
}

fn add_color_well(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    x: f64,
    y: f64,
    action: objc2::runtime::Sel,
) -> Retained<NSColorWell> {
    let frame = CGRect::new(CGPoint::new(x, y - 2.0), CGSize::new(60.0, 26.0));
    let well: Retained<NSColorWell> =
        unsafe { msg_send![NSColorWell::alloc(mtm), initWithFrame: frame] };
    unsafe {
        well.setTarget(Some(handler));
        well.setAction(Some(action));
    }
    parent.addSubview(&well);
    well
}

fn add_checkbox(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    text: &str,
    x: f64,
    y: f64,
    action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(280.0, 24.0));
    let button: Retained<NSButton> =
        unsafe { msg_send![NSButton::alloc(mtm), initWithFrame: frame] };
    button.setButtonType(objc2_app_kit::NSButtonType::Switch);
    button.setTitle(&NSString::from_str(text));
    unsafe {
        button.setTarget(Some(handler));
        button.setAction(Some(action));
    }
    parent.addSubview(&button);
    button
}

#[allow(clippy::too_many_arguments)]
fn add_popup(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    x: f64,
    y: f64,
    width: f64,
    items: &[&str],
    action: objc2::runtime::Sel,
) -> Retained<NSPopUpButton> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(width, 26.0));
    let popup: Retained<NSPopUpButton> =
        unsafe { msg_send![NSPopUpButton::alloc(mtm), initWithFrame: frame, pullsDown: false] };
    for item in items {
        popup.addItemWithTitle(&NSString::from_str(item));
    }
    unsafe {
        popup.setTarget(Some(handler));
        popup.setAction(Some(action));
    }
    parent.addSubview(&popup);
    popup
}

#[allow(clippy::too_many_arguments)]
fn add_input_field(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    x: f64,
    y: f64,
    width: f64,
    placeholder: &str,
    action: objc2::runtime::Sel,
) -> Retained<NSTextField> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(width, 24.0));
    let field: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] };
    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    unsafe {
        field.setTarget(Some(handler));
        field.setAction(Some(action));
    }
    parent.addSubview(&field);
    field
}

#[allow(clippy::too_many_arguments)]
fn add_button(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    title: &str,
    x: f64,
    y: f64,
    width: f64,
    action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(width, 28.0));
    let button: Retained<NSButton> =
        unsafe { msg_send![NSButton::alloc(mtm), initWithFrame: frame] };
    button.setTitle(&NSString::from_str(title));
    unsafe {
        button.setTarget(Some(handler));
        button.setAction(Some(action));
    }
    parent.addSubview(&button);
    button
}

fn set_text_field_value(text_field: &NSTextField, content: &str) {
    text_field.setStringValue(&NSString::from_str(content));
}

fn set_value_label(label: &NSTextField, value: f64) {
    label.setStringValue(&NSString::from_str(&format!("{value:.0}")));
}

fn set_color_well(well: &NSColorWell, hex: &str) {
    let color = color_from_hex(hex);
    well.setColor(&color);
}

fn color_well_to_hex(well: &NSColorWell) -> String {
    let color = well.color();
    let r = (color.redComponent() * 255.0).round() as u8;
    let g = (color.greenComponent() * 255.0).round() as u8;
    let b = (color.blueComponent() * 255.0).round() as u8;
    let a = (color.alphaComponent() * 255.0).round() as u8;
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

fn color_from_hex(hex: &str) -> Retained<objc2_app_kit::NSColor> {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(hex.get(0..2).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0;
    let g = u8::from_str_radix(hex.get(2..4).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0;
    let b = u8::from_str_radix(hex.get(4..6).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0;
    let a = if hex.len() >= 8 {
        u8::from_str_radix(&hex[6..8], 16).unwrap_or(255) as f64 / 255.0
    } else {
        1.0
    };
    unsafe {
        msg_send![
            objc2_app_kit::NSColor::class(),
            colorWithSRGBRed: r,
            green: g,
            blue: b,
            alpha: a
        ]
    }
}

fn trimmed_optional_text(field: &NSTextField) -> Option<String> {
    let value = field.stringValue().to_string();
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn rule_pattern_mode_index(pattern: Option<&RulePattern>) -> NSInteger {
    match pattern
        .map(|pattern| pattern.mode)
        .unwrap_or(RuleMatchMode::Contains)
    {
        RuleMatchMode::Contains => 0,
        RuleMatchMode::Exact => 1,
        RuleMatchMode::Regex => 2,
    }
}

fn popup_rule_match_mode(popup: &NSPopUpButton) -> RuleMatchMode {
    match popup.indexOfSelectedItem() {
        1 => RuleMatchMode::Exact,
        2 => RuleMatchMode::Regex,
        _ => RuleMatchMode::Contains,
    }
}

fn read_rule_pattern(
    field: &NSTextField,
    popup: &NSPopUpButton,
    default_mode: RuleMatchMode,
) -> Option<RulePattern> {
    let value = trimmed_optional_text(field)?;
    let mode = popup_rule_match_mode(popup);
    Some(RulePattern::new(
        value,
        if field.stringValue().is_empty() {
            default_mode
        } else {
            mode
        },
    ))
}

fn parse_f64_field(field: &NSTextField, fallback: f64) -> f64 {
    field
        .stringValue()
        .to_string()
        .trim()
        .parse::<f64>()
        .unwrap_or(fallback)
}

fn parse_optional_f64_field(field: &NSTextField) -> Option<f64> {
    let value = field.stringValue().to_string();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        trimmed.parse::<f64>().ok()
    }
}

fn format_number_field(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn reset_popup_items(popup: &NSPopUpButton, items: &[String], selected_index: NSInteger) {
    popup.removeAllItems();
    for item in items {
        popup.addItemWithTitle(&NSString::from_str(item));
    }
    popup.selectItemAtIndex(selected_index.max(0));
}

fn rebuild_monitor_popup(
    popup: &NSPopUpButton,
    displays: &[WorkspaceDisplayOption],
    selected_display_id: Option<u32>,
) {
    let mut items = vec!["No preference".to_string()];
    items.extend(displays.iter().map(|display| display.label.clone()));

    let selected_index = if let Some(display_id) = selected_display_id {
        if let Some(index) = displays
            .iter()
            .position(|display| display.display_id == display_id)
        {
            index as NSInteger + 1
        } else {
            items.push(format!("Disconnected ({display_id})"));
            items.len() as NSInteger - 1
        }
    } else {
        0
    };

    reset_popup_items(popup, &items, selected_index);
}

fn popup_selected_display_id(popup: &NSPopUpButton) -> Option<u32> {
    let title = popup.titleOfSelectedItem()?.to_string();
    let start = title.rfind('(')? + 1;
    let end = title.rfind(')')?;
    title[start..end].parse::<u32>().ok()
}

fn popup_overlay_position(popup: &NSPopUpButton) -> String {
    match popup.indexOfSelectedItem() {
        1 => "top".to_string(),
        2 => "bottom".to_string(),
        _ => "center".to_string(),
    }
}

fn validate_lettered_workspace_id(value: &str, existing_ids: &[String]) -> Option<String> {
    let trimmed = value.trim();
    let mut chars = trimmed.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || !ch.is_ascii_alphabetic() {
        return None;
    }
    let letter = ch.to_ascii_uppercase().to_string();
    (!existing_ids
        .iter()
        .any(|id| id.eq_ignore_ascii_case(&letter)))
    .then_some(letter)
}

fn validate_special_workspace_name(value: &str, existing_ids: &[String]) -> Option<String> {
    let trimmed = value
        .trim()
        .strip_prefix("special:")
        .unwrap_or(value.trim());
    if trimmed.is_empty() {
        return None;
    }
    let id = format!("special:{trimmed}");
    (!existing_ids.iter().any(|existing| existing == &id)).then(|| trimmed.to_string())
}

fn prompt_for_workspace_text(
    mtm: MainThreadMarker,
    title: &str,
    message: &str,
    placeholder: &str,
    validate: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let alert: Retained<objc2_app_kit::NSAlert> =
        unsafe { msg_send![objc2_app_kit::NSAlert::alloc(mtm), init] };
    let title = NSString::from_str(title);
    let message = NSString::from_str(message);
    unsafe {
        let _: () = msg_send![&*alert, setMessageText: &*title];
        let _: () = msg_send![&*alert, setInformativeText: &*message];
        let _: Retained<NSButton> =
            msg_send![&*alert, addButtonWithTitle: &*NSString::from_str("OK")];
        let _: Retained<NSButton> =
            msg_send![&*alert, addButtonWithTitle: &*NSString::from_str("Cancel")];
    }

    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(240.0, 24.0));
    let input: Retained<NSTextField> =
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] };
    input.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    unsafe {
        let _: () = msg_send![&*alert, setAccessoryView: &*input];
        let response: NSInteger = msg_send![&*alert, runModal];
        if response != 1000 {
            return None;
        }
    }

    validate(&input.stringValue().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::document::ConfigSource;
    use crate::config::lua::LuaKeybind;
    use crate::core::input::{Action, Key, Modifiers};

    fn managed_row(id: &str, geometry: Option<(f64, f64, f64, f64)>) -> RuleRow {
        RuleRow {
            id: id.to_string(),
            name: format!("Rule {id}"),
            enabled: true,
            source: ConfigSource::Managed,
            editable: true,
            rule: WindowRule {
                id: Some(id.to_string()),
                name: Some(format!("Rule {id}")),
                enabled: true,
                app_name: None,
                app_bundle: None,
                title: None,
                floating: None,
                workspace: None,
                geometry,
            },
        }
    }

    fn managed_keybind_row(id: &str) -> KeybindRow {
        KeybindRow {
            id: id.to_string(),
            shortcut: "mod+shift+period".to_string(),
            action: "reload".to_string(),
            source: ConfigSource::Managed,
            editable: true,
            keybind: LuaKeybind {
                modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
                key: Key::Period,
                action: Action::Reload,
            },
        }
    }

    fn external_keybind_row(id: &str, source: ConfigSource) -> KeybindRow {
        KeybindRow {
            id: id.to_string(),
            shortcut: "mod+return".to_string(),
            action: "spawn_terminal".to_string(),
            source,
            editable: false,
            keybind: LuaKeybind {
                modifiers: Modifiers::COMMAND,
                key: Key::Return,
                action: Action::SpawnTerminal,
            },
        }
    }

    fn lua_row(id: &str) -> RuleRow {
        RuleRow {
            id: id.to_string(),
            name: format!("Lua {id}"),
            enabled: true,
            source: ConfigSource::Lua,
            editable: false,
            rule: WindowRule {
                id: Some(id.to_string()),
                name: Some(format!("Lua {id}")),
                enabled: true,
                app_name: None,
                app_bundle: None,
                title: None,
                floating: Some(true),
                workspace: Some("special:web".to_string()),
                geometry: None,
            },
        }
    }

    fn workspace_row(id: &str, source: ConfigSource, kind: WorkspaceKind) -> WorkspaceRow {
        WorkspaceRow {
            id: id.to_string(),
            kind,
            source,
            editable: source == ConfigSource::Managed,
            definition: WorkspaceDefinition {
                id: crate::core::workspace::WorkspaceId::parse(id).expect("invalid workspace id"),
                kind,
                prefs: crate::core::workspace::WorkspacePrefs {
                    monitor: None,
                    default_layout: WorkspaceLayout::Bsp,
                    gap_inner: None,
                    gap_outer: None,
                },
            },
            special: (kind == WorkspaceKind::Special).then(|| SpecialWorkspaceConfig {
                name: id.trim_start_matches("special:").to_string(),
                position: "center".to_string(),
                width: 0.7,
                height: 0.7,
            }),
        }
    }

    #[test]
    fn selecting_lua_rule_is_read_only() {
        let rows = vec![managed_row("rule_001", None), lua_row("lua_rule")];
        let state = derive_rule_inspector_state(&rows, Some("lua_rule"), None);
        assert!(!state.editable);
        assert!(!state.can_apply);
        assert_eq!(state.source_label, "Lua");
    }

    #[test]
    fn selecting_managed_rule_is_editable() {
        let rows = vec![managed_row("rule_001", None), lua_row("lua_rule")];
        let state = derive_rule_inspector_state(&rows, Some("rule_001"), None);
        assert!(state.editable);
        assert!(state.can_apply);
        assert_eq!(state.source_label, "Managed");
    }

    #[test]
    fn geometry_enabled_tracks_draft_geometry() {
        let rows = vec![managed_row("rule_001", None)];
        let draft = WindowRule {
            geometry: Some((10.0, 20.0, 800.0, 600.0)),
            ..rows[0].rule.clone()
        };
        let state = derive_rule_inspector_state(&rows, Some("rule_001"), Some(&draft));
        assert!(state.geometry_enabled);
        assert_eq!(state.rule.expect("rule missing").geometry, draft.geometry);
    }

    #[test]
    fn selecting_external_keybind_is_read_only() {
        let rows = vec![
            managed_keybind_row("managed:0"),
            external_keybind_row("default:0", ConfigSource::Default),
        ];
        let state = derive_keybind_inspector_state(&rows, Some("default:0"), None);
        assert!(!state.editable);
        assert!(state.can_copy_to_managed);
        assert!(!state.can_apply);
        assert_eq!(state.source_label, "Default");
    }

    #[test]
    fn selecting_managed_keybind_is_editable() {
        let rows = vec![
            managed_keybind_row("managed:0"),
            external_keybind_row("lua:0", ConfigSource::Lua),
        ];
        let state = derive_keybind_inspector_state(&rows, Some("managed:0"), None);
        assert!(state.editable);
        assert!(!state.can_copy_to_managed);
        assert!(state.can_apply);
        assert_eq!(state.source_label, "Managed");
    }

    #[test]
    fn keybind_draft_prefers_local_edits() {
        let rows = vec![managed_keybind_row("managed:0")];
        let draft = KeybindDraft {
            shortcut: "mod+shift+comma".to_string(),
            action: "exit".to_string(),
        };
        let state = derive_keybind_inspector_state(&rows, Some("managed:0"), Some(&draft));
        assert_eq!(state.draft, Some(draft));
    }

    #[test]
    fn selecting_default_workspace_is_read_only() {
        let rows = vec![
            workspace_row("1", ConfigSource::Default, WorkspaceKind::Numbered),
            workspace_row("A", ConfigSource::Managed, WorkspaceKind::Lettered),
        ];
        let state = derive_workspace_inspector_state(&rows, Some("1"), None);
        assert!(!state.editable);
        assert!(state.can_copy_to_managed);
        assert!(!state.can_apply);
        assert_eq!(state.source_label, "Default");
    }

    #[test]
    fn selecting_managed_workspace_is_editable() {
        let rows = vec![
            workspace_row("1", ConfigSource::Default, WorkspaceKind::Numbered),
            workspace_row("A", ConfigSource::Managed, WorkspaceKind::Lettered),
        ];
        let state = derive_workspace_inspector_state(&rows, Some("A"), None);
        assert!(state.editable);
        assert!(state.can_delete);
        assert!(state.can_apply);
        assert_eq!(state.source_label, "Managed");
    }

    #[test]
    fn special_workspace_draft_enables_overlay_fields() {
        let rows = vec![workspace_row(
            "special:term",
            ConfigSource::Managed,
            WorkspaceKind::Special,
        )];
        let state = derive_workspace_inspector_state(&rows, Some("special:term"), None);
        assert!(state.is_special);
        assert_eq!(
            state.draft.expect("draft missing").overlay_position,
            "center".to_string()
        );
    }

    #[test]
    fn validate_lettered_workspace_normalizes_and_rejects_duplicates() {
        assert_eq!(
            validate_lettered_workspace_id("w", &["A".to_string()]),
            Some("W".to_string())
        );
        assert_eq!(
            validate_lettered_workspace_id("a", &["A".to_string()]),
            None
        );
    }

    #[test]
    fn validate_special_workspace_name_rejects_empty_and_duplicates() {
        assert_eq!(
            validate_special_workspace_name("terminal", &[]),
            Some("terminal".to_string())
        );
        assert_eq!(
            validate_special_workspace_name("special:terminal", &["special:terminal".to_string()]),
            None
        );
        assert_eq!(validate_special_workspace_name("   ", &[]), None);
    }
}
