use std::{
    ffi::c_void,
    ptr::null_mut,
    sync::{atomic::{AtomicBool, Ordering}, RwLock},
};

use lazy_static::lazy_static;
use unity_rs::{common::method::UnityMethod, il2cpp::types::{Il2CppMethod, Il2CppObject}};

use crate::{base_assembly, constants::InvokeFnIl2Cpp, debug, errors::DynErr, hooks::NativeHook, internal_failure, runtime};

lazy_static! {
    pub static ref INVOKE_HOOK: RwLock<NativeHook<InvokeFnIl2Cpp>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));
}

// Whether the init pipeline has already run; once true this hook is a no-op passthrough.
static STARTED: AtomicBool = AtomicBool::new(false);

pub fn detour(
    method: *mut Il2CppMethod,
    obj: *mut Il2CppObject,
    params: *mut *mut c_void,
    exc: *mut *mut Il2CppObject,
) -> *mut Il2CppObject {
    detour_inner(method, obj, params, exc).unwrap_or_else(|e| {
        internal_failure!("il2cpp_runtime_invoke detour failed: {e}");
    })
}

fn detour_inner(
    method: *mut Il2CppMethod,
    obj: *mut Il2CppObject,
    params: *mut *mut c_void,
    exc: *mut *mut Il2CppObject,
) -> Result<*mut Il2CppObject, DynErr> {
    let trampoline = INVOKE_HOOK.try_read()?;
    let result = trampoline(method, obj, params, exc);

    if STARTED.load(Ordering::Relaxed) {
        return Ok(result);
    }

    let runtime = runtime!()?;
    let safe_method = UnityMethod::new(method.cast())?;
    let name = safe_method.get_name(runtime)?;

    // Trigger on the FIRST active-scene change and detach immediately. This hook must never stay live
    // into Unity's main loop: keeping it installed a few seconds longer (to delay init) segfaulted
    // inside libil2cpp under Unity::UnityApplication::ProcessFrame on Quest. A scene also cannot become
    // active before Unity's graphics device exists, so by here the XR/Vulkan bring-up is already done -
    // which is exactly what the init pipeline must not interrupt.
    if name.contains("Internal_ActiveSceneChanged") && !STARTED.swap(true, Ordering::Relaxed) {
        debug!("Detaching hook from il2cpp_runtime_invoke")?;
        trampoline.unhook()?;

        // The ENTIRE MelonLoader init pipeline runs here rather than during il2cpp_init, where its ~5s
        // main-thread stall delayed Unity's graphics bring-up past the window/focus change and crashed
        // the render thread in vkCreateFramebuffer. Here graphics is already up, and we're still on the
        // JVM-attached main thread (so JNISharp stays valid) with a live scene for the type injection.
        //
        // ORDER MATTERS: init() must come before the mono thread reset. mono_lib!() only loads
        // libcoreclr, it does not initialize the runtime - resetting the thread first makes mono
        // pthread_kill a thread that doesn't exist yet and abort on
        // "mono-threads-posix-signals.c:340, condition `ret != -1' not met".
        base_assembly::init(runtime!()?)?;

        debug!("Resetting mono thread")?;
        let lib = crate::mono_lib!()?;
        let thread_suspend_reload = lib.exports.mono_melonloader_thread_suspend_reload.as_ref().unwrap();
        thread_suspend_reload();
        debug!("Mono thread reset")?;

        base_assembly::pre_start()?;
        base_assembly::start()?;

        // DIAGNOSTIC (0.1.17): the crash lands ~130ms after this point and CoreCLR swallows the SIGSEGV
        // (so debuggerd never writes a tombstone), leaving only a fault IP with no library attribution.
        // Dump the executable mappings here so that IP can be resolved to an actual .so - every run so
        // far has faulted at the same tiny `ldrb w0,[x0]; ret` function, but in an unknown library.
        crate::log!("[DIAG] executable maps follow - correlate with the crash-report fault IP");
        if let Ok(maps) = std::fs::read_to_string("/proc/self/maps") {
            for line in maps.lines() {
                if line.contains("r-xp") && line.contains('/') {
                    crate::log!("[MAPS] {}", line);
                }
            }
        }
        crate::log!("[DIAG] end of executable maps");
    }

    Ok(result)
}
