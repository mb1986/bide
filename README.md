# bide

Block until a target is *stable*, defined as N consecutive matching probes.

`bide` fills the niche between `ping -c N` (counts total replies, not consecutive replies) and TCP-only wait tools. It reads naturally in scripts:

```sh
bide -t 30s server01 && ssh server01 ...
```

## Install

```sh
cargo install bide
```

For a local checkout:

```sh
cargo install --path .
```

Or build a release binary:

```sh
cargo build --release
# binary at target/release/bide
```

Requires Linux with unprivileged ICMP enabled (the default on modern kernels). If opening the socket fails with `EACCES`, either widen `net.ipv4.ping_group_range` or grant the capability:

```sh
sudo setcap cap_net_raw+ep target/release/bide
```

## Usage

```text
bide [OPTIONS] <TARGET>
```

| Flag | Long form | Default | Meaning |
|------|-----------|---------|---------|
| `-i` | `--interval` | `3s` | Time between probe attempts. Bare numbers are seconds; suffixes: `ms`, `s`, `m`, `h`. |
| `-s` | `--stable` | `3` | Number of consecutive matching probes required. |
| `-n` | `--max-tries` | `0` | Maximum total probe attempts. `0` means no limit. |
| `-t` | `--timeout` | `0` | Overall deadline. `0` means no deadline. |
| - | `--down` | off | Wait for the target to stop responding. |
| `-q` | `--quiet` | off | Suppress progress output; only exit code matters. |
| `-v` | `--verbose` | off | Print each attempt with sequence number and RTT. |
| `-h` | `--help` | - | Show help. |
| `-V` | `--version` | - | Show version. |

`TARGET` may be a plain host, IP address, or explicit ICMP URL:

```text
server01
192.168.10.10
icmp://server01
icmp://[2001:db8::10]
```

Plain targets use ICMP. `tcp://`, `http://`, and `https://` target forms are reserved for future probe backends and currently return a usage error.

## How it works

Every `--interval`, one probe is sent. In normal mode, a reply builds the stable streak and a no-response resets it to 0. In `--down` mode, that rule is inverted: a no-response builds the stable streak and a reply resets it.

As soon as the streak reaches `--stable`, the tool exits `0`. If `--timeout` elapses or `--max-tries` is exhausted first, it exits `1`.

### Output

Default output goes to stderr and stays compact:

- Startup: `server01: waiting for 3 stable icmp replies every 3s`
- Progress: `server01: 1/3`
- Repeated mismatches: `server01: waiting ..........`
- Success: `server01: 3/3 ok`
- Deadline: `server01: deadline reached after 30s`
- Attempt cap: `server01: max tries reached after 10 attempts`

Waiting dots wrap after 50 dots. Before printing a progress or terminal line, `bide` finishes any active dot line with a newline.

Sample output:

```text
server01: waiting for 3 stable icmp replies every 3s
server01: 1/3
server01: 2/3
server01: waiting ...
server01: 1/3
server01: 2/3
server01: 3/3 ok
```

Verbose mode is line-oriented and includes attempt details. Quiet mode emits nothing during normal operation.

## Examples

```sh
# Wait forever until 3 ICMP replies in a row succeed, 3 s apart.
bide server01

# 5 s interval, 3 consecutive replies, 30 s overall deadline.
bide -i 5s -s 3 -t 30s 192.168.10.10

# Bare durations are seconds.
bide -i 5 -t 30 server01

# Bound by attempts instead of wall-clock: give up after 10 probes.
bide -s 3 -n 10 server01

# Wait up to 60 s for a host to stop responding.
bide --down -t 60s server01 && echo "server01 stopped responding"

# Power-cycle a host and wait for it to stabilize before SSHing in.
pwrctl off server01 && sleep 5 && pwrctl on server01 && \
  bide -t 2m server01.lan && \
  ssh server01 systemctl status my-service
```

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Required stable streak achieved. |
| `1` | `--timeout` reached or `--max-tries` exhausted without achieving the streak. |
| `2` | Invalid arguments, unsupported target scheme, or usage error. |
| `3` | Host resolution failed or fatal network error, such as missing ICMP permission. |
| `130` | Interrupted by SIGINT (Ctrl-C). |
| `143` | Terminated by SIGTERM. |

Progress and errors go to stderr; stdout is reserved for future machine-readable output.

## Probe Backends

Current builds ship with ICMP echo only. The target syntax reserves URL forms for future backends:

```text
tcp://server01:22
http://server01/healthz
https://server01/healthz
```

## Platform

Primary target is Linux. IPv4 and IPv6 are both supported; address family is inferred from the resolved address. macOS / Windows support is best-effort.
