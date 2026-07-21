//! Null-guards for il2cpp's method-parameter APIs.
//!
//! # The bug being worked around
//!
//! Unity 6 ships engine modules whose managed methods are stripped - on Quest it is
//! UnityEngine.AccessibilityModule. Unity looks those methods up, the lookup returns NULL (that is the
//! "Unable to find method ... in [UnityEngine.AccessibilityModule.dll]" spam it logs), and Unity then
//! passes the NULL straight to `il2cpp_method_get_param_count` without checking it. il2cpp reads
//! `method->parameters_count` - a byte at +0x52 - so the process dies with SIGSEGV at fault address
//! 0x52.
//!
//! A crash-handler backtrace confirmed the caller is libunity.so itself, reached from libgame.so's
//! main loop, with no MelonLoader frames anywhere on the stack: this is a Unity bug that MelonLoader's
//! presence merely causes the engine to reach. We cannot fix Unity, so we make the il2cpp side
//! tolerate the NULL, which is the sane behaviour anyway - a null method has no parameters.
//!
//! # Why this is not build-specific
//!
//! Nothing here hardcodes an address. We resolve the EXPORTED symbol by name and then *decode its
//! first instruction* to decide how to patch it:
//!
//! * If it is an unconditional `B` the export is a tail-call thunk - the whole function is that single
//!   4-byte branch. We rewrite exactly those 4 bytes to branch to our stub, so neighbouring functions
//!   cannot be damaged. (An earlier attempt used a ~16-byte inline detour here and corrupted the
//!   adjacent `il2cpp_method_get_param`, which sits only 4 bytes away, producing a worse crash.)
//! * Otherwise it is a normal function with a prologue, and a conventional inline hook is safe.
//!
//! The stub is generated at runtime and simply does `if (method == NULL) return 0;` before tail-calling
//! the original target, so it works for any build regardless of layout.

use std::{ffi::c_void, sync::RwLock};

use lazy_static::lazy_static;

use crate::{debug, errors::DynErr, hooks::NativeHook, log, runtime};

/// `uint32_t il2cpp_method_get_param_count(const MethodInfo*)` and
/// `const Il2CppType* il2cpp_method_get_param(const MethodInfo*, uint32_t)` - both take the method in
/// x0, and for both "0" is the correct answer for a null method (count 0 / null pointer).
pub type ParamCountFn = extern "C" fn(*mut c_void) -> u64;

lazy_static! {
    // One slot per guarded export. A shared Vec would not work: the detour has no way to tell which
    // export it was entered for, so each needs its own hook and its own detour.
    static ref PARAM_COUNT_HOOK: RwLock<NativeHook<ParamCountFn>> =
        RwLock::new(NativeHook::new(std::ptr::null_mut(), std::ptr::null_mut()));
    static ref PARAM_HOOK: RwLock<NativeHook<ParamCountFn>> =
        RwLock::new(NativeHook::new(std::ptr::null_mut(), std::ptr::null_mut()));
}

// ---------------------------------------------------------------------------------------------
// AArch64 encoding helpers
// ---------------------------------------------------------------------------------------------

const B_OPCODE: u32 = 0x1400_0000;
const B_MASK: u32 = 0xFC00_0000;
const RET: u32 = 0xD65F_03C0;
const MOV_W0_0: u32 = 0x5280_0000;
/// `cbz x0, #8` - skip the tail-call and fall into the "return 0" tail.
const CBZ_X0_PLUS8: u32 = 0xB400_0040;

/// Maximum reach of a B instruction: +/-128MB.
const BRANCH_RANGE: i64 = 128 * 1024 * 1024;

fn is_uncond_branch(insn: u32) -> bool {
    insn & B_MASK == B_OPCODE
}

/// Decode the destination of a `B` at `pc`.
fn branch_target(pc: usize, insn: u32) -> usize {
    let mut imm = (insn & 0x03FF_FFFF) as i64;
    if imm & 0x0200_0000 != 0 {
        imm -= 0x0400_0000; // sign extend imm26
    }
    (pc as i64 + (imm << 2)) as usize
}

/// Encode a `B` from `pc` to `target`, if it is in range.
fn encode_branch(pc: usize, target: usize) -> Option<u32> {
    let delta = target as i64 - pc as i64;
    if delta.abs() >= BRANCH_RANGE || delta & 3 != 0 {
        return None;
    }
    Some(B_OPCODE | (((delta >> 2) as u32) & 0x03FF_FFFF))
}

