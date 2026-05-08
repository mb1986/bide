# `bide` — Requirements

A small Linux command-line tool that blocks until a remote host is *stably* reachable, defined as N consecutive successful probes. Intended for use in scripts that need to wait for a machine to come back online (after a reboot, power cycle, DHCP lease renewal, etc.) before proceeding.

Reads naturally in wait-until-ready scripts: `bide -t 30 server01 && ssh server01 ...`

Also exposes a tries-based cap (`-n`) for callers that prefer to bound by attempts rather than wall-clock time, and an inversion flag (`--not`) that waits for a host to *stop* responding rather than start.

## Rationale

Existing tools have gaps:

- `ping -c N` counts total replies, not consecutive ones — a flaky host that answers once and goes silent passes.
- `ping -w` has an overall deadline but no streak semantic.
- `wait-for-it` / `wait-for` test TCP ports, not ICMP, and have no "N consecutive" logic.
- `fping` reports per-packet results without a "settle until stable" mode.

`bide` fills the specific niche of *"block my script until this host is reliably up,"* and does so with a short name that reflects waiting and stays accurate as probe types expand beyond ICMP.

## Synopsis

```
bide [OPTIONS] <HOST>
```

## Probe model

`bide` is built around a pluggable probe concept: something that, given a target, returns success or failure within a bounded time. The tool itself handles scheduling, streak tracking, deadlines, and exit codes — the probe backend only answers "did the host respond?"

