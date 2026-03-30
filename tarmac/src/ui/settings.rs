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
    NSTextField, NSTextView, NSView, NSViewController, NSWindow, NSWindowStyleMask,
    NSWindowToolbarStyle,
};
use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
use objc2_foundation::{NSIndexSet, NSInteger, NSNotification, NSObject, NSString};

use crate::config::document::RuleRow;
use crate::config::lua::{RuleMatchMode, RulePattern, WindowRule};

const WIN_W: f64 = 920.0;
const WIN_H: f64 = 660.0;

const RULE_COLUMN_ENABLED: &str = "enabled";
const RULE_COLUMN_NAME: &str = "name";
const RULE_COLUMN_WHEN: &str = "when";
const RULE_COLUMN_THEN: &str = "then";
const RULE_COLUMN_SOURCE: &str = "source";

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
    ApplyManagedKeybinds(String),
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
    ApplyManagedWorkspaces(String),
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
    pub keybinds_external_text: String,
    pub keybinds_managed_text: String,
    pub rules: Vec<RuleRow>,
    pub selected_rule_id: Option<String>,
    pub workspaces_external_text: String,
    pub workspaces_managed_text: String,
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

struct SettingsHandlerIvars {
    tx: mpsc::Sender<SettingsAction>,
    keybinds_editor: RefCell<Option<Retained<NSTextView>>>,
    workspaces_editor: RefCell<Option<Retained<NSTextView>>>,
    rule_rows: RefCell<Vec<RuleRow>>,
    selected_rule_id: RefCell<Option<String>>,
    draft_rule: RefCell<Option<WindowRule>>,
    suppress_rule_selection_change: RefCell<bool>,
    rules_ui: RefCell<Option<RulesUiRefs>>,
}

