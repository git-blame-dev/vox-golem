use reqwest::header::{HeaderMap, RETRY_AFTER};
use std::time::{Duration, SystemTime};

/// RFC 9110 §10.2.3. Ambiguous/malformed values are unknown, never a reset guess.
pub(super) fn parse(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let mut values = headers.get_all(RETRY_AFTER).iter();
    let value = values.next()?;
    if values.next().is_some() || value.as_bytes().len() > 64 {
        return None;
    }
    let text = value.to_str().ok()?.trim();
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        text.parse::<u64>().ok().map(Duration::from_secs)
    } else {
        httpdate::parse_http_date(text)
            .ok()
            .map(|date| date.duration_since(now).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_values_and_invalid_values() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(784111777);
        for (value, expected) in [
            ("60", Some(60)),
            ("0", Some(0)),
            ("18446744073709551615", Some(u64::MAX)),
            ("Sun, 06 Nov 1994 08:48:37 GMT", Some(0)),
            ("Sun, 06 Nov 1994 08:49:37 GMT", Some(0)),
            ("Sun, 06 Nov 1994 08:50:37 GMT", Some(60)),
            ("-1", None),
            ("1.5", None),
            ("60, 90", None),
            ("18446744073709551616", None),
            ("unknown", None),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(RETRY_AFTER, value.parse().unwrap());
            assert_eq!(
                parse(&headers, now).map(|value| value.as_secs()),
                expected,
                "{value}"
            );
        }
        let mut headers = HeaderMap::new();
        assert_eq!(parse(&headers, now), None);
        headers.append(RETRY_AFTER, "60".parse().unwrap());
        headers.append(RETRY_AFTER, "90".parse().unwrap());
        assert_eq!(parse(&headers, now), None);
        headers.clear();
        headers.insert(RETRY_AFTER, "1".repeat(65).parse().unwrap());
        assert_eq!(parse(&headers, now), None);
    }
}
