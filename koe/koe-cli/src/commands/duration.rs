//! Duration string parsing shared by `record` and `transcribe`.

use std::time::Duration;

/// Caps at ~100 years so `Instant::now() + duration` cannot panic.
const MAX_DURATION_SECS: u64 = 100 * 365 * 24 * 60 * 60;

/// Parses durations like `30s`, `5m`, `1h`, `2h30m`, `1h30m10s`.
///
/// Plain integers are seconds. Rejects empty, zero, and values larger than
/// ~100 years (so deadline math cannot overflow).
pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("duration is empty".into());
    }
    // Plain integer = seconds.
    if raw.chars().all(|c| c.is_ascii_digit()) {
        let secs: u64 = raw
            .parse()
            .map_err(|_| format!("invalid duration '{raw}'"))?;
        return checked_positive_duration(secs, raw);
    }

    let mut total_secs: u64 = 0;
    let mut number = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        if number.is_empty() {
            return Err(format!("invalid duration '{raw}'"));
        }
        let value: u64 = number
            .parse()
            .map_err(|_| format!("invalid duration '{raw}'"))?;
        number.clear();
        let factor = match ch {
            'h' | 'H' => 3600_u64,
            'm' | 'M' => 60,
            's' | 'S' => 1,
            _ => return Err(format!("invalid duration unit '{ch}' in '{raw}'")),
        };
        let part = value
            .checked_mul(factor)
            .ok_or_else(|| format!("duration '{raw}' is too large"))?;
        total_secs = total_secs
            .checked_add(part)
            .ok_or_else(|| format!("duration '{raw}' is too large"))?;
    }
    if !number.is_empty() {
        return Err(format!(
            "duration '{raw}' is missing a unit on trailing digits"
        ));
    }
    checked_positive_duration(total_secs, raw)
}

fn checked_positive_duration(
    secs: u64,
    raw: &str,
) -> Result<Duration, String> {
    if secs == 0 {
        return Err(format!("duration '{raw}' must be greater than zero"));
    }
    if secs > MAX_DURATION_SECS {
        return Err(format!("duration '{raw}' is too large"));
    }
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_compound() {
        assert_eq!(parse_duration("2h30m").unwrap(), Duration::from_mins(150));
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("1m30s").unwrap(), Duration::from_secs(90));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("0").is_err());
        assert!(parse_duration("999999999h").is_err());
    }
}
