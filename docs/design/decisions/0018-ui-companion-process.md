# 0018 - Run the winio UI renderer in an independent helper process

## Context

ADR 0016 selects an external `winio` window and a shared declarative UI service
for Rust and JavaScript plugins. Running the renderer in a thread of the UI
plugin still places native GUI event-loop failures in the Parsec client process.
The service must also preserve its C ABI and callback pointers while supporting
the Windows and macOS renderers. The macOS host's out-of-scope rule concerns
moving arbitrary plugin execution out of the client; a child process limited to
rendering the UI is a narrower boundary.

## Decision

1. Keep the `parsec-orbit.ui` ABI, schema validation, callback registrations,
   registry and desired visibility in the official UI plugin inside the client.
   The plugin has no GUI dependency.
2. Use `winio` as the sole native UI implementation and run
   `parsec-orbit-ui-companion[.exe]` beside the plugin library. The helper owns
   the main-thread `winio` event loop on Windows and macOS; platform windowing
   stays behind the corresponding `winio` backend.
3. Use private inherited stdin/stdout pipes for version 1 newline-delimited
   JSON IPC. The protocol has a 4 MiB maximum complete line and a version
   handshake. Do not create a socket listener, port or authentication token.
   Callback and user pointers remain in the plugin process.
4. Launch hidden and return promptly from plugin `init`. `show()` and `hide()`
   update desired visibility and enqueue requests without waiting. Closing the
   window hides it; parent-pipe EOF exits the helper.
5. Deliver action callbacks on a separate plugin-owned thread, outside the UI,
   IPC and host main threads. Cancellation waits for in-flight callbacks except
   when cancellation is called by that callback.
6. On helper or transport failure, reconnect automatically with a finite retry
   count and backoff, then restore the registry and desired visibility. This
   isolates ordinary renderer and transport faults from the client. Native
   plugin and callback faults remain in the client process and are not contained.
7. On Windows session 0, retain the existing host activation channel for
   hotkeys and CLI, but do not launch a UI helper. macOS initially exposes
   programmatic show/hide; external activation is not claimed unless its
   renderer implements it.
8. Keep GUI dependencies in `parsec-orbit-ui-companion`; the protocol uses
   ordinary `serde`/`serde_json` in the UI plugin and has no `no_std` requirement.
   Do not change the loader, `parsec-orbit-core`, or the existing macOS release
   artifact whitelist. Public release packaging is deferred.

## Consequences

* UI declarations and callback identity remain governed by the existing service
  ABI, while only serializable renderer data crosses the process boundary.
* A crashed helper can be restarted without losing registered sections or the
  requested visibility. The UI may be unavailable while finite retries are
  exhausted or in progress.
* A helper cannot directly invoke native callbacks. The plugin validates and
  dispatches actions on its delivery thread; this does not sandbox native
  plugins or protect the client from their faults.
* Development staging must place the helper beside the plugin library. macOS
  helper signing and runtime-state sandbox compatibility require validation in
  the parent process before they can be claimed as supported.
