# `bide` Requirements

A small Linux command-line tool that blocks until a target is *stable*, defined as N consecutive matching probes. It is intended for scripts that need to wait for a machine to come online after a reboot, power cycle, DHCP lease renewal, or similar operation.

It reads naturally in wait-until-ready scripts:

```sh
bide -t 30s server01 && ssh server01 ...
```

It also supports a tries-based cap (`--max-tries`) and a `--down` mode for waiting until a target stops responding.

## Rationale

Existing tools have gaps:

- `ping -c N` counts total replies, not consecutive replies.
- `ping -w` has an overall deadline but no stable-streak semantic.
- `wait-for-it` / `wait-for` test TCP ports, not ICMP, and have no "N consecutive" logic.
- `fping` reports per-packet results without a "settle until stable" mode.

`bide` fills the specific niche of "block my script until this target is reliably in the desired state."

## Synopsis

```text
bide [OPTIONS] <TARGET>
```

## Probe Model

`bide` is built around a pluggable probe concept: something that, given a target, returns success or failure within a bounded time. The scheduler handles timing, stable-streak tracking, deadlines, and exit codes. The probe backend only answers "did the target respond?"

Current builds ship with ICMP only. Plain targets default to ICMP, and `icmp://` is accepted as an explicit ICMP target. `tcp://`, `http://`, and `https://` target forms are reserved for future backends and currently return a usage error.

## Behavior

1. Every `--interval`, send one probe to `TARGET`.
2. A probe succeeds if a valid response arrives before the next tick is due; otherwise it fails. There is no separate per-probe timeout knob.
3. In normal mode, a successful probe builds the stable streak and a failed probe resets the streak to 0.
4. In `--down` mode, a failed probe builds the stable streak and a successful probe resets the streak to 0.
5. Exit `0` as soon as the streak reaches `--stable`.
6. If `--timeout` is set and the overall deadline is reached before the streak is achieved, exit `1`.
7. If `--max-tries` is set and that many probe attempts have completed before the streak is achieved, exit `1`.
8. If both `--timeout` and `--max-tries` are `0`, keep trying indefinitely.

`--down` is useful for scripts that must wait for a host to go offline, such as confirming that a shutdown completed. A no-response is still not proof of intentional shutdown; local routing, ARP, firewall, or network faults can produce the same observation.

## Command-Line Interface

| Flag | Long form | Default | Meaning |
|------|-----------|---------|---------|
| `-i` | `--interval` | `3s` | Time between probe attempts. Bare numbers are seconds; suffixes: `ms`, `s`, `m`, `h`. |
| `-s` | `--stable` | `3` | Number of consecutive matching probes required. |
| `-n` | `--max-tries` | `0` | Maximum total probe attempts. `0` means no limit. |
| `-t` | `--timeout` | `0` | Overall deadline. Bare numbers are seconds; suffixes: `ms`, `s`, `m`, `h`. `0` means no deadline. |
| - | `--down` | off | Wait for the target to stop responding. |
| `-q` | `--quiet` | off | Suppress progress output; only exit code matters. |
| `-v` | `--verbose` | off | Print each attempt and its result. |
| `-h` | `--help` | - | Show help. |
| `-V` | `--version` | - | Show version. |

Targets:

- `server01` means ICMP.
- `192.168.10.10` means ICMP.
- `icmp://server01` means ICMP.
- `icmp://[2001:db8::10]` means ICMP.
- `tcp://server01:22`, `http://server01/healthz`, and `https://server01/healthz` are reserved but not implemented yet.

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Required stable streak achieved. |
| `1` | Overall deadline (`--timeout`) reached, or max tries (`--max-tries`) exhausted, without achieving the streak. |
| `2` | Invalid arguments, unsupported target scheme, or usage error. |
| `3` | Host resolution failed or fatal network error unrelated to reachability, such as missing ICMP permission. |
| `130` | Interrupted by SIGINT. |
| `143` | Terminated by SIGTERM. |

## Functional Requirements

