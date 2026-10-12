//! The freestanding runtime of a `no_std` plugin.
//!
//! A plugin that links neither `std` nor the C runtime is a few kilobytes
//! instead of a hundred. [`freestanding!`](crate::freestanding) installs what
//! such a plugin lacks:
//!
//! * a `#[global_allocator]` on the process heap ([`ProcessHeap`]),
//! * a `#[panic_handler]` that writes the panic to the loader log through the
//!   host table and then terminates the process ([`on_panic`]),
//! * the few C runtime functions the compiler calls (`memcpy`, `memmove`,
//!   `memset`, `memcmp`, `bcmp`, `strlen`, `__chkstk`, `_fltused`) and the DLL
//!   entry point, as assembly,
//! * the linker directive `/NODEFAULTLIB`, so the C runtime is not pulled in.
//!
//! The pieces that must be unique in the final binary are expanded in the
//! plugin crate (not here), and not in its test build, so the crate keeps
//! working with a normal `cargo test`.

use core::sync::atomic::{AtomicPtr, Ordering};

use crate::abi::{ParsecOrbitHost, LOG_ERROR};
use crate::text::StackBuf;

static HOST: AtomicPtr<ParsecOrbitHost> = AtomicPtr::new(core::ptr::null_mut());

/// Remembers the host table so the panic handler can reach the log.
pub(crate) fn remember_host(host: &'static ParsecOrbitHost) {
    HOST.store(
        host as *const ParsecOrbitHost as *mut ParsecOrbitHost,
        Ordering::Release,
    );
}

/// The host table remembered by `init`, if any.
fn host() -> Option<&'static ParsecOrbitHost> {
    // Safety: only ever set from a `&'static ParsecOrbitHost`.
    unsafe { HOST.load(Ordering::Acquire).as_ref() }
}

/// Reports a panic through the host log and terminates the process.
///
/// Plugins are native code inside the client; there is no safe way to
/// continue after a panic, so the SDK logs it and stops.
pub fn on_panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(host) = host() {
        let mut buffer = StackBuf::<1024>::new();
        let _ = match info.location() {
            Some(at) => core::fmt::Write::write_fmt(
                &mut buffer,
                format_args!(
                    "panic: {} at {}:{}:{}",
                    info.message(),
                    at.file(),
                    at.line(),
                    at.column()
                ),
            ),
            None => {
                core::fmt::Write::write_fmt(&mut buffer, format_args!("panic: {}", info.message()))
            }
        };
        host.log_with_target(LOG_ERROR, host.plugin_id().unwrap_or(""), buffer.as_str());
    }
    abort()
}

/// Terminates the process immediately (a fast-fail, like `abort`).
pub fn abort() -> ! {
    #[cfg(all(target_arch = "x86_64", windows))]
    // Safety: `int 0x29` with ecx = FAST_FAIL_FATAL_APP_EXIT never returns.
    unsafe {
        core::arch::asm!("int 0x29", in("ecx") 7u32, options(noreturn, nostack))
    }
    #[cfg(not(all(target_arch = "x86_64", windows)))]
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(windows)]
mod heap {
    use core::alloc::{GlobalAlloc, Layout};
    use core::ffi::c_void;

    #[link(name = "kernel32", kind = "raw-dylib")]
    extern "system" {
        fn GetProcessHeap() -> *mut c_void;
        fn HeapAlloc(heap: *mut c_void, flags: u32, bytes: usize) -> *mut c_void;
        fn HeapReAlloc(
            heap: *mut c_void,
            flags: u32,
            memory: *mut c_void,
            bytes: usize,
        ) -> *mut c_void;
        fn HeapFree(heap: *mut c_void, flags: u32, memory: *mut c_void) -> i32;
    }

    const HEAP_ZERO_MEMORY: u32 = 0x08;
    /// What `HeapAlloc` guarantees on x64.
    const NATURAL_ALIGN: usize = 16;

