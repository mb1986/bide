use clap::Parser;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "bide",
    version,
    about = "Block until a target reaches a stable probe state.",
    long_about = "bide sends one probe per --interval tick and exits 0 as soon as \
                  --stable consecutive probes match. Any mismatch resets the streak. \
                  Exits 1 if --timeout elapses first."
)]
pub struct Cli {
    /// Time between probe attempts. Bare numbers are seconds; suffixes: ms, s, m, h.
    #[arg(short = 'i', long, default_value = "3s", value_name = "DURATION", value_parser = parse_duration_arg)]
    pub interval: Duration,

    /// Number of consecutive matching probes required.
    #[arg(short = 's', long, default_value_t = 3, value_name = "N")]
    pub stable: u32,

    /// Maximum total probe attempts. 0 means no limit.
    #[arg(short = 'n', long = "max-tries", default_value_t = 0, value_name = "N")]
    pub max_tries: u32,

    /// Overall deadline. Bare numbers are seconds; suffixes: ms, s, m, h. 0 means no deadline.
    #[arg(short = 't', long, default_value = "0", value_name = "DURATION", value_parser = parse_duration_arg)]
    pub timeout: Duration,

    /// Wait for the target to stop responding instead.
    #[arg(long = "down")]
    pub down: bool,

    /// Suppress progress output; only exit code matters.
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Print each attempt and its result.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Target or URL. Plain targets and icmp:// URLs use ICMP.
    pub target: String,
}

pub(crate) fn parse_duration_arg(raw: &str) -> Result<Duration, String> {
    if raw.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    if raw.starts_with('-') {
        return Err("duration cannot be negative".to_string());
    }

    let (digits, unit) = if let Some(n) = raw.strip_suffix("ms") {
        (n, "ms")
    } else if let Some(n) = raw.strip_suffix('s') {
        (n, "s")
    } else if let Some(n) = raw.strip_suffix('m') {
        (n, "m")
    } else if let Some(n) = raw.strip_suffix('h') {
        (n, "h")
    } else {
        (raw, "s")
    };

    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "invalid duration '{raw}'; use a whole number with optional ms, s, m, or h suffix"
        ));
    }

    let value = digits
        .parse::<u64>()
        .map_err(|_| format!("duration '{raw}' is too large"))?;

    match unit {
        "ms" => Ok(Duration::from_millis(value)),
        "s" => Ok(Duration::from_secs(value)),
        "m" => value
            .checked_mul(60)
            .map(Duration::from_secs)
            .ok_or_else(|| format!("duration '{raw}' is too large")),
        "h" => value
            .checked_mul(60 * 60)
            .map(Duration::from_secs)
            .ok_or_else(|| format!("duration '{raw}' is too large")),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_suffixes() {
        assert_eq!(parse_duration_arg("30").unwrap(), Duration::from_secs(30));
        assert_eq!(
            parse_duration_arg("500ms").unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(parse_duration_arg("3s").unwrap(), Duration::from_secs(3));
        assert_eq!(parse_duration_arg("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration_arg("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration_arg("0").unwrap(), Duration::ZERO);
    }

    #[test]
    fn rejects_invalid_durations() {
        for raw in ["", "-1s", "1.5s", "1d", "ms"] {
            assert!(parse_duration_arg(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn duration_suffixes_are_case_sensitive() {
        // FR-13 specifies lowercase suffixes only; lock that in.
        for raw in ["3S", "3MS", "2M", "1H"] {
            assert!(parse_duration_arg(raw).is_err(), "{raw}");
        }
    }
}
