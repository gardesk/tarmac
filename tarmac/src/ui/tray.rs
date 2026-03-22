use std::sync::mpsc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    define_class, msg_send, sel, AnyThread, ClassType, DefinedClass, MainThreadMarker,
    MainThreadOnly,
};
use objc2_app_kit::{
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObject, NSSize, NSString};

/// Embedded 32x32 PNG template image (airplane landing on strip).
const TRAY_ICON_PNG: &[u8] = include_bytes!("../../assets/tray_icon_32.png");

/// Actions dispatched from tray menu items.
#[derive(Debug)]
pub enum TrayAction {
    SwitchWorkspace(u8),
    OpenSettings,
    Reload,
    Quit,
}

/// Workspace info for building the tray menu.
pub struct TrayWorkspace {
    pub id: String,
    pub active: bool,
    pub windows: usize,
}

// --- MenuActionHandler: ObjC target for menu item selectors ---

struct TrayHandlerIvars {
    tx: mpsc::Sender<TrayAction>,
}

impl TrayHandler {
    fn new(mtm: MainThreadMarker, tx: mpsc::Sender<TrayAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(TrayHandlerIvars { tx });
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, action: TrayAction) {
        let _ = self.ivars().tx.send(action);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TarmacTrayHandler"]
    #[ivars = TrayHandlerIvars]
    struct TrayHandler;

    impl TrayHandler {
        #[unsafe(method(onSwitchWorkspace:))]
        fn on_switch_workspace(&self, sender: Option<&NSMenuItem>) {
            if let Some(sender) = sender {
                let tag = sender.tag();
                if tag > 0 {
                    self.emit(TrayAction::SwitchWorkspace(tag as u8));
                }
            }
        }

        #[unsafe(method(onOpenSettings:))]
        fn on_open_settings(&self, _sender: Option<&AnyObject>) {
            self.emit(TrayAction::OpenSettings);
        }

        #[unsafe(method(onReload:))]
        fn on_reload(&self, _sender: Option<&AnyObject>) {
            self.emit(TrayAction::Reload);
        }

        #[unsafe(method(onQuit:))]
        fn on_quit(&self, _sender: Option<&AnyObject>) {
            self.emit(TrayAction::Quit);
        }
    }
);

// --- TrayWidget ---

pub struct TrayWidget {
    _status_item: Retained<NSStatusItem>,
    handler: Retained<TrayHandler>,
    mtm: MainThreadMarker,
    action_rx: mpsc::Receiver<TrayAction>,
}

impl TrayWidget {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        // Set template image
        if let Some(btn) = status_item.button(mtm) {
            let image = load_template_image();
            btn.setImage(Some(&image));
        }

        let (tx, rx) = mpsc::channel();
        let handler = TrayHandler::new(mtm, tx);

        // Build initial empty menu
        let menu = build_menu(mtm, &handler, &[], "–");
        status_item.setMenu(Some(&menu));
        status_item.setVisible(true);

        Self {
            _status_item: status_item,
            handler,
            mtm,
            action_rx: rx,
        }
    }

    /// Rebuild the tray menu with current workspace state.
    pub fn update(&self, workspaces: &[TrayWorkspace], active_id: &str) {
        let menu = build_menu(self.mtm, &self.handler, workspaces, active_id);
        self._status_item.setMenu(Some(&menu));

        // Update button title to show active workspace
        if let Some(btn) = self._status_item.button(self.mtm) {
            let title = NSString::from_str(active_id);
            btn.setTitle(&title);
        }
    }

    /// Drain any pending menu actions. Call from the main thread polling timer.
    pub fn poll_actions(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.action_rx.try_recv() {
            actions.push(action);
        }
        actions
    }
}

impl Drop for TrayWidget {
    fn drop(&mut self) {
        let status_bar = NSStatusBar::systemStatusBar();
        status_bar.removeStatusItem(&self._status_item);
    }
}

// --- Helpers ---

fn load_template_image() -> Retained<NSImage> {
    unsafe {
        let data = NSData::with_bytes(TRAY_ICON_PNG);
        let image: Retained<NSImage> = msg_send![NSImage::alloc(), initWithData: &*data];
        image.setTemplate(true);
        image.setSize(NSSize::new(16.0, 16.0)); // point size (retina handled by 32px source)
        image
    }
}

fn build_menu(
    mtm: MainThreadMarker,
    handler: &TrayHandler,
    workspaces: &[TrayWorkspace],
    active_id: &str,
) -> Retained<NSMenu> {
    let title = NSString::from_str("tarmac");
    let menu: Retained<NSMenu> = unsafe { msg_send![NSMenu::alloc(mtm), initWithTitle: &*title] };

    // Workspace items
    for ws in workspaces {
        if ws.windows == 0 && !ws.active {
            continue; // skip empty non-active workspaces
        }
        let label = if ws.windows > 0 {
            format!("{}  ({} window{})", ws.id, ws.windows, if ws.windows == 1 { "" } else { "s" })
        } else {
            ws.id.clone()
        };
        let item = make_item(mtm, &label, Some(sel!(onSwitchWorkspace:)), Some(handler));

        // Parse workspace ID as tag for the action handler
        if let Ok(n) = ws.id.parse::<isize>() {
            item.setTag(n);
        }

        // Checkmark on active workspace
        if ws.id == active_id {
            item.setState(1); // NSControlStateValueOn
        }

        menu.addItem(&item);
    }

    // Separator
    let sep: Retained<NSMenuItem> = unsafe { msg_send![NSMenuItem::class(), separatorItem] };
    menu.addItem(&sep);

    // Settings
    let settings = make_item(mtm, "Settings\u{2026}", Some(sel!(onOpenSettings:)), Some(handler));
    menu.addItem(&settings);

    // Reload Config
    let reload = make_item(mtm, "Reload Config", Some(sel!(onReload:)), Some(handler));
    menu.addItem(&reload);

    // Quit
    let quit = make_item(mtm, "Quit tarmac", Some(sel!(onQuit:)), Some(handler));
    menu.addItem(&quit);

    menu
}

fn make_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    target: Option<&TrayHandler>,
) -> Retained<NSMenuItem> {
    let ns_title = NSString::from_str(title);
    let key_eq = NSString::from_str("");
    let item: Retained<NSMenuItem> = unsafe {
        msg_send![NSMenuItem::alloc(mtm), initWithTitle: &*ns_title, action: action, keyEquivalent: &*key_eq]
    };
    if let Some(target) = target {
        unsafe { item.setTarget(Some(target)) };
    }
    item
}
