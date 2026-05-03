use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType};

// Private API: get CGWindowID from an AXUIElement
pub type CGWindowID = u32;

unsafe extern "C" {
    fn _AXUIElementGetWindow(element: &AXUIElement, window_id: *mut CGWindowID) -> AXError;
}

/// Errors from AX operations.
#[derive(Debug, Clone)]
pub enum AxError {
    Ax(AXError),
    NullValue,
    TypeMismatch,
}

impl std::fmt::Display for AxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AxError::Ax(err) => write!(f, "AXError({:?})", err),
            AxError::NullValue => write!(f, "null value"),
            AxError::TypeMismatch => write!(f, "type mismatch"),
        }
    }
}

impl std::error::Error for AxError {}

pub type AxResult<T> = Result<T, AxError>;

fn check(err: AXError) -> AxResult<()> {
    if err == AXError(0) {
        // AXError::Success = 0
        Ok(())
    } else {
        Err(AxError::Ax(err))
    }
}

/// Get a single attribute value from an AXUIElement.
pub fn ax_copy_attribute(element: &AXUIElement, attr: &CFString) -> AxResult<CFRetained<CFType>> {
    let mut value: *const CFType = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(
            element,
            attr,
            NonNull::new(&mut value as *mut *const CFType).unwrap(),
        )
    };
    check(err)?;
    if value.is_null() {
        return Err(AxError::NullValue);
    }
    Ok(unsafe { CFRetained::from_raw(NonNull::new(value as *mut CFType).unwrap()) })
}

/// Get a string attribute.
pub fn ax_get_string(element: &AXUIElement, attr_name: &'static str) -> AxResult<String> {
    let attr = CFString::from_static_str(attr_name);
    let value = ax_copy_attribute(element, &attr)?;
    // Downcast CFType to CFString
    let s: &CFString = unsafe { &*(value.as_ref() as *const CFType as *const CFString) };
    Ok(s.to_string())
}

/// Get window position as (x, y).
pub fn ax_get_position(element: &AXUIElement) -> AxResult<(f64, f64)> {
    let attr = CFString::from_static_str("AXPosition");
    let value = ax_copy_attribute(element, &attr)?;
    let ax_value: &AXValue = unsafe { &*(value.as_ref() as *const CFType as *const AXValue) };

    #[repr(C)]
    #[derive(Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    let mut point = CGPoint::default();
    let ok = unsafe {
        AXValueGetValue(
            ax_value,
            AXValueType(1), // kAXValueCGPointType
            (&mut point as *mut CGPoint).cast::<c_void>(),
        )
    };
    if ok {
        Ok((point.x, point.y))
    } else {
        Err(AxError::TypeMismatch)
    }
}

/// Get window size as (width, height).
pub fn ax_get_size(element: &AXUIElement) -> AxResult<(f64, f64)> {
    let attr = CFString::from_static_str("AXSize");
    let value = ax_copy_attribute(element, &attr)?;
    let ax_value: &AXValue = unsafe { &*(value.as_ref() as *const CFType as *const AXValue) };

    #[repr(C)]
    #[derive(Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    let mut size = CGSize::default();
    let ok = unsafe {
        AXValueGetValue(
            ax_value,
            AXValueType(2), // kAXValueCGSizeType
            (&mut size as *mut CGSize).cast::<c_void>(),
        )
    };
    if ok {
        Ok((size.width, size.height))
    } else {
        Err(AxError::TypeMismatch)
    }
}

/// Set window position.
pub fn ax_set_position(element: &AXUIElement, x: f64, y: f64) -> AxResult<()> {
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    let mut point = CGPoint { x, y };
    let value = unsafe {
        AXValueCreate(
            AXValueType(1), // kAXValueCGPointType
            (&mut point as *mut CGPoint).cast::<c_void>(),
        )
    };
    if value.is_null() {
        return Err(AxError::NullValue);
    }
    let attr = CFString::from_static_str("AXPosition");
    let err = unsafe {
        AXUIElementSetAttributeValue(element, &attr, &*(value as *const _ as *const CFType))
    };
    unsafe { CFRelease(value as *const c_void) };
    check(err)
}

/// Set window size.
pub fn ax_set_size(element: &AXUIElement, w: f64, h: f64) -> AxResult<()> {
    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    let mut size = CGSize {
        width: w,
        height: h,
    };
    let value = unsafe {
        AXValueCreate(
            AXValueType(2), // kAXValueCGSizeType
            (&mut size as *mut CGSize).cast::<c_void>(),
        )
    };
    if value.is_null() {
        return Err(AxError::NullValue);
    }
    let attr = CFString::from_static_str("AXSize");
    let err = unsafe {
        AXUIElementSetAttributeValue(element, &attr, &*(value as *const _ as *const CFType))
    };
    unsafe { CFRelease(value as *const c_void) };
    check(err)
}

