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
use std::time::Duration;

fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.interval == 0 {
        eprintln!("bide: --interval must be > 0 (this is a wait tool, not a flooder)");
        return ExitCode::from(2);
    }
    if cli.count == 0 {
        eprintln!("bide: --count must be > 0");
        return ExitCode::from(2);
    }

    let addr = match resolve(&cli.host) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("bide: {}", msg);
            return ExitCode::from(3);
        }
    };

    let interrupted = Arc::new(AtomicBool::new(false));
    let terminated = Arc::new(AtomicBool::new(false));
    if let Err(e) =
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))
    {
        eprintln!("bide: failed to install SIGINT handler: {}", e);
        return ExitCode::from(3);
    }
    if let Err(e) =
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&terminated))
    {
        eprintln!("bide: failed to install SIGTERM handler: {}", e);
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
        interval: Duration::from_secs(cli.interval),
        count: cli.count,
        max_tries: if cli.max_tries == 0 {
            None
        } else {
            Some(cli.max_tries)
        },
        timeout: if cli.timeout == 0 {
            None
        } else {
            Some(Duration::from_secs(cli.timeout))
        },
        invert: cli.not,
        verbosity,
        interrupted,
        terminated,
    };

    match scheduler.run() {
        RunResult::Success => ExitCode::from(0),
        RunResult::Deadline | RunResult::MaxTriesReached => ExitCode::from(1),
        RunResult::Interrupted => ExitCode::from(130),
        RunResult::Terminated => ExitCode::from(143),
        RunResult::ProbeError(e) => {
            eprintln!("bide: {}", e.message);
            ExitCode::from(3)
        }
    }
}

/// FR-7: resolve once at startup. Pick the first address returned.
fn resolve(host: &str) -> Result<IpAddr, String> {
    // `to_socket_addrs` on `(host, 0)` handles IPv4 literals, IPv6 literals, and hostnames.
    match (host, 0u16).to_socket_addrs() {
        Ok(mut iter) => match iter.next() {
            Some(sa) => Ok(sa.ip()),
            None => Err(format!(
                "unable to resolve '{}': no addresses returned",
                host
            )),
        },
        Err(e) => Err(format!("unable to resolve '{}': {}", host, e)),
    }
}
