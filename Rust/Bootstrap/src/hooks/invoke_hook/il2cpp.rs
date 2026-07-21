use std::{
    ffi::c_void,
    ptr::null_mut,
    sync::{atomic::{AtomicBool, Ordering}, Mutex, RwLock},
    time::Instant,
};

use lazy_static::lazy_static;
use unity_rs::il2cpp::types::{Il2CppMethod, Il2CppObject};

use crate::{base_assembly, constants::InvokeFnIl2Cpp, debug, errors::DynErr, hooks::NativeHook, internal_failure, runtime};

lazy_static! {
    pub static ref INVOKE_HOOK: RwLock<NativeHook<InvokeFnIl2Cpp>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));

    // When the first il2cpp_runtime_invoke came through, i.e. when Unity started running managed code.
    static ref FIRST_INVOKE: Mutex<Option<Instant>> = Mutex::new(None);
}

// Whether the init pipeline has already run; once true this hook is a no-op passthrough.
static STARTED: AtomicBool = AtomicBool::new(false);

// How long to let Unity run before we execute MelonLoader's ENTIRE init pipeline (init + pre_start +
// start). That work blocks the Unity main thread for ~5s; doing it during il2cpp_init delayed Unity's
// XR/Vulkan graphics bring-up until after the window/focus had settled, and segfaulted the render
// thread in vkCreateFramebuffer on Quest / Unity 6. Waiting until Unity has been running managed code
// for a few seconds means graphics is up and a scene is live (needed for the type injection in start),
// so our stall can no longer mistime the framebuffer creation.
const INIT_DELAY_MS: u128 = 5000;

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

    // Already ran -> pure passthrough (hook is normally unhooked by then, but guard anyway).
    if STARTED.load(Ordering::Relaxed) {
        return Ok(result);
    }

    // NOTE: deliberately do NOT inspect the invoked method here. Resolving the method name on every
    // runtime_invoke (to watch for Internal_ActiveSceneChanged) is both very hot once Unity reaches its
    // main loop and unsafe - it segfaulted inside Unity::UnityApplication::ProcessFrame on Quest. A
    // plain elapsed-time trigger needs none of that.
    let first_invoke = *FIRST_INVOKE.lock().unwrap();

    let first_invoke = match first_invoke {
        Some(t) => t,
        None => {
            let now = Instant::now();
            *FIRST_INVOKE.lock().unwrap() = Some(now);
            debug!("Unity is running managed code; MelonLoader init in {}ms", INIT_DELAY_MS)?;
            return Ok(result);
        }
    };

    if first_invoke.elapsed().as_millis() >= INIT_DELAY_MS
        && !STARTED.swap(true, Ordering::Relaxed)
    {
        debug!("Detaching hook from il2cpp_runtime_invoke")?;
        trampoline.unhook()?;

        debug!("Resetting mono thread")?;
        let lib = crate::mono_lib!()?;
        let thread_suspend_reload = lib.exports.mono_melonloader_thread_suspend_reload.as_ref().unwrap();
        thread_suspend_reload();
        debug!("Mono thread reset")?;

        // Run the ENTIRE MelonLoader init pipeline HERE, now that Unity's graphics is already up, so the
        // ~5s block can no longer mistime Unity's framebuffer creation. This runs on Unity's main thread
        // (this runtime_invoke is JVM-attached, so JNISharp's JNI calls stay valid) and once a scene is
        // live, which the type injection in start() needs.
        base_assembly::init(runtime!()?)?;
        base_assembly::pre_start()?;
        base_assembly::start()?;
    }

    Ok(result)
}
