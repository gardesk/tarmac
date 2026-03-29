use std::cell::RefCell;
use std::sync::mpsc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSString};

use crate::core::workspace::WorkspaceTarget;

#[derive(Debug)]
pub enum TrayAction {
    SwitchWorkspace(WorkspaceTarget),
    OpenSettings,
    Reload,
    Quit,
}

#[derive(Debug, Clone)]
pub struct TrayWorkspace {
    pub id: String,
    pub active: bool,
    pub windows: usize,
    pub switch_target: Option<WorkspaceTarget>,
}

struct TrayHandlerIvars {
    tx: mpsc::Sender<TrayAction>,
    targets: RefCell<Vec<WorkspaceTarget>>,
}

impl TrayHandler {
    fn new(mtm: MainThreadMarker, tx: mpsc::Sender<TrayAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(TrayHandlerIvars {
            tx,
            targets: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, action: TrayAction) {
        let _ = self.ivars().tx.send(action);
    }

    fn set_targets(&self, targets: Vec<WorkspaceTarget>) {
        *self.ivars().targets.borrow_mut() = targets;
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
            let Some(sender) = sender else { return };
            let tag = sender.tag();
            if tag < 0 {
                return;
            }
            let Some(target) = self.ivars().targets.borrow().get(tag as usize).cloned() else {
                return;
            };
            self.emit(TrayAction::SwitchWorkspace(target));
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

pub struct TrayWidget {
    status_item: Retained<NSStatusItem>,
    handler: Retained<TrayHandler>,
    mtm: MainThreadMarker,
    action_rx: mpsc::Receiver<TrayAction>,
}

impl TrayWidget {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        if let Some(button) = status_item.button(mtm)
            && let Some(image) = tray_image()
        {
            button.setImage(Some(&image));
        }

        let (tx, rx) = mpsc::channel();
        let handler = TrayHandler::new(mtm, tx);
        let menu = build_menu(mtm, &handler, &[], "1");
        status_item.setMenu(Some(&menu));
        status_item.setVisible(true);

        Self {
            status_item,
            handler,
            mtm,
            action_rx: rx,
        }
    }

    pub fn update(&self, workspaces: &[TrayWorkspace], active_id: &str) {
        let targets = workspaces
            .iter()
            .filter_map(|workspace| workspace.switch_target.clone())
            .collect::<Vec<_>>();
        self.handler.set_targets(targets);

        let menu = build_menu(self.mtm, &self.handler, workspaces, active_id);
        self.status_item.setMenu(Some(&menu));
        if let Some(button) = self.status_item.button(self.mtm) {
            button.setTitle(&NSString::from_str(active_id));
        }
    }

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
        status_bar.removeStatusItem(&self.status_item);
    }
}

fn tray_image() -> Option<Retained<NSImage>> {
    // Use SF Symbol for a clean native menu bar icon.
    // "airplane" is the landing plane glyph; fall back to grid if unavailable.
    template_symbol("airplane").or_else(|| template_symbol("square.grid.2x2"))
}

fn template_symbol(symbol: &str) -> Option<Retained<NSImage>> {
    let name = NSString::from_str(symbol);
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, None)?;
    image.setTemplate(true);
    Some(image)
}

fn build_menu(
    mtm: MainThreadMarker,
    handler: &TrayHandler,
    workspaces: &[TrayWorkspace],
    active_id: &str,
) -> Retained<NSMenu> {
    let title = NSString::from_str("tarmac");
    let menu: Retained<NSMenu> = unsafe { msg_send![NSMenu::alloc(mtm), initWithTitle: &*title] };

    let mut target_index: isize = 0;
    for workspace in workspaces {
        if workspace.windows == 0 && !workspace.active {
            continue;
        }
        let label = if workspace.windows > 0 {
            format!(
                "{}  ({} window{})",
                workspace.id,
                workspace.windows,
                if workspace.windows == 1 { "" } else { "s" }
            )
        } else {
            workspace.id.clone()
        };
        let item = if workspace.switch_target.is_some() {
            let item = make_item(mtm, &label, Some(sel!(onSwitchWorkspace:)), Some(handler));
            item.setTag(target_index);
            target_index += 1;
            item
        } else {
            let item = make_item(mtm, &label, None, None);
            item.setEnabled(false);
            item
        };

        if workspace.id == active_id {
            item.setState(1);
        }
        menu.addItem(&item);
    }

    let sep: Retained<NSMenuItem> = unsafe { msg_send![NSMenuItem::class(), separatorItem] };
    menu.addItem(&sep);

    let settings = make_item(mtm, "Settings…", Some(sel!(onOpenSettings:)), Some(handler));
    menu.addItem(&settings);

    let reload = make_item(mtm, "Reload Config", Some(sel!(onReload:)), Some(handler));
    menu.addItem(&reload);

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
