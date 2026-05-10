mod cli;
mod icmp;
mod probe;
mod scheduler;

use clap::Parser;
use cli::Cli;
use icmp::IcmpProbe;
use scheduler::{RunResult, Scheduler, Verbosity};
use std::net::{IpAddr, ToSocketAddrs};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let started_at = Instant::now();
    let cli = Cli::parse();

    if cli.interval == Duration::ZERO {
        eprintln!("bide: --interval must be > 0 (this is a wait tool, not a flooder)");
        return ExitCode::from(2);
    }
    if !fits_in_instant_clock(started_at, cli.interval) {
        eprintln!("bide: --interval is too large");
        return ExitCode::from(2);
    }
    if cli.timeout != Duration::ZERO && !fits_in_instant_clock(started_at, cli.timeout) {
        eprintln!("bide: --timeout is too large");
        return ExitCode::from(2);
    }
    if cli.stable == 0 {
        eprintln!("bide: --stable must be > 0");
        return ExitCode::from(2);
    }

    let target = match parse_target(&cli.target) {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("bide: {msg}");
            return ExitCode::from(2);
        }
    };

    let addr = match resolve(target.host()) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("bide: {msg}");
            return ExitCode::from(3);
        }
    };

    let interrupted = Arc::new(AtomicBool::new(false));
    let terminated = Arc::new(AtomicBool::new(false));
    // Double-signal escape: register the conditional shutdown FIRST so it observes the
    // flag BEFORE the flag-setter flips it. First signal: shutdown handler sees a clear
    // flag and does nothing; flag-setter then arms it. Second signal: shutdown handler
    // sees the armed flag and force-exits, in case the main loop is wedged.
    if let Err(e) = signal_hook::flag::register_conditional_shutdown(
        signal_hook::consts::SIGINT,
        130,
        Arc::clone(&interrupted),
    ) {
        eprintln!("bide: failed to install SIGINT handler: {e}");
        return ExitCode::from(3);
    }
    if let Err(e) =
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))
    {
        eprintln!("bide: failed to install SIGINT handler: {e}");
        return ExitCode::from(3);
    }
    if let Err(e) = signal_hook::flag::register_conditional_shutdown(
        signal_hook::consts::SIGTERM,
        143,
        Arc::clone(&terminated),
    ) {
        eprintln!("bide: failed to install SIGTERM handler: {e}");
        return ExitCode::from(3);
    }
    if let Err(e) =
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&terminated))
    {
        eprintln!("bide: failed to install SIGTERM handler: {e}");
        return ExitCode::from(3);
    }

    let probe = match IcmpProbe::new(addr) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bide: {}", e.message);
            return ExitCode::from(3);
        }
    };

    let verbosity = if cli.quiet {
        Verbosity::Quiet
    } else if cli.verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Default
    };

    let scheduler = Scheduler {
        probe,
        interval: cli.interval,
        stable: cli.stable,
        max_tries: if cli.max_tries == 0 {
            None
        } else {
            Some(cli.max_tries)
        },
        timeout: if cli.timeout == Duration::ZERO {
            None
        } else {
            Some(cli.timeout)
        },
        down: cli.down,
        verbosity,
        target_label: target.label().to_string(),
        started_at,
        interrupted,
        terminated,
    };

    match scheduler.run() {
        RunResult::Success => ExitCode::from(0),
        RunResult::Deadline | RunResult::MaxTriesReached => ExitCode::from(1),
        RunResult::Interrupted => ExitCode::from(130),
        RunResult::Terminated => ExitCode::from(143),
        RunResult::InvalidConfig(msg) => {
            eprintln!("bide: {msg}");
            ExitCode::from(2)
        }
        RunResult::ProbeError(e) => {
            eprintln!("bide: {}", e.message);
            ExitCode::from(3)
        }
    }
}

fn fits_in_instant_clock(base: Instant, duration: Duration) -> bool {
    base.checked_add(duration).is_some()
}

enum TargetSpec {
    Icmp { host: String, label: String },
}

