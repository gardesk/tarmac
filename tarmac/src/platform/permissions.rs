use std::ffi::c_void;
use std::ptr;

// Core Foundation C types
type CFStringRef = *const c_void;
type CFBooleanRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFTypeRef = *const c_void;

unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    fn CFRelease(cf: *const c_void);

    static kCFBooleanTrue: CFBooleanRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

// AXTrustedCheckOptionPrompt key
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

/// Check if the process has accessibility permissions.
/// Prompts the user to grant access if not already trusted.
pub fn check_accessibility() -> bool {
    unsafe {
        let key: CFTypeRef = kAXTrustedCheckOptionPrompt;
        let value: CFTypeRef = kCFBooleanTrue;

        let options = CFDictionaryCreate(
            ptr::null(),
            &key,
            &value,
            1,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );

        let trusted = AXIsProcessTrustedWithOptions(options);
        CFRelease(options);
        trusted
    }
}
