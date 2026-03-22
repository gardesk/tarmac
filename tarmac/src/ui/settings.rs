use std::sync::mpsc;

use objc2::rc::Retained;
use objc2::{
    define_class, msg_send, sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColorWell, NSPopUpButton, NSScrollView, NSSegmentedControl,
    NSSlider, NSTextField, NSTextView, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObject, NSString};

/// Actions emitted by settings controls.
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
    Open,
}

/// Current settings snapshot for populating controls.
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
    pub keybinds: Vec<(String, String)>, // (key combo, action)
    pub rules: Vec<(String, String)>,    // (match, action)
}

// --- ObjC action handler ---

struct SettingsHandlerIvars {
    tx: mpsc::Sender<SettingsAction>,
    tab_views: std::cell::RefCell<Vec<Retained<NSView>>>,
    content_container: std::cell::RefCell<Option<Retained<NSView>>>,
}

impl SettingsHandler {
    fn new(mtm: MainThreadMarker, tx: mpsc::Sender<SettingsAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(SettingsHandlerIvars {
            tx,
            tab_views: std::cell::RefCell::new(Vec::new()),
            content_container: std::cell::RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, action: SettingsAction) {
        let _ = self.ivars().tx.send(action);
    }

    fn set_tab_views(&self, views: Vec<Retained<NSView>>, container: Retained<NSView>) {
        *self.ivars().tab_views.borrow_mut() = views;
        *self.ivars().content_container.borrow_mut() = Some(container);
    }

    fn switch_to_tab(&self, index: usize) {
        let views = self.ivars().tab_views.borrow();
        let container_ref = self.ivars().content_container.borrow();
        let Some(container) = container_ref.as_ref() else {
            return;
        };
        // Remove all subviews from container
        let subviews = container.subviews();
        for sv in subviews.iter() {
            sv.removeFromSuperview();
        }
        // Add the selected tab view
        if let Some(view) = views.get(index) {
            container.addSubview(view);
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
        #[unsafe(method(onTabChanged:))]
        fn on_tab_changed(&self, sender: Option<&NSSegmentedControl>) {
            if let Some(seg) = sender {
                let idx = seg.selectedSegment() as usize;
                self.switch_to_tab(idx);
            }
        }

        #[unsafe(method(onGapInnerChanged:))]
        fn on_gap_inner(&self, sender: Option<&NSSlider>) {
            if let Some(s) = sender {
                self.emit(SettingsAction::GapInner(s.doubleValue()));
            }
        }

        #[unsafe(method(onGapOuterChanged:))]
        fn on_gap_outer(&self, sender: Option<&NSSlider>) {
            if let Some(s) = sender {
                self.emit(SettingsAction::GapOuter(s.doubleValue()));
            }
        }

        #[unsafe(method(onBarHeightChanged:))]
        fn on_bar_height(&self, sender: Option<&NSSlider>) {
            if let Some(s) = sender {
                self.emit(SettingsAction::BarHeight(s.doubleValue()));
            }
        }

        #[unsafe(method(onBorderWidthChanged:))]
        fn on_border_width(&self, sender: Option<&NSSlider>) {
            if let Some(s) = sender {
                self.emit(SettingsAction::BorderWidth(s.doubleValue()));
            }
        }

        #[unsafe(method(onBorderRadiusChanged:))]
        fn on_border_radius(&self, sender: Option<&NSSlider>) {
            if let Some(s) = sender {
                self.emit(SettingsAction::BorderRadius(s.doubleValue()));
            }
        }

        #[unsafe(method(onBorderColorFocusedChanged:))]
        fn on_border_color_focused(&self, sender: Option<&NSColorWell>) {
            if let Some(well) = sender {
                let hex = color_well_to_hex(well);
                self.emit(SettingsAction::BorderColorFocused(hex));
            }
        }

        #[unsafe(method(onBorderColorUnfocusedChanged:))]
        fn on_border_color_unfocused(&self, sender: Option<&NSColorWell>) {
            if let Some(well) = sender {
                let hex = color_well_to_hex(well);
                self.emit(SettingsAction::BorderColorUnfocused(hex));
            }
        }

        #[unsafe(method(onFfmToggled:))]
        fn on_ffm_toggled(&self, sender: Option<&NSButton>) {
            if let Some(btn) = sender {
                self.emit(SettingsAction::FocusFollowsMouse(btn.state() == 1));
            }
        }

        #[unsafe(method(onMffToggled:))]
        fn on_mff_toggled(&self, sender: Option<&NSButton>) {
            if let Some(btn) = sender {
                self.emit(SettingsAction::MouseFollowsFocus(btn.state() == 1));
            }
        }

        #[unsafe(method(onModKeyChanged:))]
        fn on_mod_key(&self, sender: Option<&NSPopUpButton>) {
            if let Some(popup) = sender {
                let idx = popup.indexOfSelectedItem();
                let key = match idx {
                    0 => "command",
                    1 => "option",
                    2 => "control",
                    _ => "command",
                };
                self.emit(SettingsAction::ModKey(key.to_string()));
            }
        }
    }
);

// --- Constants ---

const WIN_W: f64 = 480.0;
const WIN_H: f64 = 520.0;
const TAB_BAR_H: f64 = 36.0;
const CONTENT_H: f64 = WIN_H - TAB_BAR_H;

// --- SettingsWindow ---

pub struct SettingsWindow {
    window: Retained<NSWindow>,
    _handler: Retained<SettingsHandler>,
    action_rx: mpsc::Receiver<SettingsAction>,
    // General tab controls
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
    // Keybindings/Rules text views for repopulating
    keybinds_text: Retained<NSTextView>,
    rules_text: Retained<NSTextView>,
}

impl SettingsWindow {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let (tx, rx) = mpsc::channel();
        let handler = SettingsHandler::new(mtm, tx);

        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;

        let frame = CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(WIN_W, WIN_H));
        let window: Retained<NSWindow> = unsafe {
            msg_send![
                NSWindow::alloc(mtm),
                initWithContentRect: frame,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        let title = NSString::from_str("tarmac Settings");
        window.setTitle(&title);
        window.center();
        unsafe { window.setReleasedWhenClosed(false) };

        // Root content view
        let root: Retained<NSView> = unsafe {
            msg_send![NSView::alloc(mtm), initWithFrame: frame]
        };
        window.setContentView(Some(&root));

        // Segmented control (tab bar) at top
        let seg_frame = CGRect::new(
            CGPoint::new(20.0, WIN_H - TAB_BAR_H + 4.0),
            CGSize::new(WIN_W - 40.0, 24.0),
        );
        let seg: Retained<NSSegmentedControl> = unsafe {
            msg_send![NSSegmentedControl::alloc(mtm), initWithFrame: seg_frame]
        };
        seg.setSegmentCount(4);
        set_segment_label(&seg, 0, "General");
        set_segment_label(&seg, 1, "Keybindings");
        set_segment_label(&seg, 2, "Rules");
        set_segment_label(&seg, 3, "About");
        for i in 0..4 {
            seg.setWidth_forSegment(100.0, i);
        }
        seg.setSelectedSegment(0);
        unsafe {
            seg.setTarget(Some(&*handler));
            seg.setAction(Some(sel!(onTabChanged:)));
        }
        root.addSubview(&seg);

        // Content container (below tab bar)
        let container_frame = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(WIN_W, CONTENT_H),
        );
        let container: Retained<NSView> = unsafe {
            msg_send![NSView::alloc(mtm), initWithFrame: container_frame]
        };
        root.addSubview(&container);

        // Build tab views
        let general = build_general_view(mtm, &handler);
        let (keybinds_view, keybinds_text) = build_text_tab(mtm, "No keybindings loaded.");
        let (rules_view, rules_text) = build_text_tab(mtm, "No rules loaded.");
        let about_view = build_about_view(mtm);

        // Register tab views with handler for switching
        handler.set_tab_views(
            vec![
                general.view.clone(),
                keybinds_view,
                rules_view,
                about_view,
            ],
            container.clone(),
        );

        // Show General tab initially
        container.addSubview(&general.view);

        Self {
            window,
            _handler: handler,
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
            keybinds_text,
            rules_text,
        }
    }

    pub fn toggle(&self) {
        if self.window.isVisible() {
            self.window.orderOut(None);
        } else {
            self.window.makeKeyAndOrderFront(None);
        }
    }

    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn populate(&self, snap: &SettingsSnapshot) {
        // General tab
        self.gap_inner_slider.setDoubleValue(snap.gap_inner);
        self.gap_outer_slider.setDoubleValue(snap.gap_outer);
        self.bar_height_slider.setDoubleValue(snap.bar_height);
        self.border_width_slider.setDoubleValue(snap.border_width);
        self.border_radius_slider.setDoubleValue(snap.border_radius);

        set_value_label(&self.gap_inner_label, snap.gap_inner);
        set_value_label(&self.gap_outer_label, snap.gap_outer);
        set_value_label(&self.bar_height_label, snap.bar_height);
        set_value_label(&self.border_width_label, snap.border_width);
        set_value_label(&self.border_radius_label, snap.border_radius);

        set_color_well(&self.focused_color_well, &snap.border_color_focused);
        set_color_well(&self.unfocused_color_well, &snap.border_color_unfocused);

        self.ffm_checkbox
            .setState(if snap.focus_follows_mouse { 1 } else { 0 });
        self.mff_checkbox
            .setState(if snap.mouse_follows_focus { 1 } else { 0 });

        let mod_idx: isize = match snap.mod_key.as_str() {
            "command" | "cmd" => 0,
            "option" | "alt" => 1,
            "control" | "ctrl" => 2,
            _ => 0,
        };
        self.mod_key_popup.selectItemAtIndex(mod_idx);

        // Keybindings tab
        let mut kb_text = String::new();
        for (keys, action) in &snap.keybinds {
            kb_text.push_str(&format!("{:<28} {}\n", keys, action));
        }
        if kb_text.is_empty() {
            kb_text.push_str("No keybindings configured.");
        }
        set_text_view(&self.keybinds_text, &kb_text);

        // Rules tab
        let mut rules_text = String::new();
        for (match_str, action_str) in &snap.rules {
            rules_text.push_str(&format!("{:<28} {}\n", match_str, action_str));
        }
        if rules_text.is_empty() {
            rules_text.push_str("No window rules configured.");
        }
        set_text_view(&self.rules_text, &rules_text);
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
        set_value_label(
            &self.bar_height_label,
            self.bar_height_slider.doubleValue(),
        );
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

// --- General tab builder ---

struct GeneralTab {
    view: Retained<NSView>,
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

fn build_general_view(mtm: MainThreadMarker, handler: &SettingsHandler) -> GeneralTab {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, CONTENT_H));
    let view: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    let lx = 20.0_f64;
    let cx = 180.0_f64;
    let cw = 200.0_f64;
    let row = 32.0_f64;
    let mut y = CONTENT_H - 30.0;

    // Layout
    add_section_label(mtm, &view, "Layout", lx, y);
    y -= row;
    let gap_inner_label = add_value_label(mtm, &view, "0", cx + cw + 8.0, y);
    let gap_inner_slider = add_slider(
        mtm, &view, handler, "Inner Gap", lx, cx, y, cw, 0.0, 50.0, 0.0,
        sel!(onGapInnerChanged:),
    );
    y -= row;
    let gap_outer_label = add_value_label(mtm, &view, "0", cx + cw + 8.0, y);
    let gap_outer_slider = add_slider(
        mtm, &view, handler, "Outer Gap", lx, cx, y, cw, 0.0, 50.0, 0.0,
        sel!(onGapOuterChanged:),
    );
    y -= row;
    let bar_height_label = add_value_label(mtm, &view, "0", cx + cw + 8.0, y);
    let bar_height_slider = add_slider(
        mtm, &view, handler, "Bar Height", lx, cx, y, cw, 0.0, 60.0, 0.0,
        sel!(onBarHeightChanged:),
    );
    y -= row + 12.0;

    // Borders
    add_section_label(mtm, &view, "Borders", lx, y);
    y -= row;
    let border_width_label = add_value_label(mtm, &view, "0", cx + cw + 8.0, y);
    let border_width_slider = add_slider(
        mtm, &view, handler, "Width", lx, cx, y, cw, 0.0, 10.0, 0.0,
        sel!(onBorderWidthChanged:),
    );
    y -= row;
    let border_radius_label = add_value_label(mtm, &view, "0", cx + cw + 8.0, y);
    let border_radius_slider = add_slider(
        mtm, &view, handler, "Radius", lx, cx, y, cw, 0.0, 30.0, 10.0,
        sel!(onBorderRadiusChanged:),
    );
    y -= row;
    add_label(mtm, &view, "Focused Color", lx, y);
    let focused_color_well =
        add_color_well(mtm, &view, handler, cx, y, sel!(onBorderColorFocusedChanged:));
    y -= row;
    add_label(mtm, &view, "Unfocused Color", lx, y);
    let unfocused_color_well =
        add_color_well(mtm, &view, handler, cx, y, sel!(onBorderColorUnfocusedChanged:));
    y -= row + 12.0;

    // Behavior
    add_section_label(mtm, &view, "Behavior", lx, y);
    y -= row;
    let ffm_checkbox = add_checkbox(
        mtm, &view, handler, "Focus follows mouse", lx, y, sel!(onFfmToggled:),
    );
    y -= row;
    let mff_checkbox = add_checkbox(
        mtm, &view, handler, "Mouse follows focus", lx, y, sel!(onMffToggled:),
    );
    y -= row + 12.0;

    // Modifier Key
    add_section_label(mtm, &view, "Modifier Key", lx, y);
    y -= row;
    let mod_key_popup = add_popup(
        mtm, &view, handler, lx, y, cw,
        &["Command", "Option", "Control"],
        sel!(onModKeyChanged:),
    );
    let _ = y;

    GeneralTab {
        view,
        gap_inner_slider, gap_outer_slider, bar_height_slider,
        border_width_slider, border_radius_slider,
        focused_color_well, unfocused_color_well,
        ffm_checkbox, mff_checkbox, mod_key_popup,
        gap_inner_label, gap_outer_label, bar_height_label,
        border_width_label, border_radius_label,
    }
}

// --- Text-based tabs (Keybindings, Rules) ---

fn build_text_tab(
    mtm: MainThreadMarker,
    placeholder: &str,
) -> (Retained<NSView>, Retained<NSTextView>) {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, CONTENT_H));
    let view: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    let scroll_frame = CGRect::new(
        CGPoint::new(15.0, 15.0),
        CGSize::new(WIN_W - 30.0, CONTENT_H - 30.0),
    );
    let scroll: Retained<NSScrollView> = unsafe {
        msg_send![NSScrollView::alloc(mtm), initWithFrame: scroll_frame]
    };
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(objc2_app_kit::NSBorderType(2)); // NSBezelBorder

    let text_frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W - 50.0, CONTENT_H));
    let text: Retained<NSTextView> = unsafe {
        msg_send![NSTextView::alloc(mtm), initWithFrame: text_frame]
    };
    text.setEditable(false);
    unsafe {
        let mono: Retained<objc2_app_kit::NSFont> =
            msg_send![objc2_app_kit::NSFont::class(), monospacedSystemFontOfSize: 11.0_f64, weight: 0.0_f64];
        text.setFont(Some(&mono));
    }
    set_text_view(&text, placeholder);

    scroll.setDocumentView(Some(&text));
    view.addSubview(&scroll);

    (view, text)
}

