//! System-level global hotkeys via Carbon RegisterEventHotKey.
//! This works even when apps like Firefox consume Cmd+number keys,
//! because the OS intercepts the hotkey before dispatching to apps.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::core::input::{Action, Key, Modifiers};
use crate::core::workspace::WorkspaceTarget;

// Carbon types
type OSStatus = i32;
type EventHandlerRef = *mut c_void;
type EventRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventHotKeyRef = *mut c_void;

const NO_ERR: OSStatus = 0;

// Carbon event constants
const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");

// Carbon modifier constants
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[allow(non_snake_case)]
struct EventTypeSpec {
    eventClass: u32,
    eventKind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    #[allow(dead_code)]
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn GetEventDispatcherTarget() -> EventTargetRef;

    fn InstallEventHandler(
        target: EventTargetRef,
        handler: Option<
            unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus,
        >,
        num_types: u32,
        type_list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;

    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;

    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;
    fn RemoveEventHandler(handler_ref: EventHandlerRef) -> OSStatus;

    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: u32,
        actual_size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
}

/// Callback type for hotkey events.
type HotkeyCallback = Box<dyn Fn(Action)>;

#[allow(dead_code)]
struct RegisteredHotKey {
    carbon_ref: EventHotKeyRef,
    action: Action,
}

/// Global hotkey manager using Carbon RegisterEventHotKey.
pub struct HotkeyManager {
    _handler_ref: EventHandlerRef,
    hotkeys: HashMap<u32, RegisteredHotKey>,
    _callback_ctx: *mut HotkeyCallbackCtx,
}

struct HotkeyCallbackCtx {
    callback: HotkeyCallback,
    // Map from hotkey ID → action (shared with the callback)
    id_to_action: HashMap<u32, Action>,
}

static NEXT_HOTKEY_ID: AtomicU32 = AtomicU32::new(1);
const HOTKEY_SIGNATURE: u32 = u32::from_be_bytes(*b"tarm");

impl HotkeyManager {
    /// Create a hotkey manager with the given callback.
    /// Register individual hotkeys with `register()`.
    pub fn new(callback: HotkeyCallback) -> Option<Self> {
        let ctx = Box::new(HotkeyCallbackCtx {
            callback,
            id_to_action: HashMap::new(),
        });
        let ctx_ptr = Box::into_raw(ctx);

        let event_types = [EventTypeSpec {
            eventClass: K_EVENT_CLASS_KEYBOARD,
            eventKind: K_EVENT_HOT_KEY_PRESSED,
        }];

        let mut handler_ref: EventHandlerRef = ptr::null_mut();
        let status = unsafe {
            InstallEventHandler(
                GetEventDispatcherTarget(),
                Some(hotkey_event_handler),
                1,
                event_types.as_ptr(),
                ctx_ptr as *mut c_void,
                &mut handler_ref,
            )
        };

        if status != NO_ERR || handler_ref.is_null() {
            unsafe { drop(Box::from_raw(ctx_ptr)) };
            tracing::error!(status, "failed to install Carbon event handler");
            return None;
        }

        tracing::info!("Carbon hotkey handler installed");

        Some(Self {
            _handler_ref: handler_ref,
            hotkeys: HashMap::new(),
            _callback_ctx: ctx_ptr,
        })
    }

    /// Register a global hotkey.
    pub fn register(&mut self, modifiers: Modifiers, key: Key, action: Action) -> bool {
        let carbon_mods = modifiers_to_carbon(modifiers);
        let carbon_key = key_to_carbon(key);

        let Some(carbon_key) = carbon_key else {
            tracing::warn!(?key, "no Carbon keycode for key");
            return false;
        };

        let id = NEXT_HOTKEY_ID.fetch_add(1, Ordering::Relaxed);
        let hot_key_id = EventHotKeyID {
            signature: HOTKEY_SIGNATURE,
            id,
        };

        let mut carbon_ref: EventHotKeyRef = ptr::null_mut();
        let status = unsafe {
            RegisterEventHotKey(
                carbon_key,
                carbon_mods,
                hot_key_id,
                GetEventDispatcherTarget(),
                0,
                &mut carbon_ref,
            )
        };

        if status != NO_ERR {
            tracing::warn!(
                ?key,
                ?modifiers,
                status,
                carbon_key,
                carbon_mods,
                "failed to register hotkey"
            );
            return false;
        }

        // Register the action mapping in the callback context
        unsafe {
            (*self._callback_ctx)
                .id_to_action
                .insert(id, action.clone());
        }

        tracing::debug!(?key, ?modifiers, ?action, id, "hotkey registered");
        self.hotkeys
            .insert(id, RegisteredHotKey { carbon_ref, action });
        true
    }

