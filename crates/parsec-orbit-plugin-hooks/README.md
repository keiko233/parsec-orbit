# Hooks

Official [Parsec Orbit](../../README.md) plugin. It provides
`parsec-orbit.hooks` ([spec](../../docs/spec/services.md#parsec-orbithooks-provided-by-the-hooks-plugin-version-10)):
inline hooks on functions of the client process. A hook overwrites the start of
the target with a jump to a detour and builds a trampoline that runs the
displaced instructions and continues in the original function (the instruction
relocation is done by [`retour`](https://crates.io/crates/retour)).

```rust
let hooks = HooksClient::new(cx).expect("is com.parsec-orbit.hooks installed?");
let target = hooks.resolve("kernel32.dll", "GetTickCount").unwrap();
let (hook, original) = unsafe { hooks.install(target, my_detour as *const _)? };
hooks.enable(hook)?;
```

Hooks are installed disabled; `enable`, `disable` and `remove` take the id; a
function can be hooked once. A detour has the exact signature and calling
convention of the target and must never unwind. Everything beyond that - what
the detour does, whether other threads are inside the function when the hook is
switched - is the plugin's responsibility. This is the sharpest tool a plugin
can ask for: list the plugin in `requires` only if you need it, and install it
only for plugins you trust. It is not part of the default install.

x86-64 only. Tests hook functions of the test process, including a system
function.
