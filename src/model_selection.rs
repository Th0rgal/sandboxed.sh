//! Picker policy, applied after discovery and harness/account eligibility.
//! Never mutate observations, exact execution IDs, or saved mission history.
use std::collections::BTreeMap;

#[derive(Debug, Eq, PartialEq)]
struct Release {
    family: String,
    version: Vec<u32>,
    alias: bool,
}

fn release(id: &str) -> Option<Release> {
    let id = id.to_ascii_lowercase();
    let (provider, raw) = id.rsplit_once('/').unwrap_or(("", &id));
    let normalized = raw.replace('.', "-");
    let tokens: Vec<&str> = normalized.split('-').collect();
    let vendor = *tokens.first()?;
    if !matches!(vendor, "claude" | "gpt" | "grok") {
        return None;
    }
    let mut version = Vec::new();
    let mut words = Vec::new();
    let mut alias = false;
    let mut dated = false;
    for part in tokens.iter().skip(1) {
        if *part == "latest" || (part.len() >= 4 && part.bytes().all(|b| b.is_ascii_digit())) {
            alias = true;
            dated |= part.bytes().all(|b| b.is_ascii_digit());
        } else if let Ok(n) = part.parse::<u32>() {
            if !dated {
                version.push(n);
            }
        } else {
            words.push(*part);
        }
    }
    if version.is_empty() {
        return None; // Unversioned aliases cannot reliably be ranked.
    }
    let class = words.join("-");
    let known = match vendor {
        "claude" => matches!(class.as_str(), "opus" | "sonnet" | "haiku" | "fable"),
        "gpt" => matches!(
            class.as_str(),
            "" | "astra"
                | "sol"
                | "terra"
                | "luna"
                | "pro"
                | "mini"
                | "nano"
                | "codex"
                | "codex-mini"
                | "codex-max"
        ),
        "grok" => matches!(
            class.as_str(),
            "" | "build"
                | "reasoning"
                | "non-reasoning"
                | "fast"
                | "fast-reasoning"
                | "fast-non-reasoning"
                | "multi-agent"
        ),
        _ => false,
    };
    if !known {
        return None;
    }
    while version.len() > 1 && version.last() == Some(&0) {
        version.pop();
    }
    Some(Release {
        family: format!("{provider}/{vendor}/{class}"),
        version,
        alias,
    })
}

/// Keep the newest *available* release of each recognized class. Distinct
/// capabilities stay separate. Unknown/custom IDs are never guessed away.
/// Prefer canonical IDs over equivalent dated/latest aliases, then lexical ID
/// order, so source ordering cannot change the selected model.
pub fn preferred_indices(ids: &[&str]) -> Vec<usize> {
    let releases: Vec<_> = ids.iter().map(|id| release(id)).collect();
    let mut winners: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, release) in releases.iter().enumerate() {
        let Some(r) = release else { continue };
        winners
            .entry(&r.family)
            .and_modify(|winner| {
                let old = releases[*winner].as_ref().unwrap();
                if r.version > old.version
                    || (r.version == old.version
                        && ((!r.alias && old.alias)
                            || (r.alias == old.alias && ids[i] < ids[*winner])))
                {
                    *winner = i;
                }
            })
            .or_insert(i);
    }
    let mut kept: Vec<_> = releases
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            (r.as_ref().is_none_or(|r| winners[r.family.as_str()] == i)).then_some(i)
        })
        .collect();
    // First option is the default for clients without an explicit selection.
    kept.sort_by_key(|i| {
        !releases[*i]
            .as_ref()
            .is_some_and(|r| r.family.ends_with("/claude/opus"))
    });
    kept
}

/// Recognized versioned text models; unknown new capabilities need review.
pub fn is_versioned_text_model(id: &str, vendor: &str) -> bool {
    !id.contains('/') && id.starts_with(&format!("{vendor}-")) && release(id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pick<'a>(ids: &[&'a str]) -> Vec<&'a str> {
        preferred_indices(ids).into_iter().map(|i| ids[i]).collect()
    }
    #[test]
    fn latest_claude_classes_and_opus_default() {
        assert_eq!(
            pick(&[
                "claude-3-5-haiku-20241022",
                "claude-opus-5",
                "claude-sonnet-4-6",
                "claude-opus-5-5",
                "claude-sonnet-5",
                "claude-fable-5-1"
            ]),
            vec![
                "claude-opus-5-5",
                "claude-3-5-haiku-20241022",
                "claude-sonnet-5",
                "claude-fable-5-1"
            ]
        );
    }
    #[test]
    fn numeric_versions_aliases_and_future_discovery() {
        assert_eq!(
            pick(&[
                "gpt-5.9",
                "gpt-5.10",
                "gpt-6-sol",
                "gpt-5.6-sol",
                "gpt-6-astra",
                "grok-4.6-latest",
                "grok-4.6",
                "grok-4.5"
            ]),
            vec!["gpt-5.10", "gpt-6-sol", "gpt-6-astra", "grok-4.6"]
        );
        assert_eq!(
            pick(&["claude-opus-5-5", "claude-opus-6"]),
            vec!["claude-opus-6"]
        );
    }
    #[test]
    fn preserves_capabilities_unknown_ids_and_provider_boundaries() {
        let ids = [
            "gpt-6",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "private/model-v1",
            "grok-build-latest",
            "grok-4.20-0309-reasoning",
            "grok-4.20-0309-non-reasoning",
            "a/gpt-5",
            "b/gpt-6",
        ];
        assert_eq!(pick(&ids), ids);
    }
    #[test]
    fn aliases_are_deterministic_and_dates_are_not_versions() {
        let ids = [
            "claude-opus-5-5-20260926",
            "claude-opus-5.5",
            "claude-opus-5-5",
        ];
        assert_eq!(pick(&ids), vec!["claude-opus-5-5"]);
        assert_eq!(pick(&[ids[2], ids[1], ids[0]]), vec!["claude-opus-5-5"]);
        assert_eq!(pick(&["gpt-5.6-2026-09-26", "gpt-5.6"]), vec!["gpt-5.6"]);
    }

    #[test]
    fn available_subset_and_empty_are_safe() {
        assert_eq!(pick(&["claude-opus-5"]), vec!["claude-opus-5"]);
        assert!(pick(&[]).is_empty());
        assert!(!is_versioned_text_model("grok-imagine-image", "grok"));
        assert!(is_versioned_text_model("gpt-7-astra", "gpt"));
    }
}