    /// Register all default keybindings.
    pub fn register_defaults(&mut self) {
        use crate::core::tree::Direction::*;
        let m = Modifiers::COMMAND;
        let ms = Modifiers::COMMAND | Modifiers::SHIFT;
        let mc = Modifiers::COMMAND | Modifiers::CONTROL;

        // Spawn / close
        self.register(m, Key::Return, Action::SpawnTerminal);
        self.register(ms, Key::Q, Action::CloseWindow);

        // Focus: Cmd+hjkl and Cmd+Arrows
        self.register(m, Key::H, Action::Focus(Left));
        self.register(m, Key::J, Action::Focus(Down));
        self.register(m, Key::K, Action::Focus(Up));
        self.register(m, Key::L, Action::Focus(Right));
        self.register(m, Key::Left, Action::Focus(Left));
        self.register(m, Key::Down, Action::Focus(Down));
        self.register(m, Key::Up, Action::Focus(Up));
        self.register(m, Key::Right, Action::Focus(Right));

        // Swap: Cmd+Shift+hjkl and Cmd+Shift+Arrows
        self.register(ms, Key::H, Action::Swap(Left));
        self.register(ms, Key::J, Action::Swap(Down));
        self.register(ms, Key::K, Action::Swap(Up));
        self.register(ms, Key::L, Action::Swap(Right));
        self.register(ms, Key::Left, Action::Swap(Left));
        self.register(ms, Key::Down, Action::Swap(Down));
        self.register(ms, Key::Up, Action::Swap(Up));
        self.register(ms, Key::Right, Action::Swap(Right));

        // Resize: Cmd+Ctrl+hjkl
        self.register(mc, Key::H, Action::Resize(Left));
        self.register(mc, Key::J, Action::Resize(Down));
        self.register(mc, Key::K, Action::Resize(Up));
        self.register(mc, Key::L, Action::Resize(Right));

        // Equalize
        self.register(m, Key::E, Action::Equalize);

        // Toggle float
        self.register(ms, Key::Space, Action::ToggleFloat);

        // Workspaces: Cmd+1-9, Cmd+0
        self.register(
            m,
            Key::Num1,
            Action::Workspace(WorkspaceTarget::Numbered(1)),
        );
        self.register(
            m,
            Key::Num2,
            Action::Workspace(WorkspaceTarget::Numbered(2)),
        );
        self.register(
            m,
            Key::Num3,
            Action::Workspace(WorkspaceTarget::Numbered(3)),
        );
        self.register(
            m,
            Key::Num4,
            Action::Workspace(WorkspaceTarget::Numbered(4)),
        );
        self.register(
            m,
            Key::Num5,
            Action::Workspace(WorkspaceTarget::Numbered(5)),
        );
        self.register(
            m,
            Key::Num6,
            Action::Workspace(WorkspaceTarget::Numbered(6)),
        );
        self.register(
            m,
            Key::Num7,
            Action::Workspace(WorkspaceTarget::Numbered(7)),
        );
        self.register(
            m,
            Key::Num8,
            Action::Workspace(WorkspaceTarget::Numbered(8)),
        );
        self.register(
            m,
            Key::Num9,
            Action::Workspace(WorkspaceTarget::Numbered(9)),
        );
        self.register(
            m,
            Key::Num0,
            Action::Workspace(WorkspaceTarget::Numbered(10)),
        );

        // Move to workspace: Cmd+Shift+1-9, Cmd+Shift+0
        self.register(
            ms,
            Key::Num1,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(1)),
        );
        self.register(
            ms,
            Key::Num2,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(2)),
        );
        self.register(
            ms,
            Key::Num3,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(3)),
        );
        self.register(
            ms,
            Key::Num4,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(4)),
        );
        self.register(
            ms,
            Key::Num5,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(5)),
        );
        self.register(
            ms,
            Key::Num6,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(6)),
        );
        self.register(
            ms,
            Key::Num7,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(7)),
        );
        self.register(
            ms,
            Key::Num8,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(8)),
        );
        self.register(
            ms,
            Key::Num9,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(9)),
        );
        self.register(
            ms,
            Key::Num0,
            Action::MoveToWorkspace(WorkspaceTarget::Numbered(10)),
        );
    }
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        for hk in self.hotkeys.values() {
            unsafe { UnregisterEventHotKey(hk.carbon_ref) };
        }
        if !self._handler_ref.is_null() {
            unsafe { RemoveEventHandler(self._handler_ref) };
        }
        unsafe { drop(Box::from_raw(self._callback_ctx)) };
        tracing::info!("hotkey manager dropped");
    }
}

unsafe extern "C" fn hotkey_event_handler(
    _call_ref: EventHandlerCallRef,
    event: EventRef,
    user_data: *mut c_void,
) -> OSStatus {
    if user_data.is_null() {
        return -1;
    }

    let ctx = unsafe { &*(user_data as *const HotkeyCallbackCtx) };

    let mut hot_key_id: EventHotKeyID = EventHotKeyID {
        signature: 0,
        id: 0,
    };
    let status = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>() as u32,
            ptr::null_mut(),
            &mut hot_key_id as *mut EventHotKeyID as *mut c_void,
        )
    };

    if status != NO_ERR {
        return status;
    }

    if hot_key_id.signature != HOTKEY_SIGNATURE {
        return -1;
    }

    if let Some(action) = ctx.id_to_action.get(&hot_key_id.id) {
        tracing::debug!(?action, id = hot_key_id.id, "hotkey fired");
        (ctx.callback)(action.clone());
    }

    NO_ERR
}

