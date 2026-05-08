use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "bide",
    version,
    about = "Block until a host is stably reachable (N consecutive successful probes).",
    long_about = "bide sends one probe per --interval tick and exits 0 as soon as \
                  --count consecutive probes succeed. Any failure resets the streak. \
                  Exits 1 if --timeout elapses first."
)]
pub struct Cli {
    /// Seconds between probe attempts.
    #[arg(short = 'i', long, default_value_t = 3, value_name = "SECS")]
    pub interval: u64,

    /// Number of consecutive successful probes required.
    #[arg(short = 'c', long, default_value_t = 3, value_name = "N")]
    pub count: u32,

    /// Maximum total probe attempts. 0 means no limit.
    #[arg(short = 'n', long = "max-tries", default_value_t = 0, value_name = "N")]
    pub max_tries: u32,

    /// Overall deadline in seconds. 0 means no deadline.
    #[arg(short = 't', long, default_value_t = 0, value_name = "SECS")]
    pub timeout: u64,

    /// Invert the streak rule: wait for the host to stop responding instead.
    #[arg(long = "not")]
    pub not: bool,

    /// Suppress progress output; only exit code matters.
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Print each attempt and its result.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Target host (IPv4, IPv6, or hostname).
    pub host: String,
}
