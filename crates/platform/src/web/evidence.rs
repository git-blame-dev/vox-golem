use super::{classify_tool_error, public_url, same_public_url, Operation};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use voxgolem_core::search::*;

pub fn decode_result(
    provider: Provider,
    operation: &Operation,
    value: &Value,
    execution_id: &str,
    next_source: &mut usize,
) -> Result<ToolResult, Failure> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Failure::InvalidUpstream)?
        .as_millis();
    let kind = if operation.fetch() {
        EvidenceKind::PageRead
    } else {
        EvidenceKind::SearchExcerpt
    };
    let mut result = ToolResult::default();
    let mut add_source =
        |url: &str, title: &str, text: String, date: Option<String>| -> Result<bool, Failure> {
            let url = public_url(url).map_err(|_| Failure::InvalidUpstream)?;
            if let Operation::Fetch { urls, .. } = operation {
                if !urls
                    .iter()
                    .any(|requested| public_url(requested).is_ok_and(|requested| requested == url))
                {
                    return Err(Failure::InvalidUpstream);
                }
            }
            let truncated = text.len() > 12000;
            let text = truncate_utf8(&text, 12000);
            if operation.fetch() && !has_readable_text(&text) {
                result.failures.push(PageFailure {
                    url,
                    failure: Failure::InvalidUpstream,
                });
                return Ok(false);
            }
            *next_source += 1;
            let trimmed_title = title.trim();
            let title = truncate_utf8(
                if trimmed_title.is_empty() || trimmed_title.eq_ignore_ascii_case("n/a") {
                    &url
                } else {
                    trimmed_title
                },
                512,
            );
            result.sources.push(Source {
                id: format!("{execution_id}-s{next_source}"),
                url,
                title,
                provider,
                kind,
                text,
                retrieved_unix_ms: now,
                publisher_date: date,
                truncated,
            });
            Ok(true)
        };
    match provider {
        Provider::Parallel => {
            let structured = value
                .get("structuredContent")
                .ok_or(Failure::InvalidUpstream)?;
            let items = structured["results"]
                .as_array()
                .ok_or(Failure::InvalidUpstream)?;
            let max = match operation {
                Operation::Search { limit, .. } => *limit,
                _ => 3,
            };
            for item in items.iter().take(max) {
                let url = item["url"].as_str().ok_or(Failure::InvalidUpstream)?;
                let title = item["title"].as_str().unwrap_or("");
                let excerpts = item["excerpts"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .filter(|text| {
                        !operation.fetch() || has_readable_text(&truncate_utf8(text, 12000))
                    })
                    .or_else(|| {
                        item["full_content"]
                            .as_str()
                            .filter(|text| {
                                !operation.fetch() || has_readable_text(&truncate_utf8(text, 12000))
                            })
                            .map(str::to_string)
                    });
                let excerpts = match excerpts {
                    Some(text) => text,
                    None if operation.fetch() => String::new(),
                    None => return Err(Failure::InvalidUpstream),
                };
                add_source(
                    url,
                    title,
                    excerpts,
                    item["publish_date"].as_str().map(str::to_string),
                )?;
            }
            if let Some(errors) = structured["errors"].as_array() {
                for error in errors {
                    result.failures.push(PageFailure {
                        url: error["url"].as_str().unwrap_or("").into(),
                        failure: classify_tool_error(error),
                    });
                }
            }
            result.partial = !result.failures.is_empty();
        }
        Provider::Exa => {
            let content = value["content"]
                .as_array()
                .ok_or(Failure::InvalidUpstream)?;
            let blocks = content
                .iter()
                .filter(|part| part["type"] == "text")
                .filter_map(|part| part["text"].as_str())
                .filter(|text| !text.is_empty());
            let max = match operation {
                Operation::Search { limit, .. } => *limit,
                _ => 3,
            };
            let mut source_count = 0;
            let mut unparsed = Vec::new();
            for block in blocks {
                if !operation.fetch() && max > 1 && block.contains("\n\n---\n\n") {
                    if let Some(parts) = exa_split_strict_records(block) {
                        for part in parts {
                            if source_count < max {
                                let added = match exa_leading_record(part, false) {
                                    Some((url, title, body)) => {
                                        add_source(url, title, body.into(), None)?
                                    }
                                    None => false,
                                };
                                if added {
                                    source_count += 1;
                                    continue;
                                }
                            }
                            unparsed.push(part);
                        }
                        continue;
                    }
                }
                // Search prose separators may come from highlights, not provider-owned records.
                if source_count < max && (operation.fetch() || !block.contains("\n\n---\n\n")) {
                    let added = match exa_leading_record(block, operation.fetch()) {
                        Some((url, title, body)) => add_source(url, title, body.into(), None)?,
                        None => false,
                    };
                    if added {
                        source_count += 1;
                        continue;
                    }
                }
                unparsed.push(block);
            }
            if source_count == 0 && unparsed.is_empty() {
                return Err(Failure::InvalidUpstream);
            }
            result.evidence_unavailable = result.sources.is_empty();
            result.partial = !unparsed.is_empty();
            if !unparsed.is_empty() {
                let raw = truncate_utf8(&unparsed.join("\n\n"), 12000);
                if result.sources.is_empty() && is_exa_soft_rate_limit(&raw) {
                    if let Operation::Fetch { urls, .. } = operation {
                        for url in urls {
                            result.failures.push(PageFailure {
                                url: url.clone(),
                                failure: Failure::RateLimited,
                            });
                        }
                    } else {
                        result.failures.push(PageFailure {
                            url: String::new(),
                            failure: Failure::RateLimited,
                        });
                    }
                } else {
                    result.raw_text = Some(raw);
                }
            }
        }
    }
    result.partial |= result.sources.iter().any(|source| source.truncated);
    if let Operation::Fetch { urls, .. } = operation {
        result.evidence_unavailable = result.sources.is_empty();
        for url in urls {
            if !result
                .sources
                .iter()
                .any(|source| public_url(url).is_ok_and(|url| url == source.url))
                && !result
                    .failures
                    .iter()
                    .any(|failure| same_public_url(&failure.url, url))
            {
                result.failures.push(PageFailure {
                    url: url.clone(),
                    failure: Failure::InvalidUpstream,
                });
            }
        }
        result.partial |= !result.failures.is_empty();
    }
    Ok(result)
}

