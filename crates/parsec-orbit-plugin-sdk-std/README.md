# parsec-orbit-plugin-sdk-std

The `std` companions of
[`parsec-orbit-plugin-sdk`](https://crates.io/crates/parsec-orbit-plugin-sdk):

* `ContextExt` - `spawn` (a thread named after the plugin) and
  `install_panic_hook` (panics go to the loader log),
* `TracingExt` (feature `tracing`) - forwards `tracing` events to the loader log,
* `testing::MockHost` - runs a plugin's `init` in a unit test. Use it as a
  dev-dependency of `no_std` plugins too.

It re-exports the SDK, so a plugin that uses `std` needs this dependency only:

```rust
use parsec_orbit_plugin_sdk_std::prelude::*;
```

## License

MIT