impl SettingsHandler {
    fn new(mtm: MainThreadMarker, tx: mpsc::Sender<SettingsAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(SettingsHandlerIvars {
            tx,
            keybinds_editor: RefCell::new(None),
            workspaces_editor: RefCell::new(None),
            rule_rows: RefCell::new(Vec::new()),
            selected_rule_id: RefCell::new(None),
            draft_rule: RefCell::new(None),
            suppress_rule_selection_change: RefCell::new(false),
            rules_ui: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, action: SettingsAction) {
        let _ = self.ivars().tx.send(action);
    }

    fn set_keybinds_editor(&self, editor: Retained<NSTextView>) {
        *self.ivars().keybinds_editor.borrow_mut() = Some(editor);
    }

    fn set_workspaces_editor(&self, editor: Retained<NSTextView>) {
        *self.ivars().workspaces_editor.borrow_mut() = Some(editor);
    }

    fn set_rules_ui(&self, ui: RulesUiRefs) {
        *self.ivars().rules_ui.borrow_mut() = Some(ui);
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

    fn current_rule_inspector_state(&self) -> RuleInspectorState {
        let rows = self.ivars().rule_rows.borrow().clone();
        let selected_rule_id = self.ivars().selected_rule_id.borrow().clone();
        let draft_rule = self.ivars().draft_rule.borrow().clone();
        derive_rule_inspector_state(&rows, selected_rule_id.as_deref(), draft_rule.as_ref())
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

    fn refresh_rule_inspector(&self) {
        let state = self.current_rule_inspector_state();
        let ui_borrow = self.ivars().rules_ui.borrow();
        let Some(ui) = ui_borrow.as_ref() else { return };
        apply_rule_inspector_state(ui, &state);
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

        #[unsafe(method(onApplyManagedKeybinds:))]
        fn on_apply_managed_keybinds(&self, _sender: Option<&AnyObject>) {
            let text = {
                let editor = self.ivars().keybinds_editor.borrow();
                let Some(editor) = editor.as_ref() else { return };
                editor.string().to_string()
            };
            self.emit(SettingsAction::ApplyManagedKeybinds(text));
        }

        #[unsafe(method(onResetManagedKeybinds:))]
        fn on_reset_managed_keybinds(&self, _sender: Option<&AnyObject>) {
            self.emit(SettingsAction::ResetManagedKeybinds);
        }

        #[unsafe(method(onApplyManagedWorkspaces:))]
        fn on_apply_managed_workspaces(&self, _sender: Option<&AnyObject>) {
            let text = {
                let editor = self.ivars().workspaces_editor.borrow();
                let Some(editor) = editor.as_ref() else { return };
                editor.string().to_string()
            };
            self.emit(SettingsAction::ApplyManagedWorkspaces(text));
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
            self.ivars().rule_rows.borrow().len() as NSInteger
        }

        #[unsafe(method(tableView:viewForTableColumn:row:))]
        fn table_view_view_for_table_column_row(
            &self,
            _table_view: &NSTableView,
            table_column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> *mut NSView {
            let mtm = self.mtm();
            let Some(row) = usize::try_from(row).ok() else {
                return std::ptr::null_mut();
            };
            let Some(row_data) = self.ivars().rule_rows.borrow().get(row).cloned() else {
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
    keybinds_external: Retained<NSTextView>,
    keybinds_editor: Retained<NSTextView>,
    workspaces_external: Retained<NSTextView>,
    workspaces_editor: Retained<NSTextView>,
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
        let keybindings = build_editor_tab(
            mtm,
            &handler,
            "Current non-managed rows are shown above. Edit the managed block below using `key_spec | action` per line.",
            "Read-only rows (effective config)",
            "Managed rows",
            Some(sel!(onApplyManagedKeybinds:)),
            Some(sel!(onResetManagedKeybinds:)),
            "Apply",
            "Reset Defaults",
        );
        handler.set_keybinds_editor(keybindings.editor.clone());

        let rules = build_rules_tab(mtm, &handler);
        handler.set_rules_ui(rules.ui);

        let workspaces = build_editor_tab(
            mtm,
            &handler,
            "Use `id | monitor=<display_id> layout=bsp gap_inner=<n> gap_outer=<n>` or `special:name | ... overlay.position=<pos> overlay.width=<f> overlay.height=<f>`.",
            "Read-only rows (effective config)",
            "Managed rows",
            Some(sel!(onApplyManagedWorkspaces:)),
            None,
            "Apply",
            "",
        );
        handler.set_workspaces_editor(workspaces.editor.clone());

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
            keybinds_external: keybindings.read_only,
            keybinds_editor: keybindings.editor,
            workspaces_external: workspaces.read_only,
            workspaces_editor: workspaces.editor,
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
        set_text_view(&self.keybinds_external, &snapshot.keybinds_external_text);
        set_text_view(&self.keybinds_editor, &snapshot.keybinds_managed_text);
        self.handler
            .load_rules(snapshot.rules.clone(), snapshot.selected_rule_id.clone());
        set_text_view(
            &self.workspaces_external,
            &snapshot.workspaces_external_text,
        );
        set_text_view(&self.workspaces_editor, &snapshot.workspaces_managed_text);
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

struct EditorTab {
    root: Retained<NSView>,
    read_only: Retained<NSTextView>,
    editor: Retained<NSTextView>,
}

struct RulesTab {
    root: Retained<NSView>,
    ui: RulesUiRefs,
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

#[allow(clippy::too_many_arguments)]
fn build_editor_tab(
    mtm: MainThreadMarker,
    handler: &SettingsHandler,
    description: &str,
    read_only_title: &str,
    editor_title: &str,
    primary_action: Option<objc2::runtime::Sel>,
    secondary_action: Option<objc2::runtime::Sel>,
    primary_label: &str,
    secondary_label: &str,
) -> EditorTab {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, WIN_H));
    let root: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    add_wrapped_label(
        mtm,
        &root,
        description,
        28.0,
        WIN_H - 56.0,
        WIN_W - 56.0,
        32.0,
    );
    add_section_label(mtm, &root, read_only_title, 28.0, WIN_H - 108.0);
    let read_only = add_text_editor(mtm, &root, 28.0, WIN_H - 324.0, WIN_W - 56.0, 190.0, false);
    add_section_label(mtm, &root, editor_title, 28.0, WIN_H - 360.0);
    let editor = add_text_editor(mtm, &root, 28.0, 90.0, WIN_W - 56.0, 230.0, true);

    if let Some(primary_action) = primary_action {
        let button = add_button(
            mtm,
            &root,
            handler,
            primary_label,
            WIN_W - 160.0,
            34.0,
            120.0,
            primary_action,
        );
        let _ = button;
    }
    if let Some(secondary_action) = secondary_action {
        let button = add_button(
            mtm,
            &root,
            handler,
            secondary_label,
            WIN_W - 300.0,
            34.0,
            120.0,
            secondary_action,
        );
        let _ = button;
    }

    EditorTab {
        root,
        read_only,
        editor,
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

fn add_section_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
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

fn add_text_editor(
    mtm: MainThreadMarker,
    parent: &NSView,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    editable: bool,
) -> Retained<NSTextView> {
    let scroll_frame = CGRect::new(CGPoint::new(x, y), CGSize::new(width, height));
    let scroll: Retained<NSScrollView> =
        unsafe { msg_send![NSScrollView::alloc(mtm), initWithFrame: scroll_frame] };
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(objc2_app_kit::NSBorderType(2));

    let text_frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width - 24.0, height));
    let text: Retained<NSTextView> =
        unsafe { msg_send![NSTextView::alloc(mtm), initWithFrame: text_frame] };
    text.setEditable(editable);
    unsafe {
        let mono: Retained<objc2_app_kit::NSFont> = msg_send![
            objc2_app_kit::NSFont::class(),
            monospacedSystemFontOfSize: 11.0_f64,
            weight: 0.0_f64
        ];
        text.setFont(Some(&mono));
    }

    scroll.setDocumentView(Some(&text));
    parent.addSubview(&scroll);
    text
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

fn set_text_view(text_view: &NSTextView, content: &str) {
    text_view.setString(&NSString::from_str(content));
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

fn format_number_field(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::document::ConfigSource;

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
}
