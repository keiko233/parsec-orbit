# CN STUN Swap

A Parsec Orbit plugin for users whose network cannot reach Parsec's own
`stun.parsec.app` endpoint. It hooks `ws2_32.dll!getaddrinfo` inside the
Parsec client and rewrites only that hostname to a healthy STUN server IP.
Nothing else is touched. `stun6.parsec.app` (the IPv6 lookup) is deliberately
left alone: the candidates are IPv4-only, and answering an AAAA lookup with an
A address would break the client's address-family handling.

## Why not `app_stun_address`

`app_stun_address` and `app_client_stun_address` configure Parsec's high
performance relay endpoints, not an RFC 5389 STUN server. Filling them with a
public STUN server is the wrong layer. Parsec's default public STUN hostname is
`stun.parsec.app` (UDP 3478); that is what this plugin replaces.

## What it does

1. Probes every configured candidate with a standard STUN Binding Request and reports each result.
2. Uses either the fastest healthy candidate (automatic mode) or the server selected in the UI (manual mode).
3. Hooks `ws2_32!getaddrinfo` and returns that IP when Parsec resolves
   `stun.parsec.app`.
4. Re-runs the probes periodically and switches endpoints when one dies.

The plugin is intentionally conservative: it never rewrites other hostnames,
does not change `hosts`, does not edit Parsec's config, and falls back to
normal behaviour when no candidate responds. If a later probe pass fails,
it returns to Parsec DNS; manual mode never silently chooses another server.

## Install

Build the workspace and copy the folder into the client's plugin directory:

```powershell
cargo build --release
$src = "F:\Code\parsec-orbit\plugins\parsec-orbit-plugin-stun-swap"
$dst = "C:\Program Files\Parsec\plugins\parsec-orbit-plugin-stun-swap"
New-Item -ItemType Directory -Force -Path $dst | Out-Null
Copy-Item "$src\plugin.toml" "$src\README.md" $dst
Copy-Item "F:\Code\parsec-orbit\target\release\parsec_orbit_plugin_stun_swap.dll" $dst
```

Or use the elevated installer, which also deploys the required hooks plugin:

```powershell
powershell -ExecutionPolicy Bypass -File F:\Code\parsec-orbit\scripts\install.ps1 -EnableStunSwap
```

Restart Parsec completely (stop the service and every `parsecd.exe`, then
start it again). The hooks plugin must be installed, which the official
`parsec-orbit-lib-hooks` release provides.

The first probe starts immediately after the client loads. Check
`%APPDATA%\ParsecOrbit\parsec-orbit.log` for lines like:

```text
stun swap: stun.parsec.app -> 111.206.174.2
```

## Configuration

The STUN Swap section in the Orbit UI selects automatic or manual mode, chooses
the manual server, and accepts custom servers one per line. Use **Save custom
servers** to validate and persist that list. Background probes preserve an
unfinished draft. The plugin stores UI settings in generated `settings.json`
in its data directory, with `format: 1`; edit settings through the panel.
The status panel shows the active server, each candidate's latency/health and
the source of the server list. Unhealthy manual targets return to Parsec DNS.

Environment variables, set before Parsec starts, take precedence over the
saved custom server list:

| Variable | Meaning |
| --- | --- |
| `PARSEC_ORBIT_STUN_SERVERS` | Comma-separated candidates, `host@port` |
| `PARSEC_ORBIT_STUN_SWAP=0` | Disables the plugin |
| `PARSEC_ORBIT_STUN_SWAP_INTERVAL_SECS` | Probe interval, default 300 |

An existing `servers.txt` file is still read for
compatibility: one `host@3478` per line, with `#` comments. It is left in place.
The UI identifies the environment override or the legacy/default list plus
saved custom entries.

The client hook only replaces the resolved IP address and preserves Parsec's
requested service port. The plugin therefore probes and accepts only port
`3478`; custom ports are rejected because the hook cannot redirect them.

Default candidates: `stun.hitv.com@3478`, `stun.miwifi.com@3478`,
`stun.chat.bilibili.com@3478`, `stun.qq.com@3478`, `stun.cloudflare.com@3478`,
then Parsec's own server as a last resort.

## Limitations

STUN only discovers the public address/port mapping. It cannot fix CGNAT,
symmetric NAT, UDP being throttled, or a login/backend endpoint being blocked.
If Parsec already logs `STUN reply from ...`, the STUN path is working and this
plugin is not the fix. It is also an unofficial in-process hook: test it on a
throwaway install first, and consider a self-hosted coturn server on a
mainland cloud provider as the long-term solution.