fn set_text_view(tv: &NSTextView, content: &str) {
    let ns = NSString::from_str(content);
    tv.setString(&ns);
}

// --- About tab ---

fn build_about_view(mtm: MainThreadMarker) -> Retained<NSView> {
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(WIN_W, CONTENT_H));
    let view: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), initWithFrame: frame] };

    let center_x = WIN_W / 2.0 - 150.0;
    let mut y = CONTENT_H - 60.0;

    // App name
    let name_frame = CGRect::new(CGPoint::new(center_x, y), CGSize::new(300.0, 28.0));
    let name: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: name_frame]
    };
    name.setStringValue(&NSString::from_str("tarmac"));
    name.setEditable(false);
    name.setBordered(false);
    name.setDrawsBackground(false);
    unsafe {
        let _: () = msg_send![&*name, setAlignment: 1_isize]; // NSTextAlignmentCenter
        let font: Retained<objc2_app_kit::NSFont> =
            msg_send![objc2_app_kit::NSFont::class(), boldSystemFontOfSize: 24.0_f64];
        name.setFont(Some(&font));
    }
    view.addSubview(&name);
    y -= 28.0;

    // Subtitle
    add_centered_label(mtm, &view, "a macOS tiling window manager", center_x, y);
    y -= 36.0;

    // Version
    let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
    add_centered_label(mtm, &view, &version, center_x, y);
    y -= 28.0;

    // Build info
    add_centered_label(mtm, &view, "Rust 2024 edition · objc2 + SkyLight", center_x, y);
    y -= 36.0;

    // Links
    add_centered_label(mtm, &view, "github.com/gardesk/tarmac", center_x, y);
    y -= 28.0;
    add_centered_label(mtm, &view, "config: ~/.config/tarmac/init.lua", center_x, y);
    let _ = y;

    view
}

