# 0001 - Host services are typed, size-prefixed vtables

## Context
Plugins need services beyond logging (WebView operations, the JS runtime, hooks). `ParsecOrbitHost::query_interface` was reserved for this and returns null. Options: (a) one generic `call(service, method, bytes) -> bytes` bus carrying JSON; (b) typed `repr(C)` function tables; (c) a mix.

## Decision
(b). Every service is a `repr(C)` struct that starts with `ParsecOrbitInterfaceHeader { size: u32, version: u32 }` followed by function pointers. Growth is by appending functions; a consumer checks `size` before using a newer entry. Services are registered by name (`parsec-orbit.<service>`) through the host table.

## Consequences
* Direct, allocation-free calls; callable from C; the compiler checks signatures on the Rust side.
* Each service needs its struct specified in `spec/services.md` and mirrored in `parsec-orbit-plugin-abi`.
* Anything JSON-shaped (UI RPC payloads) is still passed as UTF-8 strings *inside* typed calls, so the JS SDK maps one-to-one without a second protocol.
