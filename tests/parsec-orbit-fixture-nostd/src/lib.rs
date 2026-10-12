//! A `no_std` plugin that leans on every part of the freestanding runtime.
//!
//! What it does is chosen by the end of its plugin id, which the loader hands
//! over in the host table (a `no_std` plugin cannot read a script file):
//!
//! | suffix   | exercises |
//! | --- | --- |
//! | `.alloc` | the process-heap allocator, `memcpy`, `memset`, over-aligned blocks |
//! | `.cmp`   | `memcmp`/`bcmp`, `memmove` with overlap, `strlen` |
//! | `.stack` | `__chkstk` (a frame of 200 KB) |
//! | `.float` | floating point formatting (`_fltused`) |
//! | `.panic` | the panic handler: logs and terminates the process |
//! | other    | nothing |
//!
//! Every action logs `fixture-nostd: <what> ok ...` so a test can assert on it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use parsec_orbit_plugin_sdk::prelude::*;

parsec_orbit_plugin_sdk::freestanding!();

#[derive(Default)]
struct Fixture;

#[repr(align(64))]
struct Aligned([u8; 100]);

fn alloc_work(cx: &Context) {
    let mut numbers: Vec<u32> = Vec::new();
    for i in 0..20_000 {
        numbers.push(i);
    }
    let sum: u64 = numbers.iter().map(|&n| n as u64).sum();

    let zeros = vec![0u8; 1 << 20];
    let all_zero = zeros.iter().all(|&byte| byte == 0);

    let mut text = String::new();
    for i in 0..50 {
        text.push_str(&alloc::format!("{i},"));
    }
    let copy = text.clone();

    let aligned = Box::new(Aligned([7; 100]));
    let aligned_ok = (&*aligned as *const Aligned as usize) % 64 == 0 && aligned.0[99] == 7;

    info!(
        cx,
        "fixture-nostd: alloc ok sum={sum} zeros={all_zero} text={} copy={} aligned={aligned_ok}",
        text.len(),
        copy == text
    );
}

fn compare_work(cx: &Context) {
    let a = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let mut b = a;
    let equal = a == b;
    b[9] = 11;
    let different = a != b;

    let mut shifted = a;
    shifted.copy_within(0..7, 3); // overlapping, destination above source
    let forward = shifted == [1, 2, 3, 1, 2, 3, 4, 5, 6, 7];
    let mut back = a;
    back.copy_within(3..10, 0); // overlapping, destination below source
    let backward = back == [4, 5, 6, 7, 8, 9, 10, 8, 9, 10];

    let less = "apple".as_bytes() < "apply".as_bytes();
    let c_len = c"twelve chars".to_bytes().len();

    info!(
        cx,
        "fixture-nostd: cmp ok equal={equal} different={different} forward={forward} backward={backward} less={less} strlen={c_len}"
    );
}

#[inline(never)]
fn big_frame(seed: u8) -> u64 {
    let mut frame = [0u8; 200_000];
    for (i, byte) in frame.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_add(seed);
    }
    core::hint::black_box(&mut frame);
    frame.iter().map(|&byte| byte as u64).sum()
}

fn stack_work(cx: &Context) {
    info!(cx, "fixture-nostd: stack ok sum={}", big_frame(3));
}

fn float_work(cx: &Context) {
    let value = 22.0_f64 / 7.0;
    info!(
        cx,
        "fixture-nostd: float ok pi~{value:.4} sqrtish={:.2}",
        value * value
    );
}

impl Plugin for Fixture {
    fn init(&mut self, cx: &Context) -> Result {
        let id = cx.plugin_id();
        info!(cx, "fixture-nostd: init {id}");
        if id.ends_with(".alloc") {
            alloc_work(cx);
        } else if id.ends_with(".cmp") {
            compare_work(cx);
        } else if id.ends_with(".stack") {
            stack_work(cx);
        } else if id.ends_with(".float") {
            float_work(cx);
        } else if id.ends_with(".panic") {
            panic!("boom from {id}");
        }
        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.example.fixture-nostd",
    name = "Fixture (no_std)",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Exercises the freestanding runtime",
    license = "MIT",
    plugin = Fixture,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk_std::testing::MockHost;

    fn run(id: &str) -> MockHost {
        let host = MockHost::new(id);
        host.init::<Fixture>().1.unwrap();
        host
    }

    #[test]
    fn the_work_gives_the_results_the_e2e_tests_expect() {
        assert!(run("x.alloc")
            .logged("alloc ok sum=199990000 zeros=true text=140 copy=true aligned=true"));
        assert!(run("x.cmp").logged(
            "cmp ok equal=true different=true forward=true backward=true less=true strlen=12"
        ));
        assert!(run("x.stack").logged("stack ok sum="));
        assert!(run("x.float").logged("float ok pi~3.1429 sqrtish=9.88"));
    }

    #[test]
    fn other_ids_do_nothing_special() {
        let host = run("x.plain");
        assert_eq!(host.logs().len(), 1);
    }
}
