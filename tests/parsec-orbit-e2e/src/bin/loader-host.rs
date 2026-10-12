//! Stand-in for the Parsec client process in the end-to-end tests.
//!
//! Usage: `loader-host <loader dll> <log file> <text to wait for> <seconds> [thunks]`
//!
//! It maps the loader with `LoadLibraryW` (what the Windows loader does for the
//! client's static import of `bcrypt.dll`), waits until the loader's log file
//! contains the given text, and then prints what the tests want to know, one
//! `key=value` line each:
//!
//! ```text
//! waited=found|timeout
//! webview2_args=<value or "<unset>">
//! genrandom=<NTSTATUS in hex>    BCryptGenRandom called through the loader
//! thunk=<name> <slot address> <jump target>      (only with `thunks`)
//! ```
//!
//! The exit code is 0 unless the loader could not be mapped (2) or the wait
//! timed out (3).

use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

type GenRandom = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "usage: loader-host <loader dll> <log file> <text to wait for> <seconds> [thunks]"
        );
        std::process::exit(64);
    }
    let (dll, log, needle) = (&args[1], &args[2], &args[3]);
    let seconds: u64 = args[4].parse().expect("seconds");
    let thunks = args.get(5).is_some_and(|flag| flag == "thunks");

    let module = unsafe { LoadLibraryW(wide(dll).as_ptr()) };
    if module.is_null() {
        eprintln!("could not map {dll}: {}", std::io::Error::last_os_error());
        std::process::exit(2);
    }

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut found = false;
    while Instant::now() < deadline {
        if std::fs::read_to_string(log).is_ok_and(|content| content.contains(needle.as_str())) {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    println!("waited={}", if found { "found" } else { "timeout" });

    println!(
        "webview2_args={}",
        std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_else(|_| "<unset>".into())
    );

    // A real forwarded call: 4 register arguments, the last one a flag.
    match unsafe { GetProcAddress(module, c"BCryptGenRandom".as_ptr().cast()) } {
        Some(function) => {
            let function: GenRandom = unsafe { std::mem::transmute(function) };
            let mut buffer = [0u8; 32];
            let status = unsafe { function(std::ptr::null_mut(), buffer.as_mut_ptr(), 32, 2) };
            println!("genrandom={status:x}");
        }
        None => println!("genrandom=missing"),
    }

    if thunks {
        print_thunks(module as usize, &args[1]);
    }

    std::process::exit(if found { 0 } else { 3 });
}

/// Decodes the two instructions of every exported thunk:
/// `lea rax, [rip + slot]` (`48 8D 05 disp32`) and `jmp stub` (`E9 rel32`).
fn print_thunks(module: usize, path: &str) {
    for name in parsec_orbit_e2e::export_names(std::path::Path::new(path)) {
        let symbol = format!("{name}\0");
        let Some(address) = (unsafe { GetProcAddress(module as *mut c_void, symbol.as_ptr()) })
        else {
            println!("thunk={name} missing");
            continue;
        };
        let address = address as usize;
        let code = unsafe { std::slice::from_raw_parts(address as *const u8, 12) };
        if code[..3] != [0x48, 0x8D, 0x05] || code[7] != 0xE9 {
            println!("thunk={name} unexpected {code:02x?}");
            continue;
        }
        let displacement = i32::from_le_bytes(code[3..7].try_into().unwrap()) as i64;
        let relative = i32::from_le_bytes(code[8..12].try_into().unwrap()) as i64;
        let slot = (address as i64 + 7 + displacement) as usize;
        let target = (address as i64 + 12 + relative) as usize;
        println!("thunk={name} {slot:x} {target:x}");
    }
}
