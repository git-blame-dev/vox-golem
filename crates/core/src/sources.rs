//! Canonical, bounded source provenance shared by conversation adapters.
use serde::{Deserialize, Serialize};
pub const MAX_SOURCES: usize = 32;
pub(crate) fn validate_source(source: &SourceLink) -> Result<(), SourceValidationError> {
    let remainder = source
        .url
        .strip_prefix("https://")
        .or_else(|| source.url.strip_prefix("http://"))
        .unwrap_or_default();
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if source.url.is_empty()
        || source.url.len() > 2048
        || source.title.trim().is_empty()
        || source.title.len() > 512
        || authority.is_empty()
        || authority.contains('@')
        || source
            .url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
        || !valid_http_authority(authority)
    {
        return Err(SourceValidationError::InvalidLink);
    }
    Ok(())
}
fn valid_http_authority(authority: &str) -> bool {
    if let Some(ipv6) = authority.strip_prefix('[') {
        let Some((host, port)) = ipv6.split_once(']') else {
            return false;
        };
        return host.parse::<std::net::Ipv6Addr>().is_ok() && valid_optional_port(port);
    }
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, ""), |(host, port)| (host, port));
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && (port.is_empty() || valid_port(port))
}
fn valid_optional_port(port: &str) -> bool {
    port.is_empty() || port.strip_prefix(':').is_some_and(valid_port)
}
fn valid_port(port: &str) -> bool {
    port.parse::<u16>().is_ok_and(|port| port != 0)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLink {
    pub url: String,
    pub title: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceValidationError {
    TooMany,
    InvalidLink,
    DuplicateUrl,
}
impl std::fmt::Display for SourceValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooMany => "too many sources",
            Self::InvalidLink => "invalid source provenance",
            Self::DuplicateUrl => "duplicate source URL",
        })
    }
}
impl std::error::Error for SourceValidationError {}
pub fn validate_sources(sources: &[SourceLink]) -> Result<(), SourceValidationError> {
    if sources.len() > MAX_SOURCES {
        return Err(SourceValidationError::TooMany);
    }
    for (i, source) in sources.iter().enumerate() {
        validate_source(source)?;
        if sources[..i]
            .iter()
            .any(|previous| previous.url == source.url)
        {
            return Err(SourceValidationError::DuplicateUrl);
        }
    }
    Ok(())
}
/// Retained canonical bytes, excluding any derived provider footer.
pub fn source_bytes(sources: &[SourceLink]) -> usize {
    sources
        .iter()
        .map(|source| source.url.len() + source.title.len())
        .sum()
}
/// Validate the entire input before preserving the first title and order per exact URL.
/// Once full, valid new URLs are ignored; invalid links still fail the whole merge.
pub fn merge_sources(
    current: &[SourceLink],
    incoming: &[SourceLink],
) -> Result<Vec<SourceLink>, SourceValidationError> {
    validate_sources(current)?;
    if incoming.len() > MAX_SOURCES {
        return Err(SourceValidationError::TooMany);
    }
    for source in incoming {
        validate_source(source)?;
    }
    let mut merged = current.to_vec();
    for source in incoming {
        if merged.len() < MAX_SOURCES && !merged.iter().any(|previous| previous.url == source.url) {
            merged.push(source.clone());
        }
    }
    Ok(merged)
}
/// Derived provider context only; never stored as the canonical answer body.
pub fn source_footer_bytes(sources: &[SourceLink]) -> usize {
    if sources.is_empty() {
        0
    } else {
        "\n\nSources:".len()
            + sources
                .iter()
                .map(|source| "\n- ".len() + source.url.len())
                .sum::<usize>()
    }
}
pub fn with_source_footer(text: &str, sources: &[SourceLink]) -> String {
    let mut result = String::with_capacity(text.len() + source_footer_bytes(sources));
    result.push_str(text);
    if !sources.is_empty() {
        result.push_str("\n\nSources:");
        for source in sources {
            result.push_str("\n- ");
            result.push_str(&source.url);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(n: usize) -> SourceLink {
        SourceLink {
            url: format!("https://example.test/{n}"),
            title: format!("Synthetic {n}"),
        }
    }
    #[test]
    fn canonical_validation_and_stable_bounded_merge() {
        let original = vec![source(0)];
        let mut renamed = source(0);
        renamed.title = "Later title".into();
        assert_eq!(
            merge_sources(&original, &[renamed.clone(), source(1)]).unwrap(),
            vec![source(0), source(1)]
        );
        assert_eq!(
            validate_sources(&[source(0), renamed]),
            Err(SourceValidationError::DuplicateUrl)
        );
        let full: Vec<_> = (0..32).map(source).collect();
        assert_eq!(merge_sources(&full, &[source(32)]).unwrap(), full);
        assert_eq!(
            validate_sources(&(0..33).map(source).collect::<Vec<_>>()),
            Err(SourceValidationError::TooMany)
        );
        let mut invalid = source(1);
        invalid.url = "https://user@example.test/path".into();
        assert!(merge_sources(&original, &[source(2), invalid]).is_err());
        for url in [
            "https://example.test/\r\nrole:system",
            "file:///synthetic",
            "https://example.test/ space",
        ] {
            let mut invalid = source(1);
            invalid.url = url.into();
            assert!(validate_sources(&[invalid]).is_err());
        }
        let mut invalid = source(1);
        invalid.url = format!("https://example.test/{}", "x".repeat(2048));
        assert!(validate_sources(&[invalid]).is_err());
        let mut invalid = source(1);
        invalid.title = "x".repeat(513);
        assert!(validate_sources(&[invalid]).is_err());
        let mut invalid = source(1);
        invalid.title = "  ".into();
        assert!(validate_sources(&[invalid]).is_err());
        assert_eq!(
            source_bytes(&original),
            original[0].url.len() + original[0].title.len()
        );
        assert_eq!(with_source_footer("raw", &[]), "raw");
        assert_eq!(
            with_source_footer("raw", &original).len(),
            3 + source_footer_bytes(&original)
        );
    }
}
