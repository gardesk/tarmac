use std::sync::mpsc;

use objc2::rc::Retained;
use objc2::{
    define_class, msg_send, sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColorWell, NSPopUpButton, NSSlider, NSTextField, NSView,
    NSWindow, NSWindowStyleMask,
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
}

// --- ObjC action handler ---

struct SettingsHandlerIvars {
    tx: mpsc::Sender<SettingsAction>,
}

impl SettingsHandler {
    fn new(mtm: MainThreadMarker, tx: mpsc::Sender<SettingsAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(SettingsHandlerIvars { tx });
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, action: SettingsAction) {
        let _ = self.ivars().tx.send(action);
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

// --- SettingsWindow ---

pub struct SettingsWindow {
    window: Retained<NSWindow>,
    _handler: Retained<SettingsHandler>,
    action_rx: mpsc::Receiver<SettingsAction>,
    // Controls we need to update when populating
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
    // Value labels for sliders
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
            | NSWindowStyleMask::Miniaturizable;

        let frame = CGRect::new(CGPoint::new(200.0, 200.0), CGSize::new(420.0, 520.0));
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

        // Flipped content view for top-down layout
        let content: Retained<NSView> = unsafe {
            msg_send![NSView::alloc(mtm), initWithFrame: frame]
        };
        window.setContentView(Some(&content));

        // Build controls top-down (macOS default coords: 0,0 = bottom-left)
        // So we position from the top by computing y = height - offset
        let h = 520.0_f64;
        let lx = 20.0_f64; // label x
        let cx = 180.0_f64; // control x
        let cw = 200.0_f64; // control width
        let row = 32.0_f64; // row height

        let mut y = h - 40.0; // start from top

        // --- Section: Layout ---
        add_section_label(mtm, &content, "Layout", lx, y);
        y -= row;

        let gap_inner_label = add_value_label(mtm, &content, "0", cx + cw + 8.0, y);
        let gap_inner_slider = add_slider(
            mtm, &content, &handler, "Inner Gap", lx, cx, y, cw,
            0.0, 50.0, 0.0, sel!(onGapInnerChanged:),
        );
        y -= row;

        let gap_outer_label = add_value_label(mtm, &content, "0", cx + cw + 8.0, y);
        let gap_outer_slider = add_slider(
            mtm, &content, &handler, "Outer Gap", lx, cx, y, cw,
            0.0, 50.0, 0.0, sel!(onGapOuterChanged:),
        );
        y -= row;

        let bar_height_label = add_value_label(mtm, &content, "0", cx + cw + 8.0, y);
        let bar_height_slider = add_slider(
            mtm, &content, &handler, "Bar Height", lx, cx, y, cw,
            0.0, 60.0, 0.0, sel!(onBarHeightChanged:),
        );
        y -= row + 12.0;

        // --- Section: Borders ---
        add_section_label(mtm, &content, "Borders", lx, y);
        y -= row;

        let border_width_label = add_value_label(mtm, &content, "0", cx + cw + 8.0, y);
        let border_width_slider = add_slider(
            mtm, &content, &handler, "Width", lx, cx, y, cw,
            0.0, 10.0, 0.0, sel!(onBorderWidthChanged:),
        );
        y -= row;

        let border_radius_label = add_value_label(mtm, &content, "0", cx + cw + 8.0, y);
        let border_radius_slider = add_slider(
            mtm, &content, &handler, "Radius", lx, cx, y, cw,
            0.0, 30.0, 10.0, sel!(onBorderRadiusChanged:),
        );
        y -= row;

        add_label(mtm, &content, "Focused Color", lx, y);
        let focused_color_well = add_color_well(
            mtm, &content, &handler, cx, y, sel!(onBorderColorFocusedChanged:),
        );
        y -= row;

        add_label(mtm, &content, "Unfocused Color", lx, y);
        let unfocused_color_well = add_color_well(
            mtm, &content, &handler, cx, y, sel!(onBorderColorUnfocusedChanged:),
        );
        y -= row + 12.0;

        // --- Section: Behavior ---
        add_section_label(mtm, &content, "Behavior", lx, y);
        y -= row;

        let ffm_checkbox = add_checkbox(
            mtm, &content, &handler, "Focus follows mouse", lx, y,
            sel!(onFfmToggled:),
        );
        y -= row;

        let mff_checkbox = add_checkbox(
            mtm, &content, &handler, "Mouse follows focus", lx, y,
            sel!(onMffToggled:),
        );
        y -= row + 12.0;

        // --- Section: Modifier Key ---
        add_section_label(mtm, &content, "Modifier Key", lx, y);
        y -= row;

        let mod_key_popup = add_popup(
            mtm, &content, &handler, lx, y, cw,
            &["Command", "Option", "Control"],
            sel!(onModKeyChanged:),
        );
        let _ = y; // suppress unused

        Self {
            window,
            _handler: handler,
            action_rx: rx,
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

    /// Show the settings window (or bring to front).
    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    /// Populate controls from current settings.
    pub fn populate(&self, snap: &SettingsSnapshot) {
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

        self.ffm_checkbox.setState(if snap.focus_follows_mouse { 1 } else { 0 });
        self.mff_checkbox.setState(if snap.mouse_follows_focus { 1 } else { 0 });

        let mod_idx: isize = match snap.mod_key.as_str() {
            "command" | "cmd" => 0,
            "option" | "alt" => 1,
            "control" | "ctrl" => 2,
            _ => 0,
        };
        self.mod_key_popup.selectItemAtIndex(mod_idx);
    }

    /// Drain pending actions.
    pub fn poll_actions(&self) -> Vec<SettingsAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.action_rx.try_recv() {
            actions.push(action);
        }
        actions
    }

    /// Update value labels when sliders change (called from main loop after processing actions).
    pub fn refresh_labels(&self) {
        set_value_label(&self.gap_inner_label, self.gap_inner_slider.doubleValue());
        set_value_label(&self.gap_outer_label, self.gap_outer_slider.doubleValue());
        set_value_label(&self.bar_height_label, self.bar_height_slider.doubleValue());
        set_value_label(&self.border_width_label, self.border_width_slider.doubleValue());
        set_value_label(&self.border_radius_label, self.border_radius_slider.doubleValue());
    }
}

// --- Helper functions ---

fn add_section_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(360.0, 20.0));
    let label: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: frame]
    };
    let ns = NSString::from_str(text);
    label.setStringValue(&ns);
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    unsafe {
        let bold_font: Retained<objc2_app_kit::NSFont> =
            msg_send![objc2_app_kit::NSFont::class(), boldSystemFontOfSize: 13.0_f64];
        label.setFont(Some(&bold_font));
    }
    parent.addSubview(&label);
}

