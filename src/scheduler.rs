use crate::probe::{Probe, ProbeError, ProbeOutcome};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum Verbosity {
    Quiet,
    Default,
    Verbose,
}

pub enum RunResult {
    Success,
    Deadline,
    MaxTriesReached,
    Interrupted,
    Terminated,
    ProbeError(ProbeError),
}

pub struct Scheduler<P: Probe> {
    pub probe: P,
    pub interval: Duration,
    pub count: u32,
    pub max_tries: Option<u32>,
    pub timeout: Option<Duration>,
    /// If true, invert the streak rule: a no-response builds the streak and a reply resets.
    pub invert: bool,
    pub verbosity: Verbosity,
    pub interrupted: Arc<AtomicBool>,
    pub terminated: Arc<AtomicBool>,
}

impl<P: Probe> Scheduler<P> {
    pub fn run(mut self) -> RunResult {
        let start = Instant::now();
        let deadline = self.timeout.map(|t| start + t);
        let mut streak: u32 = 0;
        let target = self.probe.target();

        if matches!(self.verbosity, Verbosity::Verbose) {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(
                err,
                "bide: probe={} target={} interval={}s count={} max-tries={} timeout={} mode={}",
                self.probe.name(),
                target,
                self.interval.as_secs(),
                self.count,
                match self.max_tries {
                    Some(n) => n.to_string(),
                    None => "none".to_string(),
                },
                match self.timeout {
                    Some(t) => format!("{}s", t.as_secs()),
                    None => "none".to_string(),
                },
                if self.invert { "not" } else { "default" }
            );
        }

        let mut tick: u32 = 0;
        let mut attempts: u32 = 0;
        loop {
            if let Some(r) = self.check_signals() {
                return r;
            }

            // FR-3: each tick starts at a fixed offset from program start.
            let tick_start = start + self.interval * tick;
            let now = Instant::now();
            if now < tick_start {
                let wait = tick_start - now;
                let bounded = match deadline {
                    Some(d) if d < tick_start => d.saturating_duration_since(now),
                    _ => wait,
                };
                if let Some(r) = self.interruptible_sleep(bounded) {
                    return r;
                }
            }

            // Deadline may have fired while we slept.
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    self.print_deadline(start);
                    return RunResult::Deadline;
                }
            }

            // FR-10: a probe attempt is bounded by the next tick, and also by the overall
            // deadline. Whichever is sooner caps the per-probe wait.
            let next_tick = start + self.interval * tick.saturating_add(1);
            let probe_deadline = match deadline {
                Some(d) if d < next_tick => d,
                _ => next_tick,
            };

            let seq = (tick & 0xFFFF) as u16;
            let outcome = match self.probe.probe(seq, probe_deadline) {
                Ok(o) => o,
                Err(e) => return RunResult::ProbeError(e),
            };
            // If a signal fired during the probe (EINTR turns into NoResponse), exit
            // before printing a misleading "streak reset" line.
            if let Some(r) = self.check_signals() {
                return r;
            }
            attempts = attempts.saturating_add(1);

            // FR-12: --not inverts which outcome builds the streak.
            let (builds_streak, streak_rtt, streak_seq) = match outcome {
                ProbeOutcome::Success { rtt, seq } => (!self.invert, Some(rtt), seq),
                ProbeOutcome::NoResponse => (self.invert, None, seq),
            };

