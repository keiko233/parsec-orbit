# Additional bandwidth choices

The native and WebView high-bitrate plugins share the config-service owner
`com.parsec-orbit.native`. `extra_bandwidth_mbps` is a JSON integer array and
`enabled` is a boolean. Both plugins declare the config service as an optional
load-order dependency, retaining their defaults when it is absent.

The native High Bitrate panel edits the additional choices appended to Parsec's
built-in bandwidth list, whose maximum is 50 Mbps. For example, adding `85`
offers 85 Mbps in Parsec's own dropdown. Accept at most 128 input entries, each
an `i32` integer greater than 50, then sort and deduplicate. An empty array
adds no choices. The original default values remain
`[60,75,100,125,150,200,250,300,400,500]`, embedded from one shared asset.

Validate before persisting. Report invalid inputs, missing config service,
save failures and successful saves in the native panel's patch status. A
failed save must not replace the active in-memory list. Both the list and the
enabled setting survive restarting when the config service is installed.
`PARSEC_ORBIT_PATCHES=0`, `off`, or `false` still disables patching regardless
of stored settings.

The native patch reads the list when the client UI module becomes available;
it does not rewrite a previously patched table. The WebView plugin reads the
same owner/key when initializing and substitutes the values into the injected
script. Changes made after either patch has run require restarting the client.
Neither plugin links a GUI toolkit; the WebView plugin remains `no_std`.