    /// The allocator of a `no_std` plugin: the process heap, which is also
    /// what the C runtime's `malloc` sits on.
    pub struct ProcessHeap;

    impl ProcessHeap {
        unsafe fn allocate(layout: Layout, flags: u32) -> *mut u8 {
            let heap = unsafe { GetProcessHeap() };
            if layout.align() <= NATURAL_ALIGN {
                return unsafe { HeapAlloc(heap, flags, layout.size()) } as *mut u8;
            }
            // Over-aligned: allocate extra and keep the real pointer just
            // below the aligned one.
            let extra = layout.align() + core::mem::size_of::<usize>();
            let raw = unsafe { HeapAlloc(heap, flags, layout.size() + extra) } as *mut u8;
            if raw.is_null() {
                return raw;
            }
            let start = raw as usize + core::mem::size_of::<usize>();
            let aligned = (start + layout.align() - 1) & !(layout.align() - 1);
            unsafe { *((aligned - core::mem::size_of::<usize>()) as *mut usize) = raw as usize };
            aligned as *mut u8
        }
    }

    unsafe impl GlobalAlloc for ProcessHeap {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            unsafe { Self::allocate(layout, 0) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            unsafe { Self::allocate(layout, HEAP_ZERO_MEMORY) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            let heap = unsafe { GetProcessHeap() };
            let raw = if layout.align() <= NATURAL_ALIGN {
                pointer
            } else {
                let stored = (pointer as usize - core::mem::size_of::<usize>()) as *const usize;
                unsafe { *stored as *mut u8 }
            };
            unsafe { HeapFree(heap, 0, raw as *mut c_void) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if layout.align() <= NATURAL_ALIGN {
                let grown =
                    unsafe { HeapReAlloc(GetProcessHeap(), 0, pointer as *mut c_void, new_size) };
                return grown as *mut u8;
            }
            let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
            let fresh = unsafe { self.alloc(new_layout) };
            if !fresh.is_null() {
                unsafe {
                    core::ptr::copy_nonoverlapping(pointer, fresh, layout.size().min(new_size));
                    self.dealloc(pointer, layout);
                }
            }
            fresh
        }
    }
}

#[cfg(windows)]
pub use heap::ProcessHeap;

/// Installs the freestanding runtime in a `#![no_std]` plugin crate.
///
/// Call it once, at the crate root, next to
/// [`declare_plugin!`](crate::declare_plugin). The plugin crate must be
/// built with `panic = "abort"` in every profile it builds with (a `no_std`
/// library cannot unwind), for example in the workspace manifest:
///
/// ```toml
/// [profile.dev]
/// panic = "abort"
/// [profile.release]
/// panic = "abort"
/// ```
///
/// Nothing is emitted in the crate's own test build, which links `std`. Not
/// for plugins that use `std`: they bring their own runtime.
///
/// Only `x86_64-pc-windows-msvc` is supported.
#[macro_export]
macro_rules! freestanding {
    () => {
        #[cfg(not(test))]
        #[global_allocator]
        static __PARSEC_ORBIT_ALLOCATOR: $crate::runtime::ProcessHeap =
            $crate::runtime::ProcessHeap;

        #[cfg(not(test))]
        #[panic_handler]
        fn __parsec_orbit_panic(info: &::core::panic::PanicInfo<'_>) -> ! {
            $crate::runtime::on_panic(info)
        }

        #[cfg(not(test))]
        $crate::__runtime_support!();
    };
}

/// The C runtime stand-ins and linker directive; see [`freestanding!`].
#[doc(hidden)]
#[macro_export]
macro_rules! __runtime_support {
    () => {
        // The pieces of the C runtime that compiled code calls into. They
        // are assembly rather than Rust so that they are not exported from
        // the DLL and cannot be turned back into calls to themselves by
        // the optimiser.
        ::core::arch::global_asm!(
            ".text",
            // void *memcpy(void *dst, const void *src, size_t n)
            ".globl memcpy",
            "memcpy:",
            "    mov rax, rcx",
            "    push rdi",
            "    push rsi",
            "    mov rdi, rcx",
            "    mov rsi, rdx",
            "    mov rcx, r8",
            "    rep movsb",
            "    pop rsi",
            "    pop rdi",
            "    ret",
            // void *memmove(void *dst, const void *src, size_t n)
            ".globl memmove",
            "memmove:",
            "    mov rax, rcx",
            "    push rdi",
            "    push rsi",
            "    mov rdi, rcx",
            "    mov rsi, rdx",
            "    mov rcx, r8",
            "    cmp rdi, rsi",
            "    jbe 4f",
            "    lea r9, [rsi + rcx]",
            "    cmp rdi, r9",
            "    jae 4f",
            "    lea rdi, [rdi + rcx - 1]",
            "    lea rsi, [rsi + rcx - 1]",
            "    std",
            "    rep movsb",
            "    cld",
            "    jmp 5f",
            "4:",
            "    rep movsb",
            "5:",
            "    pop rsi",
            "    pop rdi",
            "    ret",
            // void *memset(void *dst, int byte, size_t n)
            ".globl memset",
            "memset:",
            "    mov r9, rcx",
            "    push rdi",
            "    mov rdi, rcx",
            "    mov eax, edx",
            "    mov rcx, r8",
            "    rep stosb",
            "    pop rdi",
            "    mov rax, r9",
            "    ret",
            // int memcmp(const void *a, const void *b, size_t n), and bcmp
            ".globl memcmp",
            ".globl bcmp",
            "memcmp:",
            "bcmp:",
            "    push rdi",
            "    push rsi",
            "    mov rsi, rcx",
            "    mov rdi, rdx",
            "    mov rcx, r8",
            "    xor eax, eax",
            "    test rcx, rcx",
            "    jz 6f",
            "    repe cmpsb",
            "    je 6f",
            "    movzx eax, byte ptr [rsi - 1]",
            "    movzx ecx, byte ptr [rdi - 1]",
            "    sub eax, ecx",
            "6:",
            "    pop rsi",
            "    pop rdi",
            "    ret",
            // size_t strlen(const char *s)
            ".globl strlen",
            "strlen:",
            "    push rdi",
            "    mov rdi, rcx",
            "    xor eax, eax",
            "    mov rcx, -1",
            "    repne scasb",
            "    not rcx",
            "    lea rax, [rcx - 1]",
            "    pop rdi",
            "    ret",
            // Stack probe for frames larger than a page; rax = frame size.
            ".globl __chkstk",
            "__chkstk:",
            "    push rcx",
            "    push rax",
            "    cmp rax, 0x1000",
            "    lea rcx, [rsp + 24]",
            "    jb 3f",
            "2:",
            "    sub rcx, 0x1000",
            "    test [rcx], rcx",
            "    sub rax, 0x1000",
            "    cmp rax, 0x1000",
            "    ja 2b",
            "3:",
            "    sub rcx, rax",
            "    test [rcx], rcx",
            "    pop rax",
            "    pop rcx",
            "    ret",
            // The linker's default DLL entry point: nothing to initialise.
            ".globl _DllMainCRTStartup",
            "_DllMainCRTStartup:",
            "    mov eax, 1",
            "    ret",
            // The precompiled `core` of an unoptimised build still names the
            // C++ exception personality. Nothing unwinds in a plugin that
            // aborts on panic, so this is never called.
            ".globl __CxxFrameHandler3",
            "__CxxFrameHandler3:",
            "    mov eax, 1",
            "    ret",
            // Marks that floating point is used (the CRT normally has it).
            ".data",
            ".globl _fltused",
            "_fltused:",
            "    .long 0",
        );

        // Leave the C runtime out of the link.
        #[used]
        #[link_section = ".drectve"]
        static __PARSEC_ORBIT_DIRECTIVES: [u8; 15] = *b" /NODEFAULTLIB ";
    };
}