            if builds_streak {
                streak += 1;
                if streak >= self.count {
                    self.print_final_ok(target, streak, streak_rtt, streak_seq);
                    return RunResult::Success;
                }
                self.print_progress(target, streak, streak_rtt, streak_seq);
            } else {
                // Streak-reset path. If the overall timeout fired while we were waiting,
                // report that instead of a streak reset.
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        self.print_deadline(start);
                        return RunResult::Deadline;
                    }
                }
                streak = 0;
                self.print_reset(target, streak_seq, streak_rtt);
            }

            // FR-11: attempts budget is checked after each probe, regardless of outcome.
            if let Some(max) = self.max_tries {
                if attempts >= max {
                    self.print_max_tries(attempts);
                    return RunResult::MaxTriesReached;
                }
            }

            tick = tick.saturating_add(1);
        }
    }

    fn check_signals(&self) -> Option<RunResult> {
        if self.interrupted.load(Ordering::SeqCst) {
            return Some(RunResult::Interrupted);
        }
        if self.terminated.load(Ordering::SeqCst) {
            return Some(RunResult::Terminated);
        }
        None
    }

    /// Sleep for `dur`, waking every ~100 ms to check signal flags so Ctrl-C is responsive.
    fn interruptible_sleep(&self, dur: Duration) -> Option<RunResult> {
        let end = Instant::now() + dur;
        let slice = Duration::from_millis(100);
        loop {
            if let Some(r) = self.check_signals() {
                return Some(r);
            }
            let now = Instant::now();
            if now >= end {
                return None;
            }
            let remaining = end - now;
            std::thread::sleep(remaining.min(slice));
        }
    }

    fn print_progress(
        &self,
        target: std::net::IpAddr,
        streak: u32,
        rtt: Option<Duration>,
        seq: u16,
    ) {
        match self.verbosity {
            Verbosity::Quiet => {}
            Verbosity::Default => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: {}/{}",
                    target,
                    streak,
                    self.count
                );
            }
            Verbosity::Verbose => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: {}/{} seq={} rtt={}",
                    target,
                    streak,
                    self.count,
                    seq,
                    format_rtt_opt(rtt)
                );
            }
        }
    }

    fn print_final_ok(
        &self,
        target: std::net::IpAddr,
        streak: u32,
        rtt: Option<Duration>,
        seq: u16,
    ) {
        match self.verbosity {
            Verbosity::Quiet => {}
            Verbosity::Default => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: {}/{} ok",
                    target,
                    streak,
                    self.count
                );
            }
            Verbosity::Verbose => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: {}/{} ok seq={} rtt={}",
                    target,
                    streak,
                    self.count,
                    seq,
                    format_rtt_opt(rtt)
                );
            }
        }
    }

    /// Print the "streak reset" line. Phrasing depends on mode: in default mode the
    /// event is a no-response; in `--not` mode the event is that the host replied.
    fn print_reset(&self, target: std::net::IpAddr, seq: u16, rtt: Option<Duration>) {
        let reason = if self.invert {
            "responded"
        } else {
            "no response"
        };
        match self.verbosity {
            Verbosity::Quiet => {}
            Verbosity::Default => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: {} \u{2014} streak reset",
                    target,
                    reason
                );
            }
            Verbosity::Verbose => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{}: {} seq={} rtt={} \u{2014} streak reset",
                    target,
                    reason,
                    seq,
                    format_rtt_opt(rtt)
                );
            }
        }
    }

    fn print_deadline(&self, start: Instant) {
        if matches!(self.verbosity, Verbosity::Quiet) {
            return;
        }
        let target = self.probe.target();
        let elapsed = start.elapsed().as_secs();
        let _ = writeln!(
            std::io::stderr().lock(),
            "{}: deadline reached after {}s",
            target,
            elapsed
        );
    }

    fn print_max_tries(&self, attempts: u32) {
        if matches!(self.verbosity, Verbosity::Quiet) {
            return;
        }
        let target = self.probe.target();
        let _ = writeln!(
            std::io::stderr().lock(),
            "{}: max tries reached after {} attempts",
            target,
            attempts
        );
    }
}

fn format_rtt_opt(rtt: Option<Duration>) -> String {
    match rtt {
        Some(rtt) => {
            let ms = rtt.as_secs_f64() * 1000.0;
            if ms < 1.0 {
                format!("{:.0}us", rtt.as_micros())
            } else {
                format!("{:.2}ms", ms)
            }
        }
        None => "-".to_string(),
    }
}
