//! Signal handler that dumps registers and an attributed backtrace, then chains to the previous
//! handler.
//!
//! Why this exists: CoreCLR installs its own SIGSEGV handler and "handles" the fault, printing only a
//! raw instruction pointer and then exiting. debuggerd therefore never writes a tombstone, so every
//! crash so far has given us an ASLR-shifted address with no library attribution and no stack at all.
//! This handler runs BEFORE CoreCLR's (it is installed last, after the runtime is up) and records the
//! things that actually identify a crash: fault address, pc/lr/sp/fp, all general registers, and an
//! unwound backtrace with each frame resolved to `library.so+0xoffset`. Then it chains to CoreCLR so
//! the existing behaviour is unchanged.
//!
//! Signal-handler discipline: no allocation, no locks, no Rust formatting machinery on the crash path.
//! The log fd is opened once up front and everything is rendered into a fixed stack buffer and written
//! with raw `write(2)`. /proc/self/maps is read with raw open/read into a static buffer, which is why
//! attribution works even though we cannot allocate.

use std::{
    ffi::c_void,
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

const SIGNALS: [i32; 5] = [
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
    libc::SIGABRT,
];

static LOG_FD: AtomicI32 = AtomicI32::new(-1);
static IN_HANDLER: AtomicBool = AtomicBool::new(false);

static mut OLD_ACTIONS: [Option<libc::sigaction>; 32] = [None; 32];

// Big enough for a full maps table; read fresh on each crash so it is always accurate.
const MAPS_BUF_LEN: usize = 512 * 1024;
static mut MAPS_BUF: [u8; MAPS_BUF_LEN] = [0; MAPS_BUF_LEN];

// ---------------------------------------------------------------------------------------------
// raw, allocation-free output helpers
// ---------------------------------------------------------------------------------------------

struct Out {
    buf: [u8; 4096],
    len: usize,
}

impl Out {
    fn new() -> Self {
        Out { buf: [0; 4096], len: 0 }
    }
    fn s(&mut self, text: &str) -> &mut Self {
        for &b in text.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        self
    }
    fn hex(&mut self, mut value: u64) -> &mut Self {
        self.s("0x");
        if value == 0 {
            return self.s("0");
        }
        let mut tmp = [0u8; 16];
        let mut n = 0;
        while value > 0 {
            let digit = (value & 0xf) as u8;
            tmp[n] = if digit < 10 { b'0' + digit } else { b'a' + digit - 10 };
            value >>= 4;
            n += 1;
        }
        while n > 0 {
            n -= 1;
            if self.len < self.buf.len() {
                self.buf[self.len] = tmp[n];
                self.len += 1;
            }
        }
        self
    }
    fn dec(&mut self, value: i64) -> &mut Self {
        if value < 0 {
            self.s("-");
            return self.dec(-value);
        }
        if value == 0 {
            return self.s("0");
        }
        let mut tmp = [0u8; 20];
        let mut n = 0;
        let mut v = value;
        while v > 0 {
            tmp[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
        }
        while n > 0 {
            n -= 1;
            if self.len < self.buf.len() {
                self.buf[self.len] = tmp[n];
                self.len += 1;
            }
        }
        self
    }
    fn flush(&mut self) {
        if self.len < self.buf.len() {
            self.buf[self.len] = b'\n';
            self.len += 1;
        }
        let fd = LOG_FD.load(Ordering::Relaxed);
        unsafe {
            if fd >= 0 {
                let _ = libc::write(fd, self.buf.as_ptr() as *const c_void, self.len);
            }
            // stderr is captured into logcat by dotnet_trace, so this reaches ADB.log too.
            let _ = libc::write(2, self.buf.as_ptr() as *const c_void, self.len);
        }
        self.len = 0;
    }
}

// ---------------------------------------------------------------------------------------------
// maps handling: attribute an address to library + offset without allocating
// ---------------------------------------------------------------------------------------------

fn read_maps() -> usize {
    unsafe {
        let path = b"/proc/self/maps\0";
        let fd = libc::open(path.as_ptr() as *const libc::c_char, libc::O_RDONLY);
        if fd < 0 {
            return 0;
        }
        let mut total = 0usize;
        loop {
            let want = MAPS_BUF_LEN - total;
            if want == 0 {
                break;
            }
            let n = libc::read(fd, MAPS_BUF.as_mut_ptr().add(total) as *mut c_void, want);
            if n <= 0 {
                break;
            }
            total += n as usize;
        }
        libc::close(fd);
        total
    }
}

fn parse_hex(bytes: &[u8], pos: &mut usize) -> u64 {
    let mut v: u64 = 0;
    while *pos < bytes.len() {
        let c = bytes[*pos];
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => break,
        };
        v = (v << 4) | d as u64;
        *pos += 1;
    }
    v
}

/// Find the mapping containing `addr` and write "lib.so+0xoff" into `out`.
fn attribute(addr: u64, maps_len: usize, out: &mut Out) {
    if addr == 0 {
        out.s(" <null>");
        return;
    }
    let buf = unsafe { &MAPS_BUF[..maps_len] };
    let mut i = 0usize;
    while i < buf.len() {
        let line_start = i;
        while i < buf.len() && buf[i] != b'\n' {
            i += 1;
        }
        let line = &buf[line_start..i];
        i += 1;

        let mut p = 0usize;
        let lo = parse_hex(line, &mut p);
        if p >= line.len() || line[p] != b'-' {
            continue;
        }
        p += 1;
        let hi = parse_hex(line, &mut p);
        if addr < lo || addr >= hi {
            continue;
        }
        // path is the last whitespace-separated field, if any
        let mut path_start = line.len();
        let mut seen_non_space = false;
        let mut j = line.len();
        while j > 0 {
            j -= 1;
            if line[j] == b' ' {
                if seen_non_space {
                    path_start = j + 1;
                    break;
                }
            } else {
                seen_non_space = true;
            }
        }
        if path_start < line.len() && line[path_start] == b'/' {
            // basename only, to keep lines readable
            let mut base = path_start;
            let mut k = path_start;
            while k < line.len() {
                if line[k] == b'/' {
                    base = k + 1;
                }
                k += 1;
            }
            out.s(" ");
            for &b in &line[base..] {
                if out.len < out.buf.len() {
                    out.buf[out.len] = b;
                    out.len += 1;
                }
            }
            out.s("+");
            out.hex(addr - lo);
        } else {
            out.s(" <anon ");
            out.hex(lo);
            out.s(">+");
            out.hex(addr - lo);
        }
        return;
    }
    out.s(" <unmapped>");
}

/// The kernel's aarch64 sigcontext. Declared here rather than using libc's `mcontext_t` because
/// bionic's `ucontext_t` reserves 128 bytes for the signal mask (`__u8 __unused[1024/8 - sizeof
/// (sigset_t)]`) that the libc crate's struct omits - reading through libc's layout lands 128 bytes
/// early, which produced a garbage dump (pc=0x1, sp=0x2c, registers mostly zero) on the first attempt.
#[repr(C)]
struct SigContext {
    fault_address: u64,
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
}

/// Locate the sigcontext inside the ucontext without trusting any single libc layout: scan for a
/// candidate whose fault_address matches siginfo's si_addr AND whose pc lands in an executable
/// mapping. Self-correcting, and it logs the offset it settled on.
fn find_sigcontext(ctx: *mut c_void, fault: u64, maps_len: usize, o: &mut Out) -> *const SigContext {
    let base = ctx as *const u8;
    for off in (0..1024usize).step_by(8) {
        let cand = unsafe { base.add(off) } as *const SigContext;
        let fa = unsafe { (*cand).fault_address };
        let pc = unsafe { (*cand).pc };
        if fa == fault && executable(pc, maps_len) {
            o.s("sigcontext found at ucontext+").hex(off as u64).flush();
            return cand;
        }
    }
    // Fall back to the documented layout even if validation failed (e.g. a null fault address).
    for off in (0..1024usize).step_by(8) {
        let cand = unsafe { base.add(off) } as *const SigContext;
        if executable(unsafe { (*cand).pc }, maps_len) {
            o.s("sigcontext guessed at ucontext+").hex(off as u64).flush();
            return cand;
        }
    }
    o.s("could not locate sigcontext").flush();
    std::ptr::null()
}

/// Is `addr` inside an executable mapping? Used to validate a candidate pc.
fn executable(addr: u64, maps_len: usize) -> bool {
    if addr == 0 {
        return false;
    }
    let buf = unsafe { &MAPS_BUF[..maps_len] };
    let mut i = 0usize;
    while i < buf.len() {
        let line_start = i;
        while i < buf.len() && buf[i] != b'\n' {
            i += 1;
        }
        let line = &buf[line_start..i];
        i += 1;
        let mut p = 0usize;
        let lo = parse_hex(line, &mut p);
        if p >= line.len() || line[p] != b'-' {
            continue;
        }
        p += 1;
        let hi = parse_hex(line, &mut p);
        if addr >= lo && addr < hi {
            // perms field follows one space: "r-xp"
            return p + 3 < line.len() && line[p + 3] == b'x';
        }
    }
    false
}

/// Is `addr` inside any readable mapping? Used to avoid faulting again while unwinding.
fn readable(addr: u64, maps_len: usize) -> bool {
    if addr == 0 || addr & 7 != 0 {
        return false;
    }
    let buf = unsafe { &MAPS_BUF[..maps_len] };
    let mut i = 0usize;
    while i < buf.len() {
        let line_start = i;
        while i < buf.len() && buf[i] != b'\n' {
            i += 1;
        }
        let line = &buf[line_start..i];
        i += 1;
        let mut p = 0usize;
        let lo = parse_hex(line, &mut p);
        if p >= line.len() || line[p] != b'-' {
            continue;
        }
        p += 1;
        let hi = parse_hex(line, &mut p);
        if addr >= lo && addr + 8 <= hi {
            // perms follow a space
            if p + 1 < line.len() && line[p + 1] == b'r' {
                return true;
            }
            return false;
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// the handler
// ---------------------------------------------------------------------------------------------

extern "C" fn handler(sig: i32, info: *mut libc::siginfo_t, ctx: *mut c_void) {
    // Guard against faulting inside our own handler.
    if IN_HANDLER.swap(true, Ordering::SeqCst) {
        chain(sig, info, ctx);
        return;
    }

    let maps_len = read_maps();
    let mut o = Out::new();

    o.s("").flush();
    o.s("======== LimeLoader crash handler ========").flush();
    o.s("signal ").dec(sig as i64).s("  tid ").dec(unsafe { libc::gettid() } as i64).flush();

    let fault = unsafe {
        if info.is_null() { 0 } else { (*info).si_addr() as u64 }
    };
    o.s("fault address ").hex(fault).flush();

    let sc = if ctx.is_null() {
        std::ptr::null()
    } else {
        find_sigcontext(ctx, fault, maps_len, &mut o)
    };

    if !sc.is_null() {
        let mc = unsafe { &*sc };
        let pc = mc.pc;
        let sp = mc.sp;
        let lr = mc.regs[30];
        let fp = mc.regs[29];

        o.s("pc ").hex(pc);
        attribute(pc, maps_len, &mut o);
        o.flush();

        o.s("lr ").hex(lr);
        attribute(lr, maps_len, &mut o);
        o.flush();

        o.s("sp ").hex(sp).s("  fp ").hex(fp).flush();

        // general registers - x0 matters most here: a null x0 is the whole story for this class of bug,
        // and x8/x16/x17 typically hold the target of an indirect `blr`, i.e. who was being called.
        for r in 0..31usize {
            o.s("x").dec(r as i64).s(" ").hex(mc.regs[r]);
            attribute(mc.regs[r], maps_len, &mut o);
            o.flush();
        }

        // frame-pointer unwind: [fp] = caller fp, [fp+8] = caller lr
        o.s("--- backtrace ---").flush();
        o.s("#0  ").hex(pc);
        attribute(pc, maps_len, &mut o);
        o.flush();
        o.s("#1  ").hex(lr);
        attribute(lr, maps_len, &mut o);
        o.flush();

        let mut frame = fp;
        let mut n = 2;
        while n < 40 && readable(frame, maps_len) && readable(frame + 8, maps_len) {
            let next_fp = unsafe { *(frame as *const u64) };
            let ret = unsafe { *((frame + 8) as *const u64) };
            if ret == 0 {
                break;
            }
            o.s("#").dec(n).s("  ").hex(ret);
            attribute(ret, maps_len, &mut o);
            o.flush();
            if next_fp <= frame {
                break; // stack must grow downward; anything else means we lost the chain
            }
            frame = next_fp;
            n += 1;
        }
        o.s("--- end backtrace ---").flush();
    } else {
        o.s("no usable sigcontext - raw ucontext words follow").flush();
        if !ctx.is_null() {
            for off in (0..64usize).step_by(8) {
                let v = unsafe { *((ctx as *const u8).add(off) as *const u64) };
                o.s("ucontext+").hex(off as u64).s(" ").hex(v);
                attribute(v, maps_len, &mut o);
                o.flush();
            }
        }
    }

    o.s("======== end crash handler ========").flush();

    IN_HANDLER.store(false, Ordering::SeqCst);

    // Hand over to whoever was installed before us (CoreCLR), preserving existing behaviour.
    chain(sig, info, ctx);
}

fn chain(sig: i32, info: *mut libc::siginfo_t, ctx: *mut c_void) {
    unsafe {
        let idx = sig as usize;
        if idx >= 32 {
            return;
        }
        let old = match OLD_ACTIONS[idx] {
            Some(a) => a,
            None => return,
        };
        if old.sa_sigaction == libc::SIG_DFL || old.sa_sigaction == libc::SIG_IGN {
            // restore default and re-raise so debuggerd produces a real tombstone
            let mut dfl: libc::sigaction = std::mem::zeroed();
            dfl.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &dfl, null_mut());
            libc::raise(sig);
            return;
        }
        if old.sa_flags & libc::SA_SIGINFO != 0 {
            let f: extern "C" fn(i32, *mut libc::siginfo_t, *mut c_void) =
                std::mem::transmute(old.sa_sigaction);
            f(sig, info, ctx);
        } else {
            let f: extern "C" fn(i32) = std::mem::transmute(old.sa_sigaction);
            f(sig);
        }
    }
}

/// Install (or re-install) the handler. Call again after CoreCLR starts so ours takes precedence.
pub fn install() {
    // Open the log fd once; the handler must never allocate or take a lock.
    if LOG_FD.load(Ordering::Relaxed) < 0 {
        let path = b"/sdcard/MelonLoader/debug.log\0";
        let fd = unsafe {
            libc::open(
                path.as_ptr() as *const libc::c_char,
                libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                0o644,
            )
        };
        LOG_FD.store(fd, Ordering::Relaxed);
    }

    unsafe {
        for &sig in SIGNALS.iter() {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = handler as usize;
            act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESTART;
            libc::sigemptyset(&mut act.sa_mask);

            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, &act, &mut old) == 0 {
                // Keep only the FIRST previous handler, so re-installing after CoreCLR does not make
                // us chain to ourselves.
                let idx = sig as usize;
                if idx < 32 && OLD_ACTIONS[idx].is_none() {
                    OLD_ACTIONS[idx] = Some(old);
                }
            }
        }
    }

    super::write_line("[diag] crash handler installed");
}