fn exa_has_strict_record_header(part: &str) -> bool {
    let Some(rest) = part.strip_prefix("Title:") else {
        return false;
    };
    let Some((_, rest)) = rest.split_once('\n') else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("URL:") else {
        return false;
    };
    let Some((_, rest)) = rest.split_once('\n') else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("Published:") else {
        return false;
    };
    let Some((_, rest)) = rest.split_once('\n') else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("Author:") else {
        return false;
    };
    let Some((_, rest)) = rest.split_once('\n') else {
        return false;
    };
    let (marker, _) = rest.split_once('\n').unwrap_or((rest, ""));
    matches!(marker, "Highlights:" | "Content:" | "Text:")
}

fn exa_split_strict_records(block: &str) -> Option<Vec<&str>> {
    if !exa_has_strict_record_header(block) {
        return None;
    }
    const DELIM: &str = "\n\n---\n\n";
    let mut records = Vec::new();
    let mut record_start = 0;
    let mut search_from = 0;
    while let Some(rel_pos) = block[search_from..].find(DELIM) {
        let delim_start = search_from + rel_pos;
        let after_start = delim_start + DELIM.len();
        let after = &block[after_start..];
        if exa_has_strict_record_header(after) {
            records.push(&block[record_start..delim_start]);
            record_start = after_start;
            search_from = after_start;
        } else if after.starts_with("Title:") || after.starts_with("URL:") {
            return None;
        } else {
            search_from = after_start;
        }
    }
    records.push(&block[record_start..]);
    Some(records)
}

fn exa_leading_record(block: &str, fetch: bool) -> Option<(&str, &str, &str)> {
    if fetch && block.starts_with("# ") {
        let (metadata, body) = block.split_once("\n\n")?;
        let mut lines = metadata.lines();
        let title = lines.next()?.strip_prefix("# ")?;
        let url = lines.next()?.strip_prefix("URL: ")?;
        return lines
            .all(|line| line.starts_with("Published:") || line.starts_with("Author:"))
            .then_some((url, title, body));
    }
    let (title, rest) = if let Some(rest) = block.strip_prefix("Title:") {
        let (title, rest) = rest.split_once('\n')?;
        (title.trim(), rest)
    } else {
        ("", block)
    };
    let (url, rest) = rest.strip_prefix("URL:")?.split_once('\n')?;
    let rest = if rest.starts_with("Published:") {
        rest.split_once('\n')?.1
    } else {
        rest
    };
    let rest = if rest.starts_with("Author:") {
        rest.split_once('\n')?.1
    } else {
        rest
    };
    let (marker, body) = rest.split_once('\n').unwrap_or((rest, ""));
    matches!(marker, "Highlights:" | "Content:" | "Text:").then_some((url.trim(), title, body))
}

fn is_exa_soft_rate_limit(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("exa's free mcp rate limit")
        || lower.contains("hit exa's free")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
}
