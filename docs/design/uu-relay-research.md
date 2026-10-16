# Parsec native relay to UU TURN: Windows feasibility research

Research date: 2026-10-17 (Asia/Tokyo).

Status: feasibility research and a proposed experiment plan. This is neither an accepted
architecture decision nor an implemented feature. Evidence comes from repository
sources, official documentation and static inspection of an official binary. No
installed Parsec client, account, credential, real configuration or live relay was used.

## Recommendation

Prioritize a standalone Windows Rust bridge implementing the minimum native Parsec
HPR-facing behavior, backed by a UU TURN UDP allocation as its WAN socket. Keep Parsec's
native BUD data path and application protocol intact. Orbit should supervise/configure
the bridge after feasibility is established; the first prototype should not require
general Winsock interception, a virtual network adapter, or a new client/media
implementation.

The desirable first data path is:

    Unmodified Parsec client A
       -- native STUN connectivity checks / encrypted BUD over UDP -->
    UU TURN allocated public address R
       -- TURN Data indications / ChannelData -->
    Bridge B: virtual WAN socket + HPR pairing/address maps
       -- ordinary local UDP -->
    Unmodified Parsec host B

Replies follow the inverse route. A companion on A may initially be needed for UU
authorization/session coordination and discovering the actual client egress IP. This
does not require replacing A's Parsec transport. One-sided Parsec data-plane integration
is a goal, not a validated capability or a promise of one-sided installation.

This architecture is conditional on two earliest gates: the intended Parsec
account/build must use the relay configuration; UU must permit native Parsec peer
traffic under legitimately issued TURN credentials. Do not bypass account/service
authorization if either gate rejects the intended use.

## Official Parsec evidence

- HPR configuration:
  https://support.parsec.app/hc/en-us/articles/32381057878292-Configure-Parsec-Relay-Server
- Legacy protocol description:
  https://support.parsec.app/hc/en-us/articles/32381397579284-Configure-Parsec-Relay-Server-Legacy
- HPR overview/network matrix:
  https://support.parsec.app/hc/en-us/articles/32381058430740-High-Performance-Relay-HPR-Server-Overview-and-Prerequisites
- Deployment/forcing:
  https://support.parsec.app/hc/en-us/articles/32381482907156-Deployment-Considerations-and-Options
- Advanced configuration:
  https://support.parsec.app/hc/en-us/articles/32381443626516-All-Advanced-Configuration-Options

`app_stun_address` is the host-facing relay configuration; `app_client_stun_address` is
the client-facing equivalent. The documented address format is `IP@PORT`. Official
documentation categorizes these as Enterprise settings. That categorization neither
proves acceptance by a personal account nor proves a particular client-side enforcement
mechanism; current intended builds/accounts require measurement.

The legacy documentation explicitly describes a relay that advertises its own public
address through a STUN response. The current network matrix lists UDP/STUN on the
private listener and UDP/BUD on the public listener. Thus native HPR and generic WebRTC
TURN are different client-facing protocols.

HPR v2's official service additionally registers/authenticates to the Teams API with a
Team ID and relay secret and sends heartbeats. Downloadability is not evidence that the
official service is usable without its required authorization. The bridge research
separates data-plane interoperability from that managed service's control plane.

The official sources disagree on the meaning of `app_force_relay=1` versus `=2`: the
deployment article says 1 forces all traffic and 2 allows direct LAN; the advanced
configuration table labels them in the opposite order. Both agree it is a host-side
option. Do not hard-code a semantic interpretation without checking the current client.

## Static inspection of the official HPR binary

Downloaded, extracted and inspected without executing:

- URL: https://builds.parsec.app/hpr/parsechpr2.0.1.linux-amd64.tar.gz
- Extracted executable SHA-256:
  a8c9723b2e675ff07db7b9cb32615abb11e3d1920e6b28a8f376e38fff90d24c
- ELF x86-64, static Go executable, stripped symbols; Go runtime metadata remains.
- `go version -m`: Go 1.23.8; module github.com/parsec-cloud/parsechpr; revision
  3241f022d6cf0490c93e035c4d20fe586c641dd9; VCS time 2026-03-24T13:18:34Z.
- The archive's signature was not cryptographically verified. The SHA identifies the
  inspected bytes; it does not independently authenticate them.

