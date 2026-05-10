use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Outcome of a single probe attempt.
pub enum ProbeOutcome {
    Success { rtt: Duration, seq: u16 },
    NoResponse,
}

/// A fatal error from the probe backend (e.g., permission denied on the socket).
/// Non-fatal conditions (host unreachable, timeout) are reported as `NoResponse`.
pub struct ProbeError {
    pub message: String,
}

/// A pluggable reachability probe. v1 ships only ICMP; TCP and HTTP are planned.
pub trait Probe {
    /// Resolved target address.
    fn target(&self) -> IpAddr;

    /// Short backend name, e.g. "icmp".
    fn name(&self) -> &'static str;

    /// Send one probe with the given sequence number. Wait at most until `deadline`.
    /// Returns `Success` if a matching reply arrives before `deadline`, otherwise
    /// `NoResponse`. Returns `ProbeError` only for fatal, unrecoverable issues.
    fn probe(&mut self, seq: u16, deadline: Instant) -> Result<ProbeOutcome, ProbeError>;
}
