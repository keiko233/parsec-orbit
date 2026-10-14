# macOS terminal log viewer

`scripts/console-macos.sh` follows the Orbit and isolated Parsec logs in the
terminal. It is an independent viewer: it does not launch, inject into, or stop
the Parsec client. Press Ctrl+C to stop following.

Python 3 must be available as `python3` on `PATH`. From the repository root,
run:

```bash
/bin/bash scripts/console-macos.sh
```

The default Orbit root is
`~/Library/Application Support/ParsecOrbit`. The viewer tags Orbit lines with
`[orbit]` and Parsec lines with `[parsec]`. It follows new output from the
current end of each existing log. If a log is replaced or truncated, it reads
from the new beginning. Missing logs are picked up when they appear.

Use an absolute custom root when the install uses `--orbit-root`:

```bash
/bin/bash scripts/console-macos.sh \
  --orbit-root "/Users/me/Library/Application Support/ParsecOrbit test"
```

For a bounded snapshot that exits immediately, add `--once`. It reads at most
the last 256 KiB and 1,000 lines from each log:

```bash
/bin/bash scripts/console-macos.sh --once
```

The viewer reads `parsec-orbit.log` and the isolated client's
`home/.parsec/log.txt` under the selected root. It opens files with no-follow,
nonblocking flags and accepts only regular files owned by the current user with
one link. Reads, displayed lines, and the one-shot backlog are capped; malformed
UTF-8 is displayed with replacement characters.
