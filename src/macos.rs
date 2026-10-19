//! Small macOS system queries: privacy permissions and secure input.

use std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

/// Accessibility permission: needed to type or paste into other apps.
pub fn accessibility_trusted() -> bool {
    // SAFETY: argument-less query with no preconditions.
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Input Monitoring permission: needed to see the global hotkey.
pub fn input_monitoring_granted() -> bool {
    // SAFETY: argument-less query with no preconditions.
    unsafe { CGPreflightListenEventAccess() }
}

pub fn permissions_granted() -> bool {
    accessibility_trusted() && input_monitoring_granted()
}

/// Shows the system prompts for any missing permission. macOS also adds this app to the
/// relevant lists in System Settings, so the user only has to flip the switch.
pub fn request_missing_permissions() {
    if !accessibility_trusted() {
        // SAFETY: a one-entry CFDictionary built from valid CF constants, released after use.
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            );
            if !options.is_null() {
                AXIsProcessTrustedWithOptions(options);
                CFRelease(options);
            }
        }
    }
    if !input_monitoring_granted() {
        // SAFETY: argument-less request with no preconditions.
        unsafe {
            CGRequestListenEventAccess();
        }
    }
}

/// True while a password field (or an app like a terminal with "Secure Keyboard Entry") has
/// secure input enabled. Synthetic keystrokes are silently discarded in that state.
pub fn secure_input_enabled() -> bool {
    // SAFETY: argument-less query with no preconditions.
    unsafe { IsSecureEventInputEnabled() != 0 }
}

pub const PERMISSIONS_HELP: &str = "Enable Wispr (or your terminal, when run from one) in System Settings > Privacy & Security \
     under Accessibility and Input Monitoring; wispr restarts itself once both are granted.";

unsafe extern "C" {
    fn _exit(code: i32) -> !;
}

/// Terminates immediately, skipping atexit handlers and static destructors.
pub fn force_exit(code: i32) -> ! {
    // SAFETY: _exit is async-signal-safe and never returns.
    unsafe { _exit(code) }
}

/// `struct tm` from <time.h> on macOS.
#[repr(C)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: std::ffi::c_long,
    zone: *const std::ffi::c_char,
}

unsafe extern "C" {
    fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
}

/// Current local time as `YYYY-MM-DD HH:MM:SS`, without pulling in a date crate.
pub fn local_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: localtime_r is thread-safe and fully initializes `tm` when it returns non-null.
    let ok = unsafe { !localtime_r(&secs, tm.as_mut_ptr()).is_null() };
    if !ok {
        return String::new();
    }
    // SAFETY: initialized by the successful localtime_r call above.
    let tm = unsafe { tm.assume_init() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.year + 1900,
        tm.mon + 1,
        tm.mday,
        tm.hour,
        tm.min,
        tm.sec
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_timestamp_shape() {
        let ts = super::local_timestamp();
        assert_eq!(ts.len(), 19, "{ts}");
        assert!(ts.starts_with("20"));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[13..14], ":");
    }
}
