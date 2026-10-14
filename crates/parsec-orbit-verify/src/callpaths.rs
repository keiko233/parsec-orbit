//! Forwarding check: four ways of calling `BCryptGenRandom` must agree.
//!
//!   1. direct call to the real `%SystemRoot%\System32\bcrypt.dll`
//!   2. a local `jmp [rip+slot]` thunk
//!   3. a local copy of the loader's stub shape
//!   4. the loader's own export
//!
//! The 4th argument (the flags word) is the interesting one: the forwarder
//! saves and restores the argument registers, and a mistake there shows up as
//! `STATUS_INVALID_HANDLE` from call 4 while calls 1-3 succeed. Build the
//! loader with `--features diagnostics` to also print the register values the
//! real function received.
//!
//! Usage: callpaths <path to loader bcrypt.dll>

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetProcAddress, LoadLibraryExW,
};

const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

type GenRandom = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;

static SLOT: AtomicUsize = AtomicUsize::new(0);
static REPLICA_SLOT: AtomicUsize = AtomicUsize::new(0);

#[unsafe(naked)]
unsafe extern "system" fn jump_through_slot() -> usize {
    core::arch::naked_asm!("jmp [rip + {slot}]", slot = sym SLOT)
}

unsafe extern "system" fn replica_resolver(_slot: usize) -> usize {
    REPLICA_SLOT.load(Ordering::SeqCst)
}

/// Same shape as the loader's `resolve_stub`.
#[unsafe(naked)]
unsafe extern "system" fn save_and_jump() -> usize {
    core::arch::naked_asm!(
        "push rcx",
        "push rdx",
        "push r8",
        "push r9",
        "sub rsp, 0x28",
        "mov rcx, rsp",
        "call {resolve}",
        "add rsp, 0x28",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rcx",
        "jmp rax",
        resolve = sym replica_resolver,
    )
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn module_path(module: *mut c_void) -> String {
    let mut buffer = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    String::from_utf16_lossy(&buffer[..len as usize])
}

fn run(function: GenRandom, label: &str) -> bool {
    let mut buffer = [0u8; 16];
    let status = unsafe {
        function(
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    println!(
        "{label}: status=0x{status:08X} first bytes {:02X?}",
        &buffer[..4]
    );
    status == 0
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: callpaths <path to loader bcrypt.dll>");
        std::process::exit(2);
    });
    let name = wide(&path);
    let loader = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        )
    };
    if loader.is_null() {
        eprintln!("LoadLibraryExW failed: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }
    println!("loader mapped at 0x{:X}", loader as usize);

    let real_path = wide("C:\\Windows\\System32\\bcrypt.dll");
    let real = unsafe {
        LoadLibraryExW(
            real_path.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if real.is_null() {
        eprintln!("the real bcrypt.dll could not be loaded");
        std::process::exit(1);
    }
    let symbol = b"BCryptGenRandom\0";
    let real_random = unsafe { GetProcAddress(real, symbol.as_ptr()) }.expect("real export");
    println!(
        "real bcrypt.dll at 0x{:X} ({})",
        real as usize,
        module_path(real)
    );

    let mut failures = 0;
    let direct: GenRandom = unsafe { std::mem::transmute(real_random) };
    if !run(direct, "1 direct            ") {
        failures += 1;
    }

    SLOT.store(real_random as usize, Ordering::SeqCst);
    let jump: GenRandom =
        unsafe { std::mem::transmute(jump_through_slot as *const () as *const c_void) };
    if !run(jump, "2 jmp thunk         ") {
        failures += 1;
    }

    REPLICA_SLOT.store(real_random as usize, Ordering::SeqCst);
    let replica: GenRandom =
        unsafe { std::mem::transmute(save_and_jump as *const () as *const c_void) };
    if !run(replica, "3 stub replica      ") {
        failures += 1;
    }

    let forwarded = unsafe { GetProcAddress(loader, symbol.as_ptr()) }.expect("loader export");
    let forwarded: GenRandom = unsafe { std::mem::transmute(forwarded) };
    if !run(forwarded, "4 through the loader") {
        failures += 1;
    }

    match unsafe { GetProcAddress(loader, b"ParsecOrbitLoaderLastArgs\0".as_ptr()) } {
        Some(getter) => {
            let getter: unsafe extern "system" fn() -> *const AtomicUsize =
                unsafe { std::mem::transmute(getter) };
            let args = unsafe { getter() };
            let values: Vec<usize> = (0..4)
                .map(|index| unsafe { (*args.add(index)).load(Ordering::SeqCst) })
                .collect();
            println!(
                "arguments seen by the real function: rcx=0x{:X} rdx=0x{:X} r8=0x{:X} r9=0x{:X}",
                values[0], values[1], values[2], values[3]
            );
            if values[3] != BCRYPT_USE_SYSTEM_PREFERRED_RNG as usize {
                println!("FAIL the fourth argument was not forwarded unchanged");
                failures += 1;
            }
        }
        None => println!(
            "note: this loader was built without --features diagnostics, argument recording is not exposed"
        ),
    }

    // Give the loader's worker thread time to log its own progress.
    std::thread::sleep(std::time::Duration::from_millis(1200));

    if failures == 0 {
        println!("all call paths returned STATUS_SUCCESS");
    } else {
        println!("{failures} call path(s) failed");
        std::process::exit(1);
    }
}
