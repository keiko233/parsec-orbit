use std::ffi::c_void;

#[path = "../src/unsupported.rs"]
mod unsupported_backend;

#[test]
fn unsupported_backend_resolves_nothing_and_returns_not_ready() {
    let hooks = &unsupported_backend::HOOKS;
    let resolve = hooks.resolve.expect("resolve entry");
    let install = hooks.install.expect("install entry");
    let enable = hooks.enable.expect("enable entry");
    let disable = hooks.disable.expect("disable entry");
    let remove = hooks.remove.expect("remove entry");

    unsafe {
        assert!(resolve(std::ptr::null(), 0, std::ptr::null(), 0).is_null());
        let sentinel = 0_u8;
        let sentinel_pointer = &sentinel as *const u8 as *const c_void;
        let mut original = sentinel_pointer;
        assert_eq!(
            install(std::ptr::null(), std::ptr::null(), &mut original),
            -4
        );
        assert_eq!(
            original, sentinel_pointer,
            "unsupported install leaves output alone"
        );
        assert_eq!(enable(1), -4);
        assert_eq!(disable(1), -4);
        assert_eq!(remove(1), -4);
    }
}
