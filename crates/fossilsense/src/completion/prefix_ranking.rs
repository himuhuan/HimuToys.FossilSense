use std::cmp::Ordering;

/// Ordinary identifier completion policy for the relationship between name
/// match quality and semantic evidence. `Strict` makes an exact name or
/// literal prefix an outer ordering guard; `ScopeFirst` preserves the legacy
/// evidence-only order where a higher scope tier can outrank a better name
/// match. Underscores are ordinary literal identifier characters here: the
/// prefix `wns_ipc` does not strictly prefix `wns__ipc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CompletionPrefixRanking {
    #[default]
    Strict,
    ScopeFirst,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NameMatchClass {
    Fuzzy,
    Prefix,
    Exact,
}

fn name_match_class(prefix: &str, candidate: &str) -> NameMatchClass {
    if prefix.is_empty() {
        return NameMatchClass::Fuzzy;
    }
    if candidate.eq_ignore_ascii_case(prefix) {
        NameMatchClass::Exact
    } else if candidate
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    {
        NameMatchClass::Prefix
    } else {
        NameMatchClass::Fuzzy
    }
}

pub(super) fn compare_name_match(
    prefix: &str,
    policy: CompletionPrefixRanking,
    a: &str,
    b: &str,
) -> Ordering {
    if policy == CompletionPrefixRanking::ScopeFirst {
        Ordering::Equal
    } else {
        name_match_class(prefix, b).cmp(&name_match_class(prefix, a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_classes_are_case_insensitive_and_keep_underscores_literal() {
        assert_eq!(
            name_match_class("WNS_IPC", "wns_ipc"),
            NameMatchClass::Exact
        );
        assert_eq!(
            name_match_class("WNS_IPC", "wns_ipc_send"),
            NameMatchClass::Prefix
        );
        assert_eq!(
            name_match_class("WNS_IPC", "wns__ipc_rsp_init"),
            NameMatchClass::Fuzzy
        );
    }

    #[test]
    fn scope_first_leaves_name_match_order_neutral() {
        assert_eq!(
            compare_name_match(
                "wns_ipc",
                CompletionPrefixRanking::ScopeFirst,
                "wns__ipc_rsp_init",
                "wns_ipc_send",
            ),
            Ordering::Equal
        );
    }
}