- **FR-1** Current builds use ICMP echo as the probe mechanism. The probe backend is abstracted behind an internal trait so TCP and HTTP backends can be added without altering the scheduler.
- **FR-2** Must send exactly one probe per interval tick. No bursts.
- **FR-3** Interval timing is measured from the start of each attempt, not from the end of the previous response.
- **FR-4** A mismatching probe resets the stable streak to 0, not 1.
- **FR-5** The overall deadline (`--timeout`) is measured from program start.
- **FR-6** If `--timeout` is non-zero and smaller than `--interval * --stable`, the tool must still start and fail when the deadline hits. The same applies if `--max-tries` is non-zero and smaller than `--stable`.
- **FR-7** Hostnames are resolved once at startup. If resolution fails, exit `3`.
- **FR-8** IPv4 and IPv6 are both supported. Address family is inferred from the resolved address.
- **FR-9** Must work without root on Linux kernels that support unprivileged ICMP sockets. Fall back to a clear error if neither unprivileged ICMP nor `CAP_NET_RAW` is available.
- **FR-10** The per-probe response wait is bounded by `--interval`.
- **FR-11** If `--max-tries` is non-zero, the tool exits `1` once that many probe attempts have completed without achieving the streak. Every attempt counts toward the limit.
- **FR-12** `--down` inverts the streak rule: no-response builds the streak, and a reply resets it.
- **FR-13** Bare duration values are seconds. Duration suffixes `ms`, `s`, `m`, and `h` are accepted. Fractional durations are rejected.
- **FR-14** `tcp://`, `http://`, and `https://` target forms are reserved and must return a usage error until implemented.

## Non-Functional Requirements

- **NFR-1** Single static binary, no runtime dependencies beyond libc.
- **NFR-2** Resident memory under 10 MB in steady state.
- **NFR-3** Clean shutdown on SIGINT and SIGTERM; no orphaned sockets.
- **NFR-4** Output to stderr for progress/errors; stdout reserved for future machine-readable output.
- **NFR-5** Deterministic behavior: given the same network conditions, the exit code and timing must be reproducible.

## Output

Default mode (neither `-q` nor `-v`) emits compact stderr progress:

- Startup: `server01: waiting for 3 stable icmp replies every 3s`
- Matching probe: `server01: N/M`
- Mismatching probes: `server01: waiting ..........`
- Final match: `server01: M/M ok`
- Deadline: `server01: deadline reached after Ns`
- Attempt cap: `server01: max tries reached after N attempts`

Waiting dots wrap after 50 dots. Any active dot line is finished before progress or terminal messages are printed.

Successful run:

```text
server01: waiting for 3 stable icmp replies every 3s
server01: 1/3
server01: 2/3
server01: waiting ...
server01: 1/3
server01: 2/3
server01: 3/3 ok
```

Failed run:

```text
server01: waiting for 3 stable icmp replies every 3s
server01: 1/3
server01: waiting .
server01: 1/3
server01: deadline reached after 30s
```

Verbose mode prints one line per attempt with sequence numbers, RTT where present, and reset reasons. Quiet mode emits nothing.

## Examples

```sh
# Default: wait forever until 3 ICMP replies in a row succeed, 3 s apart.
bide server01

# 5 s interval, 3 consecutive replies, 30 s overall deadline.
bide -i 5s -s 3 -t 30s 192.168.10.10

# Bound by attempts instead of wall-clock: give up after 10 probes.
bide -s 3 -n 10 server01

# Wait up to 60 s for a host to go silent.
bide --down -t 60s server01 && echo "server01 stopped responding"

# In a script: power-cycle a host and wait for it to come back up.
pwrctl off server01 && sleep 5 && pwrctl on server01 && \
  bide -t 2m server01.lan && \
  ssh server01 systemctl status my-service
```

## Planned Probe Modes

Documented here so the architecture and target syntax accommodate them without another CLI redesign:

- TCP: `bide tcp://server01:22`
- HTTP: `bide http://server01/healthz`
- HTTPS: `bide https://server01/healthz`

Adding HTTP may justify a future `--probe-timeout` flag. If added, it should default to `--interval` to preserve current semantics.

## Out Of Scope

- TCP / HTTP reachability checks in the current implementation.
- Re-resolving DNS on each attempt.
- Parallel checking of multiple targets.
- JSON or other structured output.
- Configurable packet size, TTL, DSCP markings.
- Windows / macOS support beyond best effort.
