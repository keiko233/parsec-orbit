# STUN selection panel

The STUN Swap plugin offers `auto` and `manual` selection. Automatic mode
selects the healthy candidate with the lowest measured STUN round-trip latency.
Manual mode selects only the chosen candidate, while continuing to measure all
candidates. A failed manual target is shown as unhealthy and falls back to
Parsec's original DNS rather than silently selecting another candidate.

Each completed probe updates the candidate health, resolved IPv4 address and
latency display, active target and server-list source. Selection uses the
current mode when committing probe results, including changes made while a
probe was in progress. Dynamic dropdown options do not rebuild the panel. The
manual server choice is enabled only in Manual mode, and the probe interval the
slider shows is limited to 10 to 600 seconds even when an environment override
lies outside it.

Custom entries are edited one per line, validated and explicitly saved; a
rejected list is reported as the status of the custom servers setting.
Incomplete drafts survive background probe updates. Entries use `host@3478`
(a bare host defaults to 3478). Other ports are rejected: the DNS hook replaces
the resolved address and preserves Parsec's service port. Hosts containing
whitespace, path separators or control characters are rejected. Failed
validation or persistence is displayed, and environment overrides are
identified explicitly.

The plugin persists UI settings in its own data directory's generated
`settings.json`, with `format: 1`, `mode`, `selected`, `custom_servers`,
`enabled`, and `interval`. Readers refuse unknown formats. Legacy `servers.txt`
is still readable for existing installations; new UI settings do not overwrite
that file. A non-empty `PARSEC_ORBIT_STUN_SERVERS` overrides candidate lists;
an unset, empty or whitespace-only value uses the saved custom entries with
legacy/default candidates. The panel names which source won.
`PARSEC_ORBIT_STUN_SWAP` retains its disable override, and
`PARSEC_ORBIT_STUN_SWAP_INTERVAL_SECS` overrides the saved interval.

Tests use injected settings paths and owned loopback STUN responders, never an
installed Parsec client or user configuration.
