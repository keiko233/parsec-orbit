//! Transparency check: real cryptographic work done *through* the loader must
//! produce the same answers as the real `bcrypt.dll`.
//!
//! The vectors cover the argument shapes a forwarder can break: a fourth
//! argument flag (HMAC), stack-passed arguments (the hash secret) and pointer
//! outputs.
//!
//! Usage: roundtrip <path to loader bcrypt.dll>

use std::ffi::c_void;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW};

const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
const BCRYPT_ALG_HANDLE_HMAC_FLAG: u32 = 0x0000_0008;

const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const HMAC_EXPECTED: &str = "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8";

type OpenAlgorithm =
    unsafe extern "system" fn(*mut *mut c_void, *const u16, *const u16, u32) -> i32;
type CreateHash = unsafe extern "system" fn(
    *mut c_void,
    *mut *mut c_void,
    *mut u8,
    u32,
    *const u8,
    u32,
    u32,
) -> i32;
type HashData = unsafe extern "system" fn(*mut c_void, *const u8, u32, u32) -> i32;
type FinishHash = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;
type DestroyHash = unsafe extern "system" fn(*mut c_void) -> i32;
type CloseAlgorithm = unsafe extern "system" fn(*mut c_void, u32) -> i32;
type GenRandom = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;
type GetProperty =
    unsafe extern "system" fn(*mut c_void, *const u16, *mut u8, u32, *mut u32, u32) -> i32;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn resolve(module: *mut c_void, name: &str) -> *const c_void {
    let symbol = format!("{name}\0");
    match unsafe { GetProcAddress(module, symbol.as_ptr()) } {
        Some(function) => function as *const c_void,
        None => {
            eprintln!("{name} is not exported by the loader");
            std::process::exit(2);
        }
    }
}

/// Opens `algorithm` (as HMAC when a secret is given), hashes `message`.
unsafe fn digest(
    module: *mut c_void,
    algorithm: &str,
    message: &[u8],
    secret: Option<&[u8]>,
) -> Vec<u8> {
    let open: OpenAlgorithm =
        unsafe { std::mem::transmute(resolve(module, "BCryptOpenAlgorithmProvider")) };
    let create: CreateHash = unsafe { std::mem::transmute(resolve(module, "BCryptCreateHash")) };
    let update: HashData = unsafe { std::mem::transmute(resolve(module, "BCryptHashData")) };
    let finish: FinishHash = unsafe { std::mem::transmute(resolve(module, "BCryptFinishHash")) };
    let destroy: DestroyHash = unsafe { std::mem::transmute(resolve(module, "BCryptDestroyHash")) };
    let close: CloseAlgorithm =
        unsafe { std::mem::transmute(resolve(module, "BCryptCloseAlgorithmProvider")) };

    let algorithm_id = wide(algorithm);
    let mut handle: *mut c_void = std::ptr::null_mut();
    let flags = if secret.is_some() {
        BCRYPT_ALG_HANDLE_HMAC_FLAG
    } else {
        0
    };
    let status = unsafe { open(&mut handle, algorithm_id.as_ptr(), std::ptr::null(), flags) };
    assert!(status == 0, "BCryptOpenAlgorithmProvider -> 0x{status:08X}");

    let (secret_pointer, secret_length) = match secret {
        Some(bytes) => (bytes.as_ptr(), bytes.len() as u32),
        None => (std::ptr::null(), 0),
    };
    let mut hash: *mut c_void = std::ptr::null_mut();
    let status = unsafe {
        create(
            handle,
            &mut hash,
            std::ptr::null_mut(),
            0,
            secret_pointer,
            secret_length,
            0,
        )
    };
    assert!(status == 0, "BCryptCreateHash -> 0x{status:08X}");

    let status = unsafe { update(hash, message.as_ptr(), message.len() as u32, 0) };
    assert!(status == 0, "BCryptHashData -> 0x{status:08X}");

    let mut output = [0u8; 32];
    let status = unsafe { finish(hash, output.as_mut_ptr(), output.len() as u32, 0) };
    assert!(status == 0, "BCryptFinishHash -> 0x{status:08X}");

    unsafe {
        destroy(hash);
        close(handle, 0);
    }
    output.to_vec()
}

fn check(label: &str, ok: bool) -> bool {
    println!("{} {label}", if ok { "PASS" } else { "FAIL" });
    ok
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: roundtrip <path to loader bcrypt.dll>");
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

    let mut failures = 0;

    let gen_random: GenRandom = unsafe { std::mem::transmute(resolve(loader, "BCryptGenRandom")) };
    let mut random = [0u8; 32];
    let status = unsafe {
        gen_random(
            std::ptr::null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    let distinct = random.iter().any(|byte| *byte != 0);
    if !check(
        &format!("BCryptGenRandom(system preferred) -> 0x{status:08X}, non-zero: {distinct}"),
        status == 0 && distinct,
    ) {
        failures += 1;
    }

    let sha256 = unsafe { digest(loader, "SHA256", b"abc", None) };
    if !check(
        &format!("SHA256(\"abc\") = {}", hex(&sha256)),
        hex(&sha256) == SHA256_ABC,
    ) {
        failures += 1;
    }

    let message = b"The quick brown fox jumps over the lazy dog";
    let hmac = unsafe { digest(loader, "SHA256", message, Some(b"key")) };
    if !check(
        &format!("HMAC-SHA256(\"key\", fox) = {}", hex(&hmac)),
        hex(&hmac) == HMAC_EXPECTED,
    ) {
        failures += 1;
    }

    let open: OpenAlgorithm =
        unsafe { std::mem::transmute(resolve(loader, "BCryptOpenAlgorithmProvider")) };
    let get: GetProperty = unsafe { std::mem::transmute(resolve(loader, "BCryptGetProperty")) };
    let close: CloseAlgorithm =
        unsafe { std::mem::transmute(resolve(loader, "BCryptCloseAlgorithmProvider")) };
    let sha256_name = wide("SHA256");
    let object_length = wide("ObjectLength");
    let mut handle: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    let mut written = 0u32;
    let status_open = unsafe { open(&mut handle, sha256_name.as_ptr(), std::ptr::null(), 0) };
    let status_get = unsafe {
        get(
            handle,
            object_length.as_ptr(),
            &mut length as *mut u32 as *mut u8,
            std::mem::size_of::<u32>() as u32,
            &mut written,
            0,
        )
    };
    unsafe { close(handle, 0) };
    if !check(
        &format!(
            "BCryptGetProperty(SHA256, ObjectLength) -> 0x{status_get:08X}, length={length}, written={written}, open=0x{status_open:08X}"
        ),
        status_open == 0 && status_get == 0 && length > 0 && written == 4,
    ) {
        failures += 1;
    }

    println!();
    if failures == 0 {
        println!("every call through the loader behaved like the real bcrypt.dll");
    } else {
        println!("{failures} check(s) failed");
        std::process::exit(1);
    }
}