**v1 ships with ICMP only** (see FR-1). TCP and HTTP probes are explicit future additions — see [Planned probe modes](#planned-probe-modes-post-v1). Defaulting to ICMP when no probe is specified preserves the simple `bide server01` invocation indefinitely.

## Behavior

1. Every `--interval` seconds, send one probe to `HOST`.
2. A probe succeeds if a valid response arrives before the next tick is due; otherwise it fails. No separate per-probe timeout knob — the interval is the deadline.
3. Maintain a running count of consecutive successful probes.
4. Any failure (no response, unreachable, error response) resets the streak to 0.
5. Exit `0` as soon as the streak reaches `--count`.
6. If `--timeout` is set and the overall deadline is reached before the streak is achieved, exit `1`.
7. If `--max-tries` is set and that many probe attempts have been completed before the streak is achieved, exit `1`.
8. If both `--timeout` and `--max-tries` are `0` (the defaults), keep trying indefinitely.

Three distinct "give up" events exist and are reported with different words: a single probe not answering within `--interval` is a **no response** (resets the streak, loop continues); the overall `--timeout` being reached is a **deadline** (tool exits 1); `--max-tries` being exhausted is **max tries reached** (tool exits 1). See [Output](#output).

**Inverted mode (`--not`):** steps 3 and 4 swap roles. A probe that fails to get a response counts toward the streak; a probe that *did* get a response resets the streak to zero. Everything else — scheduling, timeout, max-tries, exit codes — is unchanged. Useful for scripts that must wait for a host to go offline (e.g., confirming a shutdown completed) before proceeding.

## Command-line interface

| Flag | Long form | Default | Meaning |
|------|-----------|---------|---------|
| `-i` | `--interval` | `3` | Seconds between probe attempts. |
| `-c` | `--count` | `3` | Number of consecutive successful probes required. |
| `-n` | `--max-tries` | `0` | Maximum total probe attempts. `0` means no limit. |
| `-t` | `--timeout` | `0` | Overall deadline in seconds. `0` means no deadline. |
| —    | `--not` | off | Invert: wait for the host to *stop* responding instead. |
| `-q` | `--quiet` | off | Suppress progress output; only exit code matters. |
| `-v` | `--verbose` | off | Print each attempt and its result. |
| `-h` | `--help` | — | Show help. |
| `-V` | `--version` | — | Show version. |

Positional argument: `HOST` — an IPv4 address, IPv6 address, or hostname.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Required consecutive successes achieved. |
| `1` | Overall deadline (`--timeout`) reached, or max tries (`--max-tries`) exhausted, without achieving the streak. |
| `2` | Invalid arguments or usage error. |
| `3` | Host resolution failed or fatal network error unrelated to reachability (e.g., no route, permission denied on raw socket). |
| `130` | Interrupted by SIGINT (Ctrl-C). Streak state is discarded. |

## Functional requirements

- **FR-1** v1 uses ICMP echo as the probe mechanism. The probe backend is abstracted behind an internal trait so TCP and HTTP backends can be added without altering the scheduler, streak logic, or CLI.
- **FR-2** Must send exactly one probe per interval tick. No bursts.
- **FR-3** Interval timing is measured from the start of each attempt, not from the end of the previous response — slow responses must not slide the schedule.
- **FR-4** A failed probe resets the consecutive counter to zero, not one.
- **FR-5** The overall deadline (`--timeout`) is measured from program start, not from the first successful probe.
- **FR-6** If `--timeout` is non-zero and smaller than `--interval × --count`, the tool must still start and simply fail fast when the deadline hits (no pre-flight rejection). The same applies if `--max-tries` is non-zero and smaller than `--count` — start anyway, and fail when attempts run out.
- **FR-7** Hostnames are resolved once at startup. If resolution fails, exit `3`. (Re-resolution on every attempt is a non-goal in v1; see [Out of scope](#out-of-scope-v1).)
- **FR-8** IPv4 and IPv6 both supported. Address family inferred from the resolved address.
- **FR-9** Must work without root on Linux kernels that support unprivileged ICMP sockets (`net.ipv4.ping_group_range`). Fall back to a clear error if neither unprivileged ICMP nor `CAP_NET_RAW` is available — do not silently degrade to shelling out to `/bin/ping`.
- **FR-10** The per-probe response wait is bounded by `--interval`. A probe that has not succeeded by the time the next tick fires is counted as a failure. This keeps the "one probe per tick" invariant (FR-2) trivially true and avoids a separate per-probe timeout flag in v1.
- **FR-11** If `--max-tries` is non-zero, the tool exits `1` once that many probe attempts have been completed without achieving the streak. Every attempt counts toward the limit, whether it succeeded or not — the budget is total attempts, not failures. When both `--timeout` and `--max-tries` are set, whichever limit is hit first terminates the run.
- **FR-12** The `--not` flag inverts the streak rule: a no-response counts toward the streak, and a successful reply resets it to zero. Timeouts, attempt budgets, exit codes, and output cadence are otherwise identical to default mode. Caveat: a no-response cannot be distinguished from a local network fault (no route, firewall drop, ARP failure); `--not` is therefore not a safety-critical "host is confirmed gone" signal.

## Non-functional requirements

- **NFR-1** Single static binary, no runtime dependencies beyond libc.
- **NFR-2** Resident memory under 10 MB in steady state.
- **NFR-3** Clean shutdown on SIGINT and SIGTERM — no orphaned sockets.
- **NFR-4** Output to stderr for progress/errors; stdout reserved for future machine-readable output.
- **NFR-5** Deterministic behavior: given the same network conditions, the exit code and timing must be reproducible.

## Output

Default (neither `-q` nor `-v`): one line per state transition. Two distinct events appear:

- `N/M` — a probe matched the streak condition; streak is now at N out of M required.
- `no response — streak reset` — default mode: a probe did not answer within `--interval`; streak returns to 0.
- `responded — streak reset` — `--not` mode: a probe *did* answer; streak returns to 0.
- `M/M ok` — final streak-matching probe; tool exits 0.
- `deadline reached after Ns` — overall `--timeout` elapsed; tool exits 1.
- `max tries reached after N attempts` — `--max-tries` attempts were made without hitting the streak; tool exits 1.

Successful run (host goes briefly silent mid-run, then stabilizes):

```
192.168.10.10: 1/3
192.168.10.10: 2/3
192.168.10.10: no response — streak reset
192.168.10.10: 1/3
192.168.10.10: 2/3
192.168.10.10: 3/3 ok
```

Failed run (host never stabilizes before `-t 30` elapses):

```
192.168.10.10: 1/3
192.168.10.10: no response — streak reset
192.168.10.10: 1/3
192.168.10.10: deadline reached after 30s
```

Verbose: add RTT, sequence numbers, and the active probe backend. Quiet: emit nothing.

## Examples

```bash
# Default: wait forever until 3 ICMP pings in a row succeed, 3 s apart.
bide 192.168.10.10

# 5 s interval, 3 consecutive successes, 30 s overall deadline.
bide -i 5 -c 3 -t 30 192.168.10.10

# Bound by attempts instead of wall-clock: give up after 10 probes.
bide -c 3 -n 10 192.168.10.10

# Wait up to 60 s for a host to go silent (e.g. confirm a shutdown has taken).
bide --not -t 60 server01 && echo "server01 stopped responding"

# In a script: power-cycle a host and wait for it to come back up.
pwrctl off server01 && sleep 5 && pwrctl on server01 && \
  bide -t 120 server01.lan && \
  ssh server01 systemctl status my-service
```

## Planned probe modes (post-v1)

Documented here so v1 architecture accommodates them without CLI breakage.

- **TCP**: `bide --tcp 22 server01` — success = TCP handshake completes within `--interval`.
- **HTTP**: `bide --http /healthz server01` — success = 2xx response within `--interval`. Optional `--expect-status`.
- Probe-selection flags are mutually exclusive; default (no probe flag) remains ICMP.

Adding HTTP in particular may justify reintroducing a `--probe-timeout` flag later, since realistic HTTP health checks can legitimately take longer than a sensible ICMP/TCP interval. If that happens, `--probe-timeout` becomes optional and defaults to `--interval` — preserving v1 semantics.

## Out of scope (v1)

- TCP / HTTP reachability checks (planned; see above).
- Re-resolving DNS on each attempt.
- Parallel checking of multiple hosts.
- JSON or other structured output.
- Configurable packet size, TTL, DSCP markings.
- Windows / macOS support (best-effort only; primary target is Linux).

## Open questions

- Should `-c 1` be explicitly supported as "single-success mode," or is that redundant with plain `ping -c 1 -W <sec>` (using ping's own per-packet-wait flag)? (Lean: support it for CLI consistency.)
- On platforms without unprivileged ICMP, should the tool fail cleanly or attempt to use `CAP_NET_RAW`-granted capability automatically? (Lean: require `CAP_NET_RAW` or unprivileged ICMP; fail otherwise, with a clear message pointing to `setcap` or `ping_group_range`.)
- Should `--interval 0` be allowed (back-to-back probes)? (Lean: no, reject with exit 2 — this is a "wait" tool, not a flooder.)
- For TCP/HTTP modes, should the probe selection be a flag (`--tcp 22`) or a subcommand (`bide tcp 22 ...`)? (Lean: flag, to keep the invocation flat and the default case unchanged.)
