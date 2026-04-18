# responds

Block until a host is *stably* reachable, defined as N consecutive successful probes.

`responds` fills the niche between `ping -c N` (counts total replies, not consecutive) and `wait-for-it` (TCP ports only, no streak semantic). It reads as a predicate at the call site:

```sh
responds -t 30 server01 && ssh server01 ...
```

## Install

```sh
cargo install --path .
```

Or build a release binary:

```sh
cargo build --release
# binary at target/release/responds
```

Requires Linux with unprivileged ICMP enabled (the default on modern kernels). If opening the socket fails with `EACCES`, either widen `net.ipv4.ping_group_range` or grant the capability:

```sh
sudo setcap cap_net_raw+ep target/release/responds
```

## Usage

```
responds [OPTIONS] <HOST>
```

| Flag | Long form     | Default | Meaning                                               |
|------|---------------|---------|-------------------------------------------------------|
| `-i` | `--interval`  | `3`     | Seconds between probe attempts.                       |
| `-c` | `--count`     | `3`     | Number of consecutive successful probes required.     |
| `-n` | `--max-tries` | `0`     | Maximum total probe attempts. `0` means no limit.     |
| `-t` | `--timeout`   | `0`     | Overall deadline in seconds. `0` means no deadline.   |
| `-q` | `--quiet`     | off     | Suppress progress output; only exit code matters.     |
| `-v` | `--verbose`   | off     | Print each attempt with sequence number and RTT.      |
| `-h` | `--help`      | —       | Show help.                                            |
| `-V` | `--version`   | —       | Show version.                                         |

`HOST` is an IPv4 address, IPv6 address, or hostname. DNS is resolved once at startup.

## How it works

Every `--interval` seconds, one probe is sent. A probe that does not answer before the next tick is a failure and **resets the streak to 0**. As soon as `--count` successes land in a row, the tool exits `0`. If `--timeout` elapses or `--max-tries` is exhausted first, it exits `1`.

Three different "give up" events are reported:

- `no response — streak reset` — a single probe timed out; the loop continues.
- `deadline reached after Ns` — overall `--timeout` hit; the tool exits `1`.
- `max tries reached after N attempts` — `--max-tries` exhausted; the tool exits `1`.

## Examples

```sh
# Wait forever until 3 pings in a row succeed, 3 s apart.
responds 192.168.10.10

# 5 s interval, 3 consecutive successes, 30 s overall deadline.
responds -i 5 -c 3 -t 30 192.168.10.10

# Bound by attempts instead of wall-clock: give up after 10 probes.
responds -c 3 -n 10 192.168.10.10

# Power-cycle a host and wait for it to stabilize before SSHing in.
pwrctl off server01 && sleep 5 && pwrctl on server01 && \
  responds -t 120 server01.lan && \
  ssh server01 systemctl status my-service
```

Sample output (host briefly flaps, then stabilizes):

```
192.168.10.10: 1/3
192.168.10.10: 2/3
192.168.10.10: no response — streak reset
192.168.10.10: 1/3
192.168.10.10: 2/3
192.168.10.10: 3/3 ok
```

## Exit codes

| Code  | Meaning                                                                    |
|-------|----------------------------------------------------------------------------|
| `0`   | Required consecutive successes achieved.                                   |
| `1`   | `--timeout` reached or `--max-tries` exhausted without achieving the streak. |
| `2`   | Invalid arguments or usage error.                                          |
| `3`   | Host resolution failed or fatal network error (e.g., no permission).       |
| `130` | Interrupted by SIGINT (Ctrl-C).                                            |

Progress and errors go to stderr; stdout is reserved for future machine-readable output.

## Probe backends

v1 ships with ICMP echo. The probe layer is abstracted behind an internal trait so TCP (`--tcp 22`) and HTTP (`--http /healthz`) backends can be added without breaking the default invocation. See [REQUIREMENTS.md](REQUIREMENTS.md).

## Platform

Primary target is Linux. IPv4 and IPv6 are both supported; address family is inferred from the resolved address. macOS / Windows support is best-effort.