fn add_centered_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(300.0, 20.0));
    let label: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: frame]
    };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    unsafe {
        let _: () = msg_send![&*label, setAlignment: 1_isize]; // NSTextAlignmentCenter
    }
    parent.addSubview(&label);
}

// --- Segment helper ---

fn set_segment_label(seg: &NSSegmentedControl, index: isize, label: &str) {
    let ns = NSString::from_str(label);
    seg.setLabel_forSegment(&ns, index);
}

// --- Shared control helpers ---

fn add_section_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(360.0, 20.0));
    let label: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: frame]
    };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    unsafe {
        let bold: Retained<objc2_app_kit::NSFont> =
            msg_send![objc2_app_kit::NSFont::class(), boldSystemFontOfSize: 13.0_f64];
        label.setFont(Some(&bold));
    }
    parent.addSubview(&label);
}

fn add_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
    let frame = CGRect::new(CGPoint::new(x, y + 2.0), CGSize::new(150.0, 18.0));
    let label: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: frame]
    };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    parent.addSubview(&label);
}

fn add_value_label(
    mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64,
) -> Retained<NSTextField> {
    let frame = CGRect::new(CGPoint::new(x, y + 2.0), CGSize::new(30.0, 18.0));
    let label: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: frame]
    };
    label.setStringValue(&NSString::from_str(text));
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    parent.addSubview(&label);
    label
}

