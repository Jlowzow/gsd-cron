/// Parse an interval string like "2h", "30m", "1h30m", "90m" into minutes
pub fn parse_interval(s: &str) -> Result<u32, String> {
    let s = s.trim().to_lowercase();

    // Try combined first: "1h30m"
    let re = regex::Regex::new(r"^(\d+)h(\d+)m$").unwrap();
    if let Some(cap) = re.captures(&s) {
        let hours: u32 = cap[1].parse().map_err(|_| format!("Invalid interval: {}", s))?;
        let mins: u32 = cap[2].parse().map_err(|_| format!("Invalid interval: {}", s))?;
        return Ok(hours * 60 + mins);
    }

    // Try pure hours: "2h"
    if let Some(stripped) = s.strip_suffix('h') {
        if let Ok(hours) = stripped.parse::<u32>() {
            return Ok(hours * 60);
        }
    }

    // Try pure minutes: "90m"
    if let Some(stripped) = s.strip_suffix('m') {
        return stripped
            .parse::<u32>()
            .map_err(|_| format!("Invalid interval: {}", s));
    }

    // Try plain number as minutes
    s.parse::<u32>()
        .map_err(|_| format!("Invalid interval '{}'. Use formats like: 2h, 30m, 1h30m", s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_interval() {
        assert_eq!(parse_interval("2h").unwrap(), 120);
        assert_eq!(parse_interval("30m").unwrap(), 30);
        assert_eq!(parse_interval("1h30m").unwrap(), 90);
        assert_eq!(parse_interval("90").unwrap(), 90);
        assert!(parse_interval("abc").is_err());
    }
}