fn add_label(mtm: MainThreadMarker, parent: &NSView, text: &str, x: f64, y: f64) {
    let frame = CGRect::new(CGPoint::new(x, y + 2.0), CGSize::new(150.0, 18.0));
    let label: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: frame]
    };
    let ns = NSString::from_str(text);
    label.setStringValue(&ns);
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    parent.addSubview(&label);
}

fn add_value_label(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
) -> Retained<NSTextField> {
    let frame = CGRect::new(CGPoint::new(x, y + 2.0), CGSize::new(30.0, 18.0));
    let label: Retained<NSTextField> = unsafe {
        msg_send![NSTextField::alloc(mtm), initWithFrame: frame]
    };
    let ns = NSString::from_str(text);
    label.setStringValue(&ns);
    label.setEditable(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    parent.addSubview(&label);
    label
}

fn set_value_label(label: &NSTextField, val: f64) {
    let text = format!("{}", val as i32);
    let ns = NSString::from_str(&text);
    label.setStringValue(&ns);
}

#[allow(clippy::too_many_arguments)]
fn add_slider(
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    label_text: &str,
    label_x: f64,
    control_x: f64,
    y: f64,
    width: f64,
    min: f64,
    max: f64,
    initial: f64,
    action: objc2::runtime::Sel,
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
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    x: f64,
    y: f64,
    action: objc2::runtime::Sel,
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
    mtm: MainThreadMarker,
    parent: &NSView,
    handler: &SettingsHandler,
    title: &str,
    x: f64,
    y: f64,
    action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    let frame = CGRect::new(CGPoint::new(x, y), CGSize::new(250.0, 22.0));
    let btn: Retained<NSButton> = unsafe {
        msg_send![NSButton::alloc(mtm), initWithFrame: frame]
    };
    let ns_title = NSString::from_str(title);
    unsafe {
        let _: () = msg_send![&*btn, setButtonType: 3_isize]; // NSSwitchButton
    }
    btn.setTitle(&ns_title);
    unsafe {
        btn.setTarget(Some(handler));
        btn.setAction(Some(action));
    }
    parent.addSubview(&btn);
    btn
}

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
    let popup: Retained<NSPopUpButton> = unsafe {
        msg_send![NSPopUpButton::alloc(mtm), initWithFrame: frame, pullsDown: false]
    };
    for item in items {
        let ns = NSString::from_str(item);
        popup.addItemWithTitle(&ns);
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
        // Convert to sRGB color space
        let rgb: Option<Retained<objc2_app_kit::NSColor>> = msg_send![
            &*color,
            colorUsingColorSpaceName: &*NSString::from_str("NSCalibratedRGBColorSpace")
        ];
        if let Some(rgb) = rgb {
            let r: f64 = msg_send![&*rgb, redComponent];
            let g: f64 = msg_send![&*rgb, greenComponent];
            let b: f64 = msg_send![&*rgb, blueComponent];
            format!(
                "#{:02x}{:02x}{:02x}",
                (r * 255.0) as u8,
                (g * 255.0) as u8,
                (b * 255.0) as u8,
            )
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
            colorWithCalibratedRed: r,
            green: g,
            blue: b,
            alpha: 1.0_f64
        ];
        well.setColor(&color);
    }
}