fn set_value_label(label: &NSTextField, val: f64) {
    let ns = NSString::from_str(&format!("{}", val as i32));
    label.setStringValue(&ns);
}

#[allow(clippy::too_many_arguments)]
fn add_slider(
    mtm: MainThreadMarker, parent: &NSView, handler: &SettingsHandler,
    label_text: &str, label_x: f64, control_x: f64, y: f64, width: f64,
    min: f64, max: f64, initial: f64, action: objc2::runtime::Sel,
) -> Retained<NSSlider> {
    add_label(mtm, parent, label_text, label_x, y);
    let frame = CGRect::new(CGPoint::new(control_x, y), CGSize::new(width, 20.0));
    let slider: Retained<NSSlider> = unsafe {
        msg_send![NSSlider::alloc(mtm), initWithFrame: frame]
    };
    slider.setMinValue(min);
    slider.setMaxValue(max);
    slider.setDoubleValue(initial);
    unsafe {
        slider.setTarget(Some(handler));
        slider.setAction(Some(action));
        slider.setContinuous(true);
    }
    parent.addSubview(&slider);
    slider
}

fn add_color_well(
    mtm: MainThreadMarker, parent: &NSView, handler: &SettingsHandler,
    x: f64, y: f64, action: objc2::runtime::Sel,
) -> Retained<NSColorWell> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(44.0, 24.0));
    let well: Retained<NSColorWell> = unsafe {
        msg_send![NSColorWell::alloc(mtm), initWithFrame: frame]
    };
    unsafe {
        well.setTarget(Some(handler));
        well.setAction(Some(action));
    }
    parent.addSubview(&well);
    well
}