/// Make instruction memory visible to the instruction fetcher. Patched code is useless without this.
#[cfg(target_arch = "aarch64")]
unsafe fn flush_icache(addr: usize, len: usize) {
    let start = addr & !63;
    let end = addr + len;

    let mut p = start;
    while p < end {
        std::arch::asm!("dc cvau, {addr}", addr = in(reg) p, options(nostack, preserves_flags));
        p += 64;
    }
    std::arch::asm!("dsb ish", options(nostack, preserves_flags));

    let mut p = start;
    while p < end {
        std::arch::asm!("ic ivau, {addr}", addr = in(reg) p, options(nostack, preserves_flags));
        p += 64;
    }
    std::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn flush_icache(_addr: usize, _len: usize) {}

/// Temporarily make the page(s) covering `addr..addr+len` writable, run `f`, then restore.
unsafe fn with_writable<R>(addr: usize, len: usize, f: impl FnOnce() -> R) -> Option<R> {
    let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
    let start = addr & !(page - 1);
    let end = (addr + len + page - 1) & !(page - 1);
    let size = end - start;

    if libc::mprotect(
        start as *mut c_void,
        size,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
    ) != 0
    {
        return None;
    }

    let result = f();

    // Back to r-x; failing to restore is not fatal, the patch is already in place.
    let _ = libc::mprotect(start as *mut c_void, size, libc::PROT_READ | libc::PROT_EXEC);

    Some(result)
}

/// Allocate an executable page within branch range of `near`, so a single `B` can reach it.
unsafe fn alloc_stub_near(near: usize) -> Option<*mut u32> {
    let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;

    // Walk outwards from the target until a hint lands close enough to be reachable.
    let mut step = 0usize;
    while (step as i64) < BRANCH_RANGE {
        for direction in [1i64, -1] {
            let hint = (near as i64 + direction * step as i64) as usize & !(page - 1);
            if hint == 0 {
                continue;
            }
            let p = libc::mmap(
                hint as *mut c_void,
                page,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if p != libc::MAP_FAILED {
                if (p as i64 - near as i64).abs() < BRANCH_RANGE {
                    return Some(p as *mut u32);
                }
                libc::munmap(p, page); // landed too far; keep looking
            }
        }
        step = if step == 0 { page } else { step * 2 };
    }
    None
}

/// Build `if (x0 == 0) return 0; else goto original_target;`
unsafe fn write_stub(stub: *mut u32, original_target: usize) -> bool {
    let tail_pc = stub as usize + 4;
    let branch = match encode_branch(tail_pc, original_target) {
        Some(b) => b,
        None => return false,
    };

    stub.write(CBZ_X0_PLUS8); // +0 : if method == NULL jump to +8
    stub.add(1).write(branch); // +4 : tail-call the real implementation
    stub.add(2).write(MOV_W0_0); // +8 : return 0 (also a NULL pointer return)
    stub.add(3).write(RET); // +12

    flush_icache(stub as usize, 16);
    true
}

// ---------------------------------------------------------------------------------------------
// Fallback path for exports that are real functions rather than 4-byte thunks
// ---------------------------------------------------------------------------------------------

macro_rules! guard_detour {
    ($name:ident, $lock:expr) => {
        extern "C" fn $name(method: *mut c_void) -> u64 {
            if method.is_null() {
                return 0;
            }
            match $lock.try_read() {
                Ok(trampoline) => trampoline(method),
                Err(_) => 0,
            }
        }
    };
}

guard_detour!(param_count_detour, PARAM_COUNT_HOOK);
guard_detour!(param_detour, PARAM_HOOK);

// ---------------------------------------------------------------------------------------------

fn guard_export(
    name: &str,
    slot: &RwLock<NativeHook<ParamCountFn>>,
    detour: extern "C" fn(*mut c_void) -> u64,
) -> Result<(), DynErr> {
    let runtime = runtime!()?;

    let target = match runtime.get_export_ptr(name) {
        Ok(ptr) => ptr as usize,
        Err(_) => {
            debug!("{} not exported; nothing to guard", name)?;
            return Ok(());
        }
    };

    let first = unsafe { (target as *const u32).read() };

    if is_uncond_branch(first) {
        // Tail-call thunk: the entire function is this one branch, so replacing those exact 4 bytes
        // cannot touch anything else.
        let real_impl = branch_target(target, first);

        let stub = match unsafe { alloc_stub_near(target) } {
            Some(s) => s,
            None => {
                log!("[guard] {}: no stub memory within branch range; left unguarded", name);
                return Ok(());
            }
        };

        if !unsafe { write_stub(stub, real_impl) } {
            log!("[guard] {}: stub could not reach the implementation; left unguarded", name);
            return Ok(());
        }

        let patched = match encode_branch(target, stub as usize) {
            Some(b) => b,
            None => {
                log!("[guard] {}: stub out of branch range; left unguarded", name);
                return Ok(());
            }
        };

        let wrote = unsafe {
            with_writable(target, 4, || {
                (target as *mut u32).write(patched);
                flush_icache(target, 4);
            })
        };

        match wrote {
            Some(()) => log!(
                "[guard] {} is a tail-call thunk; redirected 4 bytes to a null-checking stub",
                name
            ),
            None => log!("[guard] {}: could not make the page writable; left unguarded", name),
        }
    } else {
        // Real function with a prologue - a conventional inline hook has room here.
        match slot.try_write() {
            Ok(mut hook) => {
                *hook = NativeHook::<ParamCountFn>::new(target as *mut c_void, detour as *mut c_void);
                match hook.hook() {
                    Ok(()) => log!("[guard] {} inline-hooked with a null check", name),
                    Err(e) => log!("[guard] {}: hook failed ({e}); left unguarded", name),
                }
            }
            Err(_) => log!("[guard] {}: hook slot busy; left unguarded", name),
        }
    }

    Ok(())
}

/// Guard the parameter APIs against the NULL MethodInfo Unity hands them for stripped modules.
pub fn hook() -> Result<(), DynErr> {
    guard_export("il2cpp_method_get_param_count", &PARAM_COUNT_HOOK, param_count_detour)?;
    guard_export("il2cpp_method_get_param", &PARAM_HOOK, param_detour)?;
    Ok(())
}
