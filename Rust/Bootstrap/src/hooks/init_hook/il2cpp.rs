use std::{ffi::c_char, sync::RwLock, ptr::null_mut, fs, env, path::Path};

use lazy_static::lazy_static;
use unity_rs::il2cpp::types::Il2CppDomain;

use crate::{
    console, constants::InitFnIl2Cpp, debug, errors::DynErr, hooks::{NativeHook, invoke_hook},
    internal_failure,
};

lazy_static! {
    pub static ref INIT_HOOK: RwLock<NativeHook<InitFnIl2Cpp>> =
        RwLock::new(NativeHook::new(null_mut(), null_mut()));
}

pub fn detour(name: *const c_char) -> *mut Il2CppDomain {
    detour_inner(name).unwrap_or_else(|e| {
        internal_failure!("il2cpp_init detour failed: {e}");
    })
}

fn detour_inner(name: *const c_char) -> Result<*mut Il2CppDomain, DynErr> {
    console::set_handles()?;

    let ssl_cert_path = "/apex/com.android.conscrypt/cacerts";
    let backup_cert_path = "/system/etc/security/cacerts";

    if Path::new(ssl_cert_path).exists() && is_readable(ssl_cert_path) {
        env::set_var("SSL_CERT_DIR", ssl_cert_path);
    } else if Path::new(backup_cert_path).exists() && is_readable(backup_cert_path) {
        env::set_var("SSL_CERT_DIR", backup_cert_path);
    } else {
        debug!("No readable SSL cert file found; HTTPS requests may fail.");
    }

    debug!(
        "Using {} for SSL certificates",
        env::var("SSL_CERT_DIR").unwrap_or_default()
    );

    let trampoline = INIT_HOOK.try_read()?;
    let domain = trampoline(name);

    // DIAGNOSTIC (0.1.11): skip ALL MelonLoader init so il2cpp_init returns immediately, leaving Unity's
    // main thread unblocked for its XR/Vulkan graphics bring-up. If the game renders with this, the
    // multi-second init block is confirmed to be what mistimes Unity's framebuffer creation (-> we then
    // eliminate the block via a pre-Unity loader / off-main init). If it STILL crashes, the block is not
    // the cause and that whole direction is a dead end. Set back to false for a real build.
    const SKIP_MELONLOADER_INIT: bool = true;
    if SKIP_MELONLOADER_INIT {
        crate::log!("[DIAG] Skipping MelonLoader init - testing Unity graphics bring-up with no il2cpp_init block");
        trampoline.unhook()?;
        return Ok(domain);
    }

    crate::base_assembly::init(crate::runtime!()?)?;

    // Run pre-start (il2cpp assembly generator check + interop assembly pre-load) HERE, right after
    // il2cpp is up, instead of deferring it to the first scene change. The generator only needs
    // libil2cpp (already available) and the pre-load is pure managed assembly loading - neither needs
    // Unity's scene. Getting that heavy work done before Unity renders its first frame keeps the
    // render-critical scene-change hook from stalling the main thread, which crashed Unity 6's Vulkan
    // framebuffer setup on Quest. Only the fast type injection (start) stays on the scene-change hook.
    crate::base_assembly::pre_start()?;

    debug!("Detaching hook from il2cpp_init")?;
    trampoline.unhook()?;
    
    invoke_hook::hook()?;

    Ok(domain)
}

fn is_readable(path: &str) -> bool {
    fs::read_dir(path).is_ok()
}