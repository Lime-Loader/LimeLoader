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

    // Unity 6 invokes methods it failed to resolve (stripped engine modules), passing NULL. Refuse
    // those here too - while this detour is installed it sits in front of il2cpp_runtime_invoke, which
    // would otherwise dereference the method immediately. Once we detach, the native stub installed
    // below takes over this duty.
    if method.is_null() {
        return Ok(null_mut());
    }

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

        // Only now is it safe to patch il2cpp_runtime_invoke's thunk: our Dobby detour lived on the
        // same 4 bytes, so doing this while hooked would have the two fighting over them. The stub
        // takes over null-checking for the rest of the session, which keeps the detour off the hot
        // path entirely - leaving it installed instead made the support-module setup abort, since
        // every nested runtime_invoke re-entered the detour while the pipeline ran inside it.
        crate::hooks::il2cpp_null_guards::guard_thunk_only("il2cpp_runtime_invoke");

        // The ENTIRE MelonLoader init pipeline runs here rather than during il2cpp_init, where its ~5s
        // main-thread stall delayed Unity's graphics bring-up past the window/focus change and crashed
        // the render thread in vkCreateFramebuffer. Here graphics is already up, and we're still on the
        // JVM-attached main thread (so JNISharp stays valid) with a live scene for the type injection.
        //
        // ORDER MATTERS: init() must come before the mono thread reset. mono_lib!() only loads
        // libcoreclr, it does not initialize the runtime - resetting the thread first makes mono
        // pthread_kill a thread that doesn't exist yet and abort on
        // "mono-threads-posix-signals.c:340, condition `ret != -1' not met".
        crate::diag!("pipeline: scene change seen, starting base_assembly::init");
        base_assembly::init(runtime!()?)?;
        crate::diag!("pipeline: base_assembly::init returned");

        // CoreCLR installs its own signal handlers during init and "handles" SIGSEGV by printing a bare
        // instruction pointer and exiting - which is why no tombstone and no stack has ever been
        // available. Re-arm ours now so it runs FIRST and can record registers and a real backtrace,
        // then chains to CoreCLR so behaviour is otherwise unchanged.
        #[cfg(feature = "diagnostics")]
        {
            crate::diagnostics::crash_handler::install();
            // Re-dump: the runtime has mapped libcoreclr and friends since the first snapshot.
            crate::diagnostics::dump_maps();
        }

        debug!("Resetting mono thread")?;
        let lib = crate::mono_lib!()?;
        let thread_suspend_reload = lib.exports.mono_melonloader_thread_suspend_reload.as_ref().unwrap();
        thread_suspend_reload();
        debug!("Mono thread reset")?;

        crate::diag!("pipeline: starting pre_start (generator + interop preload)");
        base_assembly::pre_start()?;
        crate::diag!("pipeline: pre_start returned; starting start (support module + injection)");
        base_assembly::start()?;
        crate::diag!("pipeline: start returned - MelonLoader fully initialized");
    }

    Ok(result)
}
