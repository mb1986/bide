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
    InvalidConfig(String),
    ProbeError(ProbeError),
}

pub struct Scheduler<P: Probe> {
    pub probe: P,
    pub interval: Duration,
    pub stable: u32,
    pub max_tries: Option<u32>,
    pub timeout: Option<Duration>,
    /// If true, a no-response builds the streak and a reply resets.
    pub down: bool,
    pub verbosity: Verbosity,
    pub target_label: String,
    pub started_at: Instant,
    pub interrupted: Arc<AtomicBool>,
    pub terminated: Arc<AtomicBool>,
}

impl<P: Probe> Scheduler<P> {
    pub fn run(mut self) -> RunResult {
        if self.interval == Duration::ZERO {
            return RunResult::InvalidConfig("--interval must be > 0".to_string());
        }

        let deadline_start = self.started_at;
        let deadline = match self.timeout {
            Some(timeout) => match deadline_start.checked_add(timeout) {
                Some(deadline) => Some(deadline),
                None => return RunResult::InvalidConfig("--timeout is too large".to_string()),
            },
            None => None,
        };
        let mut streak: u32 = 0;
        let mut output = OutputReporter::new(
            OutputConfig {
                verbosity: self.verbosity,
                target: self.target_label.clone(),
                resolved: self.probe.target(),
                probe: self.probe.name(),
                interval: self.interval,
                stable: self.stable,
                max_tries: self.max_tries,
                timeout: self.timeout,
                down: self.down,
            },
            std::io::stderr(),
        );
        output.print_startup();

        let mut tick: u32 = 0;
        let mut tick_start = Instant::now();
        let mut attempts: u32 = 0;
        loop {
            if let Some(r) = self.check_signals() {
                output.finish_dot_line();
                return r;
            }

            let now = Instant::now();
            while let Some(next) = tick_start.checked_add(self.interval) {
                if next > now {
                    break;
                }
                tick_start = next;
                tick = tick.saturating_add(1);
            }

            if now < tick_start {
                let wait = tick_start - now;
                let bounded = match deadline {
                    Some(d) if d < tick_start => d.saturating_duration_since(now),
                    _ => wait,
                };
                if let Some(r) = self.interruptible_sleep(bounded) {
                    output.finish_dot_line();
                    return r;
                }
            }

            // Deadline may have fired while we slept.
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    output.print_deadline(deadline_start);
                    return RunResult::Deadline;
                }
            }

            // FR-10: a probe attempt is bounded by the next tick, and also by the overall
            // deadline. Whichever is sooner caps the per-probe wait.
            let Some(next_tick) = tick_start.checked_add(self.interval) else {
                return RunResult::InvalidConfig("--interval is too large".to_string());
            };
            let probe_deadline = match deadline {
                Some(d) if d < next_tick => d,
                _ => next_tick,
            };

            let seq = (tick & 0xFFFF) as u16;
            let outcome = match self.probe.probe(seq, probe_deadline) {
                Ok(o) => o,
                Err(e) => {
                    output.finish_dot_line();
                    return RunResult::ProbeError(e);
                }
            };
            // If a signal fired during the probe (EINTR turns into NoResponse), exit
            // before printing a misleading reset marker.
            if let Some(r) = self.check_signals() {
                output.finish_dot_line();
                return r;
            }
            attempts = attempts.saturating_add(1);

            // --down inverts which outcome builds the streak.
            let (builds_streak, streak_rtt, streak_seq) = match outcome {
                ProbeOutcome::Success { rtt, seq } => (!self.down, Some(rtt), seq),
                ProbeOutcome::NoResponse => (self.down, None, seq),
            };

            if builds_streak {
                streak += 1;
                if streak >= self.stable {
                    output.print_final_ok(streak, streak_rtt, streak_seq);
                    return RunResult::Success;
                }
                output.print_progress(streak, streak_rtt, streak_seq);
            } else {
                // Reset path. If the overall timeout fired while we were waiting,
                // report that instead of a reset marker.
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        output.print_deadline(deadline_start);
                        return RunResult::Deadline;
                    }
                }
                streak = 0;
                output.print_reset(streak_seq, streak_rtt);
            }

            // FR-11: attempts budget is checked after each probe, regardless of outcome.
            if let Some(max) = self.max_tries {
                if attempts >= max {
                    output.print_max_tries(attempts);
                    return RunResult::MaxTriesReached;
                }
            }

            tick_start = next_tick;
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
}

