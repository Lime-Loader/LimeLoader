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

/// `MAP_FIXED_NOREPLACE`: map exactly here, or fail - never silently relocate. Spelled out rather than
/// taken from libc so the build does not depend on the crate exposing it.
const MAP_FIXED_NOREPLACE: libc::c_int = 0x10_0000;

/// Allocate an executable page within branch range of `near`, so a single `B` can reach it.
///
/// A plain mmap hint is only advisory - under ASLR the kernel happily places the page hundreds of
/// megabytes away, which is why the first version of this always reported "no stub memory within
/// branch range". Instead, read the address space, find a real hole near the target, and claim it
/// with MAP_FIXED_NOREPLACE so we either get that exact address or a clean failure.
unsafe fn alloc_stub_near(near: usize) -> Option<*mut u32> {
    let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;

    // Stay a little inside the +/-128MB limit so the branch is comfortably in range.
    let reach = (BRANCH_RANGE as usize) - 1024 * 1024;
    let lo = near.saturating_sub(reach);
    let hi = near.saturating_add(reach);

    // Existing mappings, sorted; gaps between them are fair game.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    if let Ok(maps) = std::fs::read_to_string("/proc/self/maps") {
        for line in maps.lines() {
            if let Some(dash) = line.find('-') {
                let end_of_range = line.find(' ').unwrap_or(line.len());
                if let (Ok(s), Ok(e)) = (
                    usize::from_str_radix(&line[..dash], 16),
                    usize::from_str_radix(&line[dash + 1..end_of_range], 16),
                ) {
                    ranges.push((s, e));
                }
            }
        }
    }
    ranges.sort_unstable();

    // Try holes closest to the target first, working outwards in both directions.
    let mut candidates: Vec<usize> = Vec::new();
    for pair in ranges.windows(2) {
        let gap_start = (pair[0].1 + page - 1) & !(page - 1);
        let gap_end = pair[1].0;
        if gap_end <= gap_start || gap_end - gap_start < page {
            continue;
        }
        // Clamp the usable part of this hole to our reachable window.
        let usable_start = gap_start.max(lo);
        let usable_end = gap_end.min(hi);
        if usable_end <= usable_start || usable_end - usable_start < page {
            continue;
        }
        candidates.push(usable_start);
        let last_page = (usable_end - page) & !(page - 1);
        if last_page > usable_start {
            candidates.push(last_page);
        }
    }
    candidates.sort_by_key(|&addr| (addr as i64 - near as i64).abs());

    for addr in candidates {
        let p = libc::mmap(
            addr as *mut c_void,
            page,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | MAP_FIXED_NOREPLACE,
            -1,
            0,
        );
        if p != libc::MAP_FAILED {
            if p as usize == addr && (p as i64 - near as i64).abs() < BRANCH_RANGE {
                return Some(p as *mut u32);
            }
            // Kernel ignored the flag (older kernel) and relocated us - give it back and continue.
            libc::munmap(p, page);
        }
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

/// Guard one export using ONLY the 4-byte thunk rewrite, never the Rust inline-hook fallback.
///
/// For multi-argument functions the fallback is unusable: its detour is declared as taking a single
/// argument, so calling the trampoline from Rust would not preserve x1-x7. The generated stub has no
/// such problem - it only tests x0 and otherwise branches with every register untouched, so it works
/// for any signature.
pub fn guard_thunk_only(name: &str) {
    let runtime = match runtime!() {
        Ok(r) => r,
        Err(_) => return,
    };
    let target = match runtime.get_export_ptr(name) {
        Ok(ptr) => ptr as usize,
        Err(_) => return,
    };
    let first = unsafe { (target as *const u32).read() };
    if !is_uncond_branch(first) {
        log!("[guard] {} is not a tail-call thunk; left unguarded (cannot wrap safely)", name);
        return;
    }
    let real_impl = branch_target(target, first);
    let stub = match unsafe { alloc_stub_near(target) } {
        Some(s) => s,
        None => {
            log!("[guard] {}: no free page within +/-128MB of {:#x}; left unguarded", name, target);
            return;
        }
    };
    if !unsafe { write_stub(stub, real_impl) } {
        log!("[guard] {}: stub could not reach the implementation; left unguarded", name);
        return;
    }
    let patched = match encode_branch(target, stub as usize) {
        Some(b) => b,
        None => {
            log!("[guard] {}: stub out of branch range; left unguarded", name);
            return;
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
            "[guard] {} thunk at {:#x} -> stub {:#x} (impl {:#x}); null check active",
            name, target, stub as usize, real_impl
        ),
        None => log!("[guard] {}: could not make the page writable; left unguarded", name),
    }
}

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
                log!(
                    "[guard] {}: no free page within +/-128MB of {:#x}; left unguarded",
                    name, target
                );
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
                "[guard] {} thunk at {:#x} -> stub {:#x} (impl {:#x}); null check active",
                name, target, stub as usize, real_impl
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

/// Declares a guard per export: a hook slot, a detour that short-circuits NULL, and the wiring.
///
/// Every one of these takes the MethodInfo* in x0, and for every one of them "0" is the right answer
/// for a null method - a zero count, a null pointer, or false. Guarding the whole family rather than
/// individual functions matters: Unity does not make one call with the bad method, it makes a run of
/// them, so patching only the first just moves the crash to the next accessor.
macro_rules! guards {
    ($(($detour:ident, $slot:ident, $export:literal)),* $(,)?) => {
        lazy_static! {
            $(
                // Each export needs its own slot: a detour cannot tell which function it was entered
                // for, so a shared slot would call the wrong trampoline.
                static ref $slot: RwLock<NativeHook<ParamCountFn>> =
                    RwLock::new(NativeHook::new(std::ptr::null_mut(), std::ptr::null_mut()));
            )*
        }
        $(
            extern "C" fn $detour(method: *mut c_void) -> u64 {
                if method.is_null() {
                    return 0;
                }
                match $slot.try_read() {
                    Ok(trampoline) => trampoline(method),
                    Err(_) => 0,
                }
            }
        )*
        /// Guard il2cpp's method APIs against the NULL MethodInfo Unity hands them for stripped modules.
        pub fn hook() -> Result<(), DynErr> {
            $( guard_export($export, &$slot, $detour)?; )*
            Ok(())
        }
    };
}

guards!(
    (d_param_count,     H_PARAM_COUNT,     "il2cpp_method_get_param_count"),
    (d_param,           H_PARAM,           "il2cpp_method_get_param"),
    (d_param_name,      H_PARAM_NAME,      "il2cpp_method_get_param_name"),
    (d_name,            H_NAME,            "il2cpp_method_get_name"),
    (d_class,           H_CLASS,           "il2cpp_method_get_class"),
    (d_declaring_type,  H_DECLARING_TYPE,  "il2cpp_method_get_declaring_type"),
    (d_return_type,     H_RETURN_TYPE,     "il2cpp_method_get_return_type"),
    (d_flags,           H_FLAGS,           "il2cpp_method_get_flags"),
    (d_token,           H_TOKEN,           "il2cpp_method_get_token"),
    (d_object,          H_OBJECT,          "il2cpp_method_get_object"),
    (d_has_attribute,   H_HAS_ATTRIBUTE,   "il2cpp_method_has_attribute"),
    (d_is_generic,      H_IS_GENERIC,      "il2cpp_method_is_generic"),
    (d_is_inflated,     H_IS_INFLATED,     "il2cpp_method_is_inflated"),
    (d_is_instance,     H_IS_INSTANCE,     "il2cpp_method_is_instance"),
    (d_from_reflection, H_FROM_REFLECTION, "il2cpp_method_get_from_reflection"),
);
