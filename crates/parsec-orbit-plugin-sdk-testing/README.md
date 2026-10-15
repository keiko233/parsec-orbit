# parsec-orbit-plugin-sdk-testing

`MockHost` for unit-testing a [Parsec Orbit](https://github.com/keiko233/parsec-orbit)
plugin: it runs the plugin's `init` against a recording host table. Use it as a
**dev-dependency**, in `std` and `no_std` plugins alike:

```toml
[dev-dependencies]
parsec-orbit-plugin-sdk-testing = "0.1"
```

```rust
use parsec_orbit_plugin_sdk_testing::MockHost;

let host = MockHost::new("com.example.hello");
let (_plugin, result) = host.init::<Hello>();
assert!(result.is_ok());
```

## License

MIT
