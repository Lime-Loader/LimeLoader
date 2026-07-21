use std::{
    ffi::c_void,
    ptr::null_mut,
    sync::{atomic::{AtomicBool, Ordering}, Mutex, RwLock},
    time::Instant,
};

use lazy_static::lazy_static;
use unity_rs::{common::method::UnityMethod, il2cpp::types::{Il2CppMethod, Il2CppObject}};

use crate::{base_assembly, constants::InvokeFnIl2Cpp, debug, errors::DynErr, hooks::NativeHook, internal_failure, runtime};

lazy_static! {
    pub static ref INVOKE_HOOK: RwLock<NativeHook<InvokeFnIl2Cpp>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));

    // When the first active-scene change was seen. Used to delay MelonLoader's Start (see below).
    static ref FIRST_SCENE_CHANGE: Mutex<Option<Instant>> = Mutex::new(None);
}

// Whether Start() has already run; once true this hook is a no-op passthrough.
static STARTED: AtomicBool = AtomicBool::new(false);

// How long to let Unity keep running after the FIRST active-scene change before we run MelonLoader's
// Start (support module + type injection). Start blocks the Unity main thread for ~1s, and doing that
// immediately on the first scene change raced Unity's first-frame Vulkan framebuffer creation - the
// render thread segfaulted in vkCreateFramebuffer on Quest / Unity 6. Letting Unity render its first
// frames first means the swapchain/framebuffer already exists before we stall the main thread.
const START_DELAY_MS: u128 = 2500;

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

    // Already started -> pure passthrough (hook is normally unhooked, but guard anyway).
    if STARTED.load(Ordering::Relaxed) {
        return Ok(result);
    }

    let scene_change_time = *FIRST_SCENE_CHANGE.lock().unwrap();

    let scene_change_time = match scene_change_time {
        Some(t) => t,
        None => {
            // Still waiting for the first active-scene change - only then is a scene live enough to
            // inject into. Checking the method name is why we keep the hook active until here.
            let runtime = runtime!()?;
            let safe_method = UnityMethod::new(method.cast())?;
            let name = safe_method.get_name(runtime)?;

            if name.contains("Internal_ActiveSceneChanged") {
                *FIRST_SCENE_CHANGE.lock().unwrap() = Some(Instant::now());
                debug!("First scene change; delaying MelonLoader Start {}ms so Unity can render first", START_DELAY_MS)?;
            }

            return Ok(result);
        }
    };

    // Scene is up; once enough time has passed run Start exactly once.
    if scene_change_time.elapsed().as_millis() >= START_DELAY_MS
        && !STARTED.swap(true, Ordering::Relaxed)
    {
        debug!("Detaching hook from il2cpp_runtime_invoke")?;
        trampoline.unhook()?;

        debug!("Resetting mono thread")?;
        let lib = crate::mono_lib!()?;
        let thread_suspend_reload = lib.exports.mono_melonloader_thread_suspend_reload.as_ref().unwrap();
        thread_suspend_reload();
        debug!("Mono thread reset")?;

        // pre_start (generator + interop assembly pre-load) already ran during il2cpp_init; only the
        // fast type injection runs here, now that Unity has had time to render its first frames.
        base_assembly::start()?;
    }

    Ok(result)
}