Used Go `debug/elf` + `debug/gosym` to resolve runtime function/line tables, and system
LLVM objdump to inspect instructions. The binary and generated disassembly are not
distributed with this document.

To repeat the static inspection, download the archive above into a temporary directory,
extract it and first compare the executable hash. Inspect metadata without executing the
binary:

```sh
shasum -a 256 parsechpr
go version -m parsechpr
```

For stripped Go symbols, read `.gopclntab` with `debug/elf`, construct
`gosym.NewLineTable` using the `.text` section address, and pass it to `gosym.NewTable`.
Enumerate `Table.Funcs` for function ranges; `PCToLine` and `PCToFunc` resolve source
locations and direct-call targets. Disassemble the ranges below with LLVM objdump's
`--start-address` and `--stop-address` options. These addresses apply only to the
identified executable.

Observed in this binary:

1. `main.listener` at 0x65dfa0, `main.stunMakeReply` at 0x6639a0, plus `mapUnameGet`,
   `mapUnameSet`, `mapGet`, and `mapSet`.
2. An internal/host binding branch recognizes a 20-byte STUN binding request and checks
   the standard magic cookie. It extracts two 6-byte keys from the 12-byte transaction
   ID and records a username/fragment-pair association. This is application-specific
   interpretation of an otherwise STUN-shaped request.
3. Another branch recognizes a 104-byte binding request and extracts two 6-byte values
   from fixed positions in the request's attribute area; it looks up the association and
   establishes endpoint maps in both directions. Inlined metadata names include
   `stunIsClientBindingRequest`, `stunGetClientUname` and `ufragPair`. Full field
   grammar and future build compatibility are unverified; fixed offsets alone must not
   become a general parser.
4. `stunMakeReply` writes Binding Success 0x0101 and XOR-MAPPED-ADDRESS 0x0020, encoding
   the selected address and port. IPv4/IPv6 response paths use 32/44 bytes respectively.
5. The steady-state mapped-packet path at 0x65f4ff..0x65f557 passes the received buffer
   and length to `net.(*UDPConn).WriteToUDPAddrPort`. The inspected path does not parse
   codecs, terminate media encryption, or add an application media header.
6. The listener also checks global enable flags before pairing/forwarding. Their
   presence is consistent with the managed service's state; this analysis does not
   establish all control-plane validation behavior.

The following instruction sites anchor the forwarding observation:

```text
0x65e14c  call net.(*UDPConn).ReadFromUDPAddrPort  listener.go:53
0x65e4c0  call main.mapUnameSet                   listener.go:73
0x65e78a  call main.stunMakeReply                 listener.go:91
0x65ee79  call main.mapUnameGet                   listener.go:105
0x65eee7  call main.mapSet                        listener.go:107
0x65ef52  call main.mapSet                        listener.go:108
0x65f54a  lea  received-buffer, %rbx              listener.go:127
0x65f557  call net.(*UDPConn).WriteToUDPAddrPort  listener.go:127
```

The forwarding call retains the received datagram length in the Go slice argument. Its
destination comes from the endpoint map. Source filenames and line numbers above come
from the executable's Go runtime metadata, not access to proprietary source.

Inference: the useful HPR interoperability surface is small enough to investigate as a
datagram bridge. It is substantially narrower than replacing all Winsock
send/receive/completion behavior. This is static evidence, not a successful
native-client session.

## Proposed bridge behavior and the critical unknowns

The private listener is local to the host. In the first prototype, bind local listeners
to loopback and restrict forwarding to the explicitly paired session. The advertised WAN
address is the actual TURN allocation. Keep fragment-pair routing separate from service
authorization: a matching fragment pair alone is not authorization to forward arbitrary
traffic.

1. Establish the authorized UU control session and obtain server-issued TURN
   URL/username/credential through the observed session handshake. Keep required session
   lifecycle active; do not assume credentials can be copied once and used indefinitely.
2. Allocate one public UDP relay endpoint for the initial single Parsec session. Prefer
   UDP client-to-TURN transport; TCP/TLS are later connectivity fallbacks with different
   latency characteristics.
3. Obtain the intended Parsec client A's actual public source IP and create the TURN
   permission before advertising R. Standard TURN silently drops inbound peer packets
   without permission; it cannot first learn an unknown peer from a packet the server
   refuses to deliver. Permission is IP-based; the actual NAT source port can be learned
   on an allowed incoming packet. Channel binding then uses the observed endpoint. Use
   Send/Data indications until an appropriate channel is established.