struct OutputConfig {
    verbosity: Verbosity,
    target: String,
    resolved: std::net::IpAddr,
    probe: &'static str,
    interval: Duration,
    stable: u32,
    max_tries: Option<u32>,
    timeout: Option<Duration>,
    down: bool,
}

struct OutputReporter<W: Write> {
    config: OutputConfig,
    dots: u8,
    writer: W,
}

impl<W: Write> OutputReporter<W> {
    const DOTS_PER_LINE: u8 = 50;

    fn new(config: OutputConfig, writer: W) -> Self {
        Self {
            config,
            dots: 0,
            writer,
        }
    }

    fn print_startup(&mut self) {
        match self.config.verbosity {
            Verbosity::Quiet => {}
            Verbosity::Default => {
                let condition = if self.config.down {
                    "misses".to_string()
                } else {
                    format!("{} replies", self.config.probe)
                };
                let _ = writeln!(
                    &mut self.writer,
                    "{}: waiting for {} stable {} every {}",
                    self.config.target,
                    self.config.stable,
                    condition,
                    format_duration(self.config.interval)
                );
            }
            Verbosity::Verbose => {
                let _ = writeln!(
                    &mut self.writer,
                    "bide: probe={} target={} addr={} interval={} stable={} max-tries={} timeout={} mode={}",
                    self.config.probe,
                    self.config.target,
                    self.config.resolved,
                    format_duration(self.config.interval),
                    self.config.stable,
                    match self.config.max_tries {
                        Some(n) => n.to_string(),
                        None => "none".to_string(),
                    },
                    match self.config.timeout {
                        Some(t) => format_duration(t),
                        None => "none".to_string(),
                    },
                    if self.config.down { "down" } else { "up" }
                );
            }
        }
    }

    fn print_progress(&mut self, streak: u32, rtt: Option<Duration>, seq: u16) {
        match self.config.verbosity {
            Verbosity::Quiet => {}
            Verbosity::Default => {
                self.finish_dot_line();
                let _ = writeln!(
                    &mut self.writer,
                    "{}: {}/{}",
                    self.config.target, streak, self.config.stable
                );
            }
            Verbosity::Verbose => {
                let _ = writeln!(
                    &mut self.writer,
                    "{}: {}/{} seq={} rtt={}",
                    self.config.target,
                    streak,
                    self.config.stable,
                    seq,
                    format_rtt_opt(rtt)
                );
            }
        }
    }

    fn print_final_ok(&mut self, streak: u32, rtt: Option<Duration>, seq: u16) {
        match self.config.verbosity {
            Verbosity::Quiet => {}
            Verbosity::Default => {
                self.finish_dot_line();
                let _ = writeln!(
                    &mut self.writer,
                    "{}: {}/{} ok",
                    self.config.target, streak, self.config.stable
                );
            }
            Verbosity::Verbose => {
                let _ = writeln!(
                    &mut self.writer,
                    "{}: {}/{} ok seq={} rtt={}",
                    self.config.target,
                    streak,
                    self.config.stable,
                    seq,
                    format_rtt_opt(rtt)
                );
            }
        }
    }

    fn print_reset(&mut self, seq: u16, rtt: Option<Duration>) {
        let reason = if self.config.down {
            "responded"
        } else {
            "no response"
        };
        match self.config.verbosity {
            Verbosity::Quiet => {}
            Verbosity::Default => self.print_dot(),
            Verbosity::Verbose => {
                let _ = writeln!(
                    &mut self.writer,
                    "{}: {} seq={} rtt={} - streak reset",
                    self.config.target,
                    reason,
                    seq,
                    format_rtt_opt(rtt)
                );
            }
        }
    }

    fn print_deadline(&mut self, start: Instant) {
        if matches!(self.config.verbosity, Verbosity::Quiet) {
            return;
        }
        self.finish_dot_line();
        let elapsed = start.elapsed();
        let _ = writeln!(
            &mut self.writer,
            "{}: deadline reached after {}",
            self.config.target,
            format_duration(elapsed)
        );
    }

    fn print_max_tries(&mut self, attempts: u32) {
        if matches!(self.config.verbosity, Verbosity::Quiet) {
            return;
        }
        self.finish_dot_line();
        let _ = writeln!(
            &mut self.writer,
            "{}: max tries reached after {} attempts",
            self.config.target, attempts
        );
    }

