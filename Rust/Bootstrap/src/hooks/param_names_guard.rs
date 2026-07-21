//! Null-guard for libil2cpp's exported `mono_method_get_param_names`.
//!
//! Unity 6 ships engine modules whose managed methods are stripped - on Quest it's
//! UnityEngine.AccessibilityModule. When Unity registers those modules its own method lookup fails
//! (that is the "Unable to find method ... in [UnityEngine.AccessibilityModule.dll]" spam it logs),
//! and it then calls mono_method_get_param_names with a NULL MonoMethod*. The game's il2cpp mono-compat
//! shim does not null-check it: the very first thing it does is read method->parameters_count, a byte
//! at +0x52 (`ldrb w0, [x0, #0x52]`), so with a null method it faults at address 0x52 and takes the
//! whole process down - which is the SIGSEGV that has been killing the game right after MelonLoader
//! finishes initializing.
//!
//! Returning early for a null method is the correct behaviour (a null method has no parameter names)
//! and matches what mono itself does. The guard is deliberately implemented natively rather than as a
//! managed detour: this export is called from Unity's own threads, which are not attached to the .NET
//! runtime, so a managed detour here would be unsafe.

use std::{
    ffi::{c_char, c_void},
    ptr::null_mut,
    sync::RwLock,
};

use lazy_static::lazy_static;

use crate::{debug, errors::DynErr, hooks::NativeHook, runtime};

pub type ParamNamesFn = extern "C" fn(*mut c_void, *mut *const c_char);

lazy_static! {
    pub static ref PARAM_NAMES_HOOK: RwLock<NativeHook<ParamNamesFn>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));
}

pub extern "C" fn detour(method: *mut c_void, names: *mut *const c_char) {
    // The whole point of the hook: swallow the null case instead of letting il2cpp deref it.
    if method.is_null() {
        return;
    }

    // Must not panic on an arbitrary Unity thread - if the lock is unavailable, do nothing.
    if let Ok(trampoline) = PARAM_NAMES_HOOK.try_read() {
        trampoline(method, names);
    }
}

pub fn hook() -> Result<(), DynErr> {
    let runtime = runtime!()?;

    // Resolved by symbol name (it is an exported mono-compat API), so this is not tied to any
    // particular game build or offset. If a game doesn't export it, there is nothing to guard.
    let target = match runtime.get_export_ptr("mono_method_get_param_names") {
        Ok(ptr) => ptr,
        Err(_) => {
            debug!("mono_method_get_param_names not exported; skipping null guard")?;
            return Ok(());
        }
    };

    debug!("Attaching null guard to mono_method_get_param_names")?;

    let mut guard = PARAM_NAMES_HOOK.try_write()?;
    *guard = NativeHook::new(target, detour as *mut c_void);
    guard.hook()?;

    Ok(())
}