4. Run an HPR-facing private UDP listener on B. On a recognized host registration,
   return the allocation's *real public relay address R*, not loopback. Associate the
   native HPR fragment pair with the local host socket and that allocation.
5. Receive A's initial native connectivity checks via the TURN virtual WAN socket;
   validate the expected Parsec session association, establish the endpoint mapping, and
   forward the original bytes to B from the bridge's local public-side UDP socket. Route
   B's replies back through TURN to A. The exact local source-port behavior and native
   host route selection must be validated against current Parsec; ordinary UDP
   forwarding alone is not proof.
6. Continue opaque BUD forwarding with bounded queues and original datagram boundaries.
   Refresh allocations and permissions. Fail the session clearly on allocation loss;
   transparent migration to a new relay address is out of scope for the first prototype.

Important unresolved conditions:

- Personal-account handling of the HPR configuration and current native client
  acceptance of its responses.
- UU service restrictions on peer IPs/ports, credentials, session duration, payloads,
  quotas and allocation refresh.
- How A's correct egress IP is obtained if its Parsec UDP path differs from its UU/API
  path due to proxies, multihoming or routing policy. A generic HTTPS public-IP lookup
  is insufficient evidence.
- Native B's ability to use the local public-side UDP source through the HPR path, with
  the advertised TURN address R; STUN/DTLS and subsequent BUD route behavior need
  real-client capture.
- HPR pairing order, registration refresh, stale mappings and changes in vendor builds.

This design avoids publishing fake loopback candidates to a remote client, a virtual
NIC, a fabricated remote source IP, and an added reliable FILE/SCTP media tunnel. Its
remaining address/source invariants are explicit test gates.

## Orbit integration boundary

The existing [STUN Swap plugin](../../plugins/parsec-orbit-plugin-stun-swap/README.md)
redirects DNS for public STUN discovery. It does not implement HPR pairing or a TURN
allocation. Keep this experiment separate from its current behavior.

After feasibility gates pass, a separate Rust executable can own UU authentication,
signaling, allocations and their mutable state. An optional Orbit plugin can supervise
that process and expose settings/status through existing services. Dependencies are
constructed and owned explicitly; callbacks and packet queues remain bounded. Keep all
new networking/runtime dependencies outside the loader and core, following the existing
[architecture](architecture.md) and [host-service contracts](../spec/services.md).

## UU source evidence

