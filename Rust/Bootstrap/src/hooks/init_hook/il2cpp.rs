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

    // Do NOT run MelonLoader's init here. Its ~5s of work (.NET load, Harmony/MonoMod fixes, assembly
    // generator, interop preload) blocks Unity's main thread during il2cpp_init, which delays Unity's
    // XR/Vulkan graphics bring-up until after the window/focus has settled - and Unity then segfaults
    // creating its framebuffer (confirmed on Quest / Unity 6: with this init skipped, the game boots
    // fine). Instead, defer the ENTIRE init pipeline to the runtime_invoke hook, which runs it a moment
    // after the first scene change - i.e. after Unity's graphics is already up. See invoke_hook/il2cpp.rs.
    debug!("Detaching hook from il2cpp_init")?;
    trampoline.unhook()?;

    // Install BEFORE anything resolves methods: Unity 6's stripped engine modules yield NULL
    // MethodInfo*, and il2cpp's parameter APIs deref that without a null check.
    crate::hooks::il2cpp_null_guards::hook()?;

    invoke_hook::hook()?;

    Ok(domain)
}

fn is_readable(path: &str) -> bool {
    fs::read_dir(path).is_ok()
}