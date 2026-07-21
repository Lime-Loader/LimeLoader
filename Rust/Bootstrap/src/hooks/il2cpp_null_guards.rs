//! Null-guards for il2cpp method-parameter APIs.
//!
//! Unity 6 ships engine modules whose managed methods are stripped - on Quest it's
//! UnityEngine.AccessibilityModule. Looking those methods up yields NULL (that is the
//! "Unable to find method ... in [UnityEngine.AccessibilityModule.dll]" spam Unity logs), and callers
//! then hand that NULL to il2cpp's parameter APIs. `il2cpp_method_get_param_count` does not null-check:
//! it tail-jumps into a leaf that reads `method->parameters_count`, a byte at +0x52
//! (`ldrb w0, [x0, #0x52]`), so a NULL method faults at address 0x52 and kills the process. That is the
//! SIGSEGV (fault addr 0x52) that had been taking the game down right after MelonLoader finished
//! initializing.
//!
//! Returning 0 / NULL for a null method is the sane behaviour - a null method has no parameters - and
//! keeps the caller's own "count == 0" path working. Guards are resolved by exported symbol NAME, so
//! they are not tied to any particular game build or offset.
//!
//! These are deliberately native hooks rather than managed detours: these exports get called from
//! Unity's own threads, which are not attached to the .NET runtime, so running managed code here would
//! be unsafe.

use std::{
    ffi::{c_char, c_void},
    ptr::null_mut,
    sync::RwLock,
};

use lazy_static::lazy_static;

use crate::{errors::DynErr, hooks::NativeHook, log, runtime};

pub type ParamCountFn = extern "C" fn(*mut c_void) -> u32;
pub type ParamFn = extern "C" fn(*mut c_void, u32) -> *mut c_void;
pub type ParamNamesFn = extern "C" fn(*mut c_void, *mut *const c_char);

lazy_static! {
    pub static ref PARAM_COUNT_HOOK: RwLock<NativeHook<ParamCountFn>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));
    pub static ref PARAM_HOOK: RwLock<NativeHook<ParamFn>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));
    pub static ref PARAM_NAMES_HOOK: RwLock<NativeHook<ParamNamesFn>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));
}

// This is the one that actually crashes: it tail-calls the +0x52 byte read with no null check.
pub extern "C" fn param_count_detour(method: *mut c_void) -> u32 {
    if method.is_null() {
        return 0;
    }
    match PARAM_COUNT_HOOK.try_read() {
        Ok(trampoline) => trampoline(method),
        Err(_) => 0,
    }
}

pub extern "C" fn param_detour(method: *mut c_void, index: u32) -> *mut c_void {
    if method.is_null() {
        return null_mut();
    }
    match PARAM_HOOK.try_read() {
        Ok(trampoline) => trampoline(method, index),
        Err(_) => null_mut(),
    }
}

pub extern "C" fn param_names_detour(method: *mut c_void, names: *mut *const c_char) {
    if method.is_null() {
        return;
    }
    if let Ok(trampoline) = PARAM_NAMES_HOOK.try_read() {
        trampoline(method, names);
    }
}

pub fn hook() -> Result<(), DynErr> {
    let runtime = runtime!()?;

    macro_rules! install {
        ($name:literal, $lock:expr, $detour:expr) => {
            match runtime.get_export_ptr($name) {
                Ok(target) => {
                    let mut guard = $lock.try_write()?;
                    *guard = NativeHook::new(target, $detour as *mut c_void);
                    match guard.hook() {
                        Ok(()) => log!("[guard] null-guarded {} at {:p}", $name, target),
                        Err(e) => log!("[guard] FAILED to hook {}: {}", $name, e),
                    }
                }
                Err(_) => log!("[guard] {} not exported; skipping", $name),
            }
        };
    }

    // The crashing path: Il2CppInterop asks for the parameter count of a method that failed to resolve.
    install!(
        "il2cpp_method_get_param_count",
        PARAM_COUNT_HOOK,
        param_count_detour
    );
    // Same object, same null: guard the indexed accessor too so a null method can't be walked.
    install!("il2cpp_method_get_param", PARAM_HOOK, param_detour);
    // The mono-compat shim reachable by the same route.
    install!(
        "mono_method_get_param_names",
        PARAM_NAMES_HOOK,
        param_names_detour
    );

    Ok(())
}