impl TargetSpec {
    fn host(&self) -> &str {
        match self {
            TargetSpec::Icmp { host, .. } => host,
        }
    }

    fn label(&self) -> &str {
        match self {
            TargetSpec::Icmp { label, .. } => label,
        }
    }
}

fn parse_target(raw: &str) -> Result<TargetSpec, String> {
    if raw.is_empty() {
        return Err("target cannot be empty".to_string());
    }

    let Some((scheme, rest)) = raw.split_once("://") else {
        let host = normalize_host(raw)?;
        return Ok(TargetSpec::Icmp {
            host,
            label: raw.to_string(),
        });
    };

    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "icmp" => {
            let host = parse_url_host(rest)?;
            Ok(TargetSpec::Icmp {
                label: host.clone(),
                host,
            })
        }
        "tcp" | "http" | "https" => Err(format!(
            "{scheme}:// targets are reserved for a future probe backend and are not implemented yet"
        )),
        _ => Err(format!(
            "unsupported target scheme '{scheme}'; use a plain host or icmp://host"
        )),
    }
}

fn parse_url_host(rest: &str) -> Result<String, String> {
    if rest.is_empty() {
        return Err("target URL is missing a host".to_string());
    }
    if rest.contains('/') || rest.contains('?') || rest.contains('#') {
        return Err("icmp:// targets must not include a path, query, or fragment".to_string());
    }
    if rest.starts_with('[') {
        if !rest.ends_with(']') {
            return Err(
                "icmp:// IPv6 targets must be bracketed and must not include a port".to_string(),
            );
        }
    } else if rest.contains(':') {
        return Err("icmp:// targets must not include a port; use tcp:// for TCP probes when that backend is available".to_string());
    }
    normalize_host(rest)
}

fn normalize_host(raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err("target host cannot be empty".to_string());
    }
    if let Some(inner) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        if inner.is_empty() {
            return Err("target host cannot be empty".to_string());
        }
        return Ok(inner.to_string());
    }
    if raw.starts_with('[') || raw.ends_with(']') {
        return Err("IPv6 target brackets are malformed".to_string());
    }
    Ok(raw.to_string())
}

/// FR-7: resolve once at startup. Pick the first address returned.
fn resolve(host: &str) -> Result<IpAddr, String> {
    // `to_socket_addrs` on `(host, 0)` handles IPv4 literals, IPv6 literals, and hostnames.
    match (host, 0u16).to_socket_addrs() {
        Ok(mut iter) => match iter.next() {
            Some(sa) => Ok(sa.ip()),
            None => Err(format!("unable to resolve '{host}': no addresses returned")),
        },
        Err(e) => Err(format!("unable to resolve '{host}': {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_target_as_icmp() {
        let t = parse_target("server01").unwrap();
        assert_eq!(t.host(), "server01");
        assert_eq!(t.label(), "server01");
    }

    #[test]
    fn parses_explicit_icmp_target() {
        let t = parse_target("icmp://server01").unwrap();
        assert_eq!(t.host(), "server01");
        assert_eq!(t.label(), "server01");
    }

    #[test]
    fn strips_brackets_from_ipv6_host() {
        let t = parse_target("icmp://[::1]").unwrap();
        assert_eq!(t.host(), "::1");
    }

    #[test]
    fn rejects_unsupported_target_schemes() {
        assert!(parse_target("tcp://server01:22").is_err());
        assert!(parse_target("http://server01/healthz").is_err());
        assert!(parse_target("ftp://server01").is_err());
    }

    #[test]
    fn rejects_malformed_targets() {
        assert!(parse_target("").is_err());
        assert!(parse_target("icmp://").is_err());
        assert!(parse_target("icmp://server01/path").is_err());
        assert!(parse_target("icmp://server01:22").is_err());
        assert!(parse_target("icmp://[::1]:22").is_err());
        assert!(parse_target("icmp://[::1").is_err());
    }

    #[test]
    fn rejects_durations_that_do_not_fit_in_instant_clock() {
        assert!(!fits_in_instant_clock(
            Instant::now(),
            Duration::from_secs(u64::MAX)
        ));
    }
}