/// Set a boolean attribute (used for AXFrontmost to activate apps).
pub fn ax_set_bool(element: &AXUIElement, attr: &CFString, value: bool) -> AxResult<()> {
    let cf_bool: *const c_void = if value {
        unsafe { kCFBooleanTrue }
    } else {
        unsafe { kCFBooleanFalse }
    };
    let err = unsafe {
        AXUIElementSetAttributeValue(element, attr, &*(cf_bool as *const _ as *const CFType))
    };
    check(err)
}

/// Get a boolean attribute value from an AXUIElement.
pub fn ax_get_bool(element: &AXUIElement, attr: &CFString) -> AxResult<bool> {
    let value = ax_copy_attribute(element, attr)?;
    let ptr = &*value as *const CFType as *const c_void;
    Ok(ptr == unsafe { kCFBooleanTrue })
}

/// Perform an action on an AXUIElement (e.g., AXRaise, AXPress).
pub fn ax_perform_action(element: &AXUIElement, action: &'static str) -> AxResult<()> {
    let action_str = CFString::from_static_str(action);
    let err = unsafe { AXUIElementPerformAction(element, &action_str) };
    check(err)
}

/// Get CGWindowID from an AXUIElement (private API).
pub fn ax_get_window_id(element: &AXUIElement) -> AxResult<CGWindowID> {
    let mut window_id: CGWindowID = 0;
    let err = unsafe { _AXUIElementGetWindow(element, &mut window_id) };
    check(err)?;
    Ok(window_id)
}

/// Get the windows array from an application AXUIElement.
pub fn ax_get_windows(app_element: &AXUIElement) -> AxResult<Vec<CFRetained<AXUIElement>>> {
    let attr = CFString::from_static_str("AXWindows");
    let value = match ax_copy_attribute(app_element, &attr) {
        Ok(v) => v,
        Err(AxError::Ax(AXError(-25212))) => return Ok(Vec::new()), // cannotComplete
        Err(e) => return Err(e),
    };

    let array: &CFArray = unsafe { &*(value.as_ref() as *const CFType as *const CFArray) };
    let count = unsafe { CFArrayGetCount(array) } as usize;
    let mut windows = Vec::with_capacity(count);

    for i in 0..count {
        let elem_ptr = unsafe { CFArrayGetValueAtIndex(array, i as isize) };
        if !elem_ptr.is_null() {
            let elem =
                unsafe { CFRetained::retain(NonNull::new(elem_ptr as *mut AXUIElement).unwrap()) };
            windows.push(elem);
        }
    }

    Ok(windows)
}

/// Check if a window is a manageable type (standard window or dialog).
pub fn is_manageable_window(element: &AXUIElement) -> bool {
    let role = match ax_get_string(element, "AXRole") {
        Ok(r) => r,
        Err(_) => return false,
    };
    let subrole = ax_get_string(element, "AXSubrole").unwrap_or_default();
    let title = ax_get_string(element, "AXTitle").unwrap_or_default();

    if role != "AXWindow" {
        tracing::trace!(role, subrole, title, "skipping non-AXWindow");
        return false;
    }

    let dominated = matches!(
        subrole.as_str(),
        "AXStandardWindow"
            | "AXDialog"
            | "AXSystemDialog"
            | "AXSheet"
            | "AXFloatingWindow"
            | "AXSystemFloatingWindow"
    );
    if !dominated {
        tracing::trace!(role, subrole, title, "skipping non-standard subrole");
    }
    dominated
}

// Raw C FFI for functions not yet in objc2-application-services with the right signatures
unsafe extern "C" {
    fn AXUIElementCopyAttributeValue(
        element: &AXUIElement,
        attribute: &CFString,
        value: NonNull<*const CFType>,
    ) -> AXError;

    fn AXUIElementSetAttributeValue(
        element: &AXUIElement,
        attribute: &CFString,
        value: &CFType,
    ) -> AXError;

    fn AXUIElementPerformAction(element: &AXUIElement, action: &CFString) -> AXError;

    fn AXValueCreate(r#type: AXValueType, value: *const c_void) -> *const AXValue;

    fn AXValueGetValue(value: &AXValue, r#type: AXValueType, value_ptr: *mut c_void) -> bool;

    fn CFArrayGetCount(array: &CFArray) -> isize;

    fn CFArrayGetValueAtIndex(array: &CFArray, idx: isize) -> *const c_void;

    fn CFRelease(cf: *const c_void);

    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
}