fn modifiers_to_carbon(mods: Modifiers) -> u32 {
    let mut carbon = 0u32;
    if mods.contains(Modifiers::COMMAND) {
        carbon |= CMD_KEY;
    }
    if mods.contains(Modifiers::SHIFT) {
        carbon |= SHIFT_KEY;
    }
    if mods.contains(Modifiers::OPTION) {
        carbon |= OPTION_KEY;
    }
    if mods.contains(Modifiers::CONTROL) {
        carbon |= CONTROL_KEY;
    }
    carbon
}

/// Map our Key enum to Carbon virtual keycodes.
/// These are the same keycodes as CGEvent but Carbon uses u32.
fn key_to_carbon(key: Key) -> Option<u32> {
    // Our keycode_to_key maps macOS keycodes to Key.
    // We need the reverse. Build it from the known mappings.
    let keycode: u16 = match key {
        Key::A => 0x00,
        Key::S => 0x01,
        Key::D => 0x02,
        Key::F => 0x03,
        Key::H => 0x04,
        Key::G => 0x05,
        Key::Z => 0x06,
        Key::X => 0x07,
        Key::C => 0x08,
        Key::V => 0x09,
        Key::B => 0x0B,
        Key::Q => 0x0C,
        Key::W => 0x0D,
        Key::E => 0x0E,
        Key::R => 0x0F,
        Key::Y => 0x10,
        Key::T => 0x11,
        Key::Num1 => 0x12,
        Key::Num2 => 0x13,
        Key::Num3 => 0x14,
        Key::Num4 => 0x15,
        Key::Num6 => 0x16,
        Key::Num5 => 0x17,
        Key::Equal => 0x18,
        Key::Num9 => 0x19,
        Key::Num7 => 0x1A,
        Key::Minus => 0x1B,
        Key::Num8 => 0x1C,
        Key::Num0 => 0x1D,
        Key::RightBracket => 0x1E,
        Key::O => 0x1F,
        Key::U => 0x20,
        Key::LeftBracket => 0x21,
        Key::I => 0x22,
        Key::P => 0x23,
        Key::Return => 0x24,
        Key::L => 0x25,
        Key::J => 0x26,
        Key::Quote => 0x27,
        Key::K => 0x28,
        Key::Semicolon => 0x29,
        Key::Backslash => 0x2A,
        Key::Comma => 0x2B,
        Key::Slash => 0x2C,
        Key::N => 0x2D,
        Key::M => 0x2E,
        Key::Period => 0x2F,
        Key::Tab => 0x30,
        Key::Space => 0x31,
        Key::Grave => 0x32,
        Key::Delete => 0x33,
        Key::Escape => 0x35,
        Key::Left => 0x7B,
        Key::Right => 0x7C,
        Key::Down => 0x7D,
        Key::Up => 0x7E,
        Key::F1 => 0x7A,
        Key::F2 => 0x78,
        Key::F3 => 0x63,
        Key::F4 => 0x76,
        Key::F5 => 0x60,
        Key::F6 => 0x61,
        Key::F7 => 0x62,
        Key::F8 => 0x64,
        Key::F9 => 0x65,
        Key::F10 => 0x6D,
        Key::F11 => 0x67,
        Key::F12 => 0x6F,
    };
    Some(keycode as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::input::keycode_to_key;

    #[test]
    fn carbon_keycode_roundtrip() {
        // Every keycode that maps to a Key via keycode_to_key should
        // produce the same keycode back via key_to_carbon
        for code in 0x00..=0xFF_u16 {
            if let Some(key) = keycode_to_key(code) {
                let carbon = key_to_carbon(key);
                assert_eq!(
                    carbon,
                    Some(code as u32),
                    "key {:?} from keycode 0x{:02X} produced carbon keycode {:?}",
                    key,
                    code,
                    carbon
                );
            }
        }
    }

    #[test]
    fn modifier_conversion() {
        assert_eq!(modifiers_to_carbon(Modifiers::COMMAND), CMD_KEY);
        assert_eq!(modifiers_to_carbon(Modifiers::SHIFT), SHIFT_KEY);
        assert_eq!(modifiers_to_carbon(Modifiers::OPTION), OPTION_KEY);
        assert_eq!(modifiers_to_carbon(Modifiers::CONTROL), CONTROL_KEY);
        assert_eq!(
            modifiers_to_carbon(Modifiers::COMMAND | Modifiers::SHIFT),
            CMD_KEY | SHIFT_KEY
        );
    }
}