    fn print_dot(&mut self) {
        if self.dots == 0 {
            let _ = write!(&mut self.writer, "{}: waiting ", self.config.target);
        }
        let _ = write!(&mut self.writer, ".");
        self.dots += 1;
        if self.dots >= Self::DOTS_PER_LINE {
            let _ = writeln!(&mut self.writer);
            self.dots = 0;
        }
        let _ = self.writer.flush();
    }

    fn finish_dot_line(&mut self) {
        if self.dots == 0 {
            return;
        }
        let _ = writeln!(&mut self.writer);
        self.dots = 0;
    }
}

fn format_rtt_opt(rtt: Option<Duration>) -> String {
    match rtt {
        Some(rtt) => {
            let ms = rtt.as_secs_f64() * 1000.0;
            if ms < 1.0 {
                format!("{:.0}us", rtt.as_micros())
            } else {
                format!("{ms:.2}ms")
            }
        }
        None => "-".to_string(),
    }
}

fn format_duration(duration: Duration) -> String {
    if duration == Duration::ZERO {
        return "0".to_string();
    }
    if duration.subsec_millis() != 0 || duration.as_secs() == 0 {
        return format!("{}ms", duration.as_millis());
    }

    let secs = duration.as_secs();
    if secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else if secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use std::sync::atomic::Ordering;

    #[test]
    fn formats_durations_for_display() {
        assert_eq!(format_duration(Duration::ZERO), "0");
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_secs(3)), "3s");
        assert_eq!(format_duration(Duration::from_secs(120)), "2m");
        assert_eq!(format_duration(Duration::from_secs(3600)), "1h");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
    }

    #[test]
    fn default_dots_wrap_and_finish_before_progress() {
        let mut output = OutputReporter::new(test_config(Verbosity::Default, false), Vec::new());

        for seq in 0..51 {
            output.print_reset(seq, None);
        }
        output.print_progress(1, None, 51);

        let text = String::from_utf8(output.writer).unwrap();
        assert_eq!(
            text,
            format!(
                "server01: waiting {}\nserver01: waiting .\nserver01: 1/3\n",
                ".".repeat(50)
            )
        );
    }

    #[test]
    fn default_deadline_uses_duration_formatting() {
        let mut output = OutputReporter::new(test_config(Verbosity::Default, false), Vec::new());

        output.print_reset(0, None);
        output.print_deadline(
            Instant::now()
                .checked_sub(Duration::from_millis(500))
                .unwrap(),
        );

        let text = String::from_utf8(output.writer).unwrap();
        assert!(text.starts_with("server01: waiting .\n"));
        assert!(text.contains("server01: deadline reached after "));
        assert!(text.contains("ms\n"));
        assert!(!text.contains("after 0s"));
    }

    #[test]
    fn verbose_down_reset_is_line_oriented() {
        let mut output = OutputReporter::new(test_config(Verbosity::Verbose, true), Vec::new());

        output.print_startup();
        output.print_reset(7, Some(Duration::from_millis(2)));

        let text = String::from_utf8(output.writer).unwrap();
        assert_eq!(
            text,
            "bide: probe=icmp target=server01 addr=192.0.2.1 interval=3s stable=3 max-tries=none timeout=none mode=down\nserver01: responded seq=7 rtt=2.00ms - streak reset\n"
        );
    }

    #[test]
    fn quiet_output_is_empty() {
        let mut output = OutputReporter::new(test_config(Verbosity::Quiet, false), Vec::new());

        output.print_startup();
        output.print_reset(1, None);
        output.print_progress(1, None, 1);
        output.print_final_ok(3, None, 3);
        output.print_deadline(Instant::now());
        output.print_max_tries(10);

        assert!(output.writer.is_empty());
    }

    #[test]
    fn down_mode_builds_streak_on_no_response() {
        let scheduler = Scheduler {
            probe: FakeProbe {
                outcomes: vec![ProbeOutcome::NoResponse, ProbeOutcome::NoResponse],
            },
            interval: Duration::from_millis(1),
            stable: 2,
            max_tries: Some(2),
            timeout: Some(Duration::from_secs(1)),
            down: true,
            verbosity: Verbosity::Quiet,
            target_label: "server01".to_string(),
            started_at: Instant::now(),
            interrupted: Arc::default(),
            terminated: Arc::default(),
        };

        assert!(matches!(scheduler.run(), RunResult::Success));
    }

    #[test]
    fn stable_one_succeeds_on_first_match() {
        let scheduler = Scheduler {
            probe: FakeProbe {
                outcomes: vec![ProbeOutcome::Success {
                    rtt: Duration::ZERO,
                    seq: 0,
                }],
            },
            interval: Duration::from_millis(1),
            stable: 1,
            max_tries: Some(1),
            timeout: Some(Duration::from_secs(1)),
            down: false,
            verbosity: Verbosity::Quiet,
            target_label: "server01".to_string(),
            started_at: Instant::now(),
            interrupted: Arc::default(),
            terminated: Arc::default(),
        };

        assert!(matches!(scheduler.run(), RunResult::Success));
    }

    #[test]
    fn down_mode_reply_resets_streak() {
        // Sequence under --down (NoResponse builds, Success resets):
        // miss -> 1, reply -> 0 (reset), miss -> 1, miss -> 2 -> Success at stable=2.
        let scheduler = Scheduler {
            probe: FakeProbe {
                outcomes: vec![
                    ProbeOutcome::NoResponse,
                    ProbeOutcome::Success {
                        rtt: Duration::ZERO,
                        seq: 1,
                    },
                    ProbeOutcome::NoResponse,
                    ProbeOutcome::NoResponse,
                ],
            },
            interval: Duration::from_millis(1),
            stable: 2,
            max_tries: Some(4),
            timeout: Some(Duration::from_secs(1)),
            down: true,
            verbosity: Verbosity::Quiet,
            target_label: "server01".to_string(),
            started_at: Instant::now(),
            interrupted: Arc::default(),
            terminated: Arc::default(),
        };

        assert!(matches!(scheduler.run(), RunResult::Success));
    }

    #[test]
    fn scheduler_does_not_replay_ticks_lost_before_run_starts() {
        let saw_expired_deadline = Arc::new(AtomicBool::new(false));
        let scheduler = Scheduler {
            probe: DeadlineProbe {
                saw_expired_deadline: Arc::clone(&saw_expired_deadline),
            },
            interval: Duration::from_millis(10),
            stable: 1,
            max_tries: Some(1),
            timeout: None,
            down: false,
            verbosity: Verbosity::Quiet,
            target_label: "server01".to_string(),
            started_at: Instant::now()
                .checked_sub(Duration::from_millis(100))
                .unwrap(),
            interrupted: Arc::default(),
            terminated: Arc::default(),
        };

        assert!(matches!(scheduler.run(), RunResult::Success));
        assert!(!saw_expired_deadline.load(Ordering::SeqCst));
    }

    fn test_config(verbosity: Verbosity, down: bool) -> OutputConfig {
        OutputConfig {
            verbosity,
            target: "server01".to_string(),
            resolved: IpAddr::from([192, 0, 2, 1]),
            probe: "icmp",
            interval: Duration::from_secs(3),
            stable: 3,
            max_tries: None,
            timeout: None,
            down,
        }
    }

    struct FakeProbe {
        outcomes: Vec<ProbeOutcome>,
    }

    impl Probe for FakeProbe {
        fn target(&self) -> IpAddr {
            IpAddr::from([192, 0, 2, 1])
        }

        fn name(&self) -> &'static str {
            "fake"
        }

        fn probe(&mut self, _seq: u16, _deadline: Instant) -> Result<ProbeOutcome, ProbeError> {
            Ok(self.outcomes.remove(0))
        }
    }

    struct DeadlineProbe {
        saw_expired_deadline: Arc<AtomicBool>,
    }

    impl Probe for DeadlineProbe {
        fn target(&self) -> IpAddr {
            IpAddr::from([192, 0, 2, 1])
        }

        fn name(&self) -> &'static str {
            "fake"
        }

        fn probe(&mut self, seq: u16, deadline: Instant) -> Result<ProbeOutcome, ProbeError> {
            if Instant::now() >= deadline {
                self.saw_expired_deadline.store(true, Ordering::SeqCst);
            }
            Ok(ProbeOutcome::Success {
                rtt: Duration::ZERO,
                seq,
            })
        }
    }
}