fn add_checkbox(
    mtm: MainThreadMarker, parent: &NSView, handler: &SettingsHandler,
    title: &str, x: f64, y: f64, action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(250.0, 22.0));
    let btn: Retained<NSButton> = unsafe {
        msg_send![NSButton::alloc(mtm), initWithFrame: frame]
    };
    btn.setTitle(&NSString::from_str(title));
    unsafe {
        let _: () = msg_send![&*btn, setButtonType: 3_isize]; // NSSwitchButton
        btn.setTarget(Some(handler));
        btn.setAction(Some(action));
    }
    parent.addSubview(&btn);
    btn
}

fn add_popup(
    mtm: MainThreadMarker, parent: &NSView, handler: &SettingsHandler,
    x: f64, y: f64, width: f64, items: &[&str], action: objc2::runtime::Sel,
) -> Retained<NSPopUpButton> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(width, 26.0));
    let popup: Retained<NSPopUpButton> = unsafe {
        msg_send![NSPopUpButton::alloc(mtm), initWithFrame: frame, pullsDown: false]
    };
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

fn color_well_to_hex(well: &NSColorWell) -> String {
    unsafe {
        let color = well.color();
        let rgb: Option<Retained<objc2_app_kit::NSColor>> = msg_send![
            &*color,
            colorUsingColorSpaceName: &*NSString::from_str("NSCalibratedRGBColorSpace")
        ];
        if let Some(rgb) = rgb {
            let r: f64 = msg_send![&*rgb, redComponent];
            let g: f64 = msg_send![&*rgb, greenComponent];
            let b: f64 = msg_send![&*rgb, blueComponent];
            format!("#{:02x}{:02x}{:02x}", (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
        } else {
            "#000000".to_string()
        }
    }
}

fn set_color_well(well: &NSColorWell, hex: &str) {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f64 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f64 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f64 / 255.0;
    unsafe {
        let color: Retained<objc2_app_kit::NSColor> = msg_send![
            objc2_app_kit::NSColor::class(),
            colorWithCalibratedRed: r, green: g, blue: b, alpha: 1.0_f64
        ];
        well.setColor(&color);
    }
}