Repository: [Farwing / OpenUUYC](https://github.com/djkcyl/farwing). Inspected `main`:
a87b8e772c238e542215ab039f1b1d2ca4a9ef46 (OpenUUYC); default `next` contains the new
Farwing project direction.

- [`src/transport/signal/negotiation.rs`][uu-source-1]:
  ControlSessionInfo/ControlIceServer with iceServers, username, credential and
  force_relay.
- [`src/transport/signal/mod.rs`][uu-source-2]:
  start_control obtains server control acknowledgement before negotiation.
- [`src/session/controller/mod.rs`][uu-source-3]:
  builds the peer using those credentials.
- [`vendor/turn/src/client/mod.rs`][uu-source-4]:
  allocation, authentication, alternate-server handling, UDP requested transport.
- [`vendor/turn/src/client/relay_conn.rs`][uu-source-5]:
  packet-oriented send/receive, explicit permission/channel entries and refresh.
- [`src/features/port_mapping/udp.rs`][uu-source-6]:
  explicitly retains SCTP reliability.
- [`src/transport/rtc/channels.rs`][uu-source-7]:
  FILE_DATA_CHANNEL is ordered; UDP mapping uses this channel.
- [`src/session/negotiation.rs`][uu-source-8]:
  a PortMapping control purpose exists, but even its shared request builder currently
  requires compatible decoder capability metadata. A minimal headless session needs
  careful extraction of required protocol behavior, not just deletion of media modules.

The service's actual behavior is not inferable from client-side support alone. RFC 8656
sections 9 and 11 document the general permission and datagram-relaying model, not
UU-specific admission policy: https://www.rfc-editor.org/rfc/rfc8656.html

Farwing/OpenUUYC's project-wide
[LICENSE](https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/LICENSE)
explicitly says it is source-visible rather than released under an open-source license.
Original contributions and third-party derived code have different permission scopes.
Protocol facts and copied implementation code must be treated separately; the latter
needs a reuse basis. Official service access is a separate authorization question.

## Alternatives considered

| Path | Value for this task | Reason |
| --- | --- | --- |
| Native HPR front end + UU TURN virtual WAN socket | First choice, conditional | Retains native Parsec features/BUD; ordinary process/socket boundary; narrow protocol adapter |
| General Winsock interception | Reserve only | Must preserve overlapped I/O, IOCP, completion/error semantics and socket lifecycle; unnecessary before HPR is tested |
| Existing OpenUUYC UDP mapping | Connectivity comparison only | FILE/SCTP reliability adds retransmission/ordering; still needs Parsec address integration |
| WebRTC/ParsecWebTurn client + UU credentials | Alternative if native-client preservation is relaxed | Existing standard TURN path, but uses a different client and SCTP media path, with feature/private-ABI limits |
| Full Parsec-to-UU media translation | Poor fit | Reimplements unrelated media/control behavior |
| Official HPR v2 service as a drop-in | Not a UU bridge | Managed Linux service with Teams registration; no documented pluggable UU upstream |

ParsecWebTurn inspected at b7fc08d1f19fa2afd8222ca407a5fa729b83893d. Its current main is
a native Windows Rust client around an unmodified pinned Parsec WASM core; older cached
web pages describe its former WebView2 version. Current sources, not cached summaries,
informed this comparison. Technical guide:
https://github.com/vampywiz17/ParsecWebTurn/blob/b7fc08d1f19fa2afd8222ca407a5fa729b83893d/docs/NATIVE_TECHNICAL.md

Its force-TURN notes report single-provider live successes but retain multi-endpoint
limitations:
https://github.com/vampywiz17/ParsecWebTurn/blob/b7fc08d1f19fa2afd8222ca407a5fa729b83893d/docs/native-force-turn.md

These are upstream-reported results, not independently reproduced here.

## Next experiments, in order

Use the existing [testing policy](../development/testing.md): automated protocol and
lifecycle tests use owned sockets, fake peers and injected paths. The Windows
client/service experiments below are separate future acceptance work using disposable
copies and private state, with explicit account/device scope; they are not authorization
to touch an installed client or a real user configuration.

A. Native HPR contract gate (no UU dependency): isolated Windows clients/configs,
explicitly chosen builds/accounts, a local/private test HPR responder and owned UDP
forwarding endpoints. Confirm registration packet shapes, response acceptance,
source-port behavior and selection of the relayed route. Establish the correct
force-relay semantics by packet evidence, given contradictory docs. If settings are
rejected by the intended account, stop this route without patching entitlement checks.

B. UU service gate (no Parsec dependency): normal authorized control session; allocate
UDP; install permission for an owned sender's public IP; send random datagrams from that
sender to R; verify exact round trips, refresh, duration, error responses and UDP
transport provenance. Separate service rejection from a local permission/entry
implementation error.

C. Single-session integration: one Windows host-side HPR bridge, original Windows Parsec
client, allowed helper for control coordination. Prove initial checks and bidirectional
BUD complete via UU, with direct fallback excluded in the disposable test environment.
Do not count a successful direct session as relay acceptance.

D. Performance acceptance: compare direct and bridge at 10/30/50 Mbps, observe RTT
p50/p95/p99, loss/reordering, local queue residence, freezes and end-to-end frame/input
behavior. Include duration across a real refresh cycle. Use synthetic packet tests for
bridge overhead and actual sessions for experience; neither substitutes for the other.

Only after A/B/C pass should Orbit add supervision/settings and distribution. No new
dependency enters the loader or core. The bridge can own its async/runtime dependencies
in a separate executable. No live account, client or service acceptance is established
by this document.

[uu-source-1]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/src/transport/signal/negotiation.rs
[uu-source-2]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/src/transport/signal/mod.rs
[uu-source-3]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/src/session/controller/mod.rs
[uu-source-4]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/vendor/turn/src/client/mod.rs
[uu-source-5]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/vendor/turn/src/client/relay_conn.rs
[uu-source-6]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/src/features/port_mapping/udp.rs
[uu-source-7]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/src/transport/rtc/channels.rs
[uu-source-8]: https://github.com/djkcyl/farwing/blob/a87b8e772c238e542215ab039f1b1d2ca4a9ef46/src/session/negotiation.rs
