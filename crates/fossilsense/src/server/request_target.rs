//! Request-local, protocol-neutral target selection shared by navigation and hover.

use super::*;
use crate::candidate_service::entities::EntitySubject;
use crate::candidate_service::member_resolution::OwnerMemberRef;

pub(super) struct LabelTarget {
    pub start_byte: usize,
    pub end_byte: usize,
}

pub(super) struct LocalTarget {
    pub parsed: Arc<FileSemanticIndex>,
    pub binding: query::LocalBindingRef,
}

pub(super) enum RequestTarget {
    Label(LabelTarget),
    Local(LocalTarget),
    Members(Vec<OwnerMemberRef>),
    Workspace(parser::CursorSyntax),
    Unavailable(TargetUnavailable),
}

pub(super) enum TargetUnavailable {
    MissingLabel,
    Unresolved {
        domain: parser::LookupDomain,
        reason: query::BindingReason,
    },
    Unsupported {
        domain: parser::LookupDomain,
    },
    Ambiguous {
        domain: parser::LookupDomain,
        candidate_count: usize,
    },
    Failed(String),
}

impl TargetUnavailable {
    pub fn diagnostic(&self) -> String {
        match self {
            Self::MissingLabel => "label target missing in proven label domain".into(),
            Self::Unresolved { domain, reason } => {
                format!("target unresolved in {domain:?}: {reason:?}")
            }
            Self::Unsupported { domain } => format!("target domain unsupported: {domain:?}"),
            Self::Ambiguous {
                domain,
                candidate_count,
            } => format!("target ambiguous in {domain:?}: {candidate_count} candidates"),
            Self::Failed(reason) => reason.clone(),
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

pub(super) struct WorkspaceTarget {
    pub semantic_set:
        crate::model::CandidateSet<crate::candidate_service::ResolvedDeclarationCandidate>,
    pub subjects: Vec<EntitySubject>,
    pub call_context: Option<crate::query::CallSiteContext>,
    pub origin_anchor: Option<crate::call_model::CallableAnchor>,
}

pub(super) struct RequestTargetInput<'a> {
    pub uri: &'a Url,
    pub document: (i32, Arc<str>),
    pub word: &'a str,
    pub cursor_byte: usize,
    pub source_language: SourceLanguage,
}

pub(super) fn resolve_workspace_target(
    service: &crate::candidate_service::CandidateQueryService<'_>,
    word: &str,
    syntax: &parser::CursorSyntax,
    position: crate::call_model::SourcePosition,
) -> Result<WorkspaceTarget> {
    let call_context = service.complete_call_context_at(position)?;
    let origin_anchor = service.anchor_at(position)?;
    let intent = if call_context.is_some() || origin_anchor.is_some() {
        crate::candidate_service::SemanticIntent::Call
    } else {
        crate::candidate_service::SemanticIntent::Neutral
    };
    let (semantic_set, subjects) = service.resolve_subject(
        word,
        intent,
        crate::candidate_service::LookupPolicy::BoundDomain {
            domain: syntax.domain,
            qualifier: syntax.qualifier.as_deref(),
        },
        call_context.clone(),
    )?;
    Ok(WorkspaceTarget {
        semantic_set,
        subjects,
        call_context,
        origin_anchor,
    })
}

impl Backend {
    pub(super) async fn resolve_request_target(
        &self,
        session: &query_session::QuerySession,
        input: RequestTargetInput<'_>,
        timer: &mut query_session::BindingTimer<'_>,
    ) -> RequestTarget {
        let RequestTargetInput {
            uri,
            document: (version, text),
            word,
            cursor_byte,
            source_language,
        } = input;
        if label_target_syntax_hint(&text, word, cursor_byte) {
            let label_started = std::time::Instant::now();
            let label_text = text.clone();
            let label_word = word.to_owned();
            let label_result = tokio::task::spawn_blocking(move || {
                label_target_byte_range(&label_text, &label_word, cursor_byte, source_language)
            })
            .await;
            timer.observation.parse_us += label_started.elapsed().as_micros();
            match label_result {
                Err(error) => {
                    return RequestTarget::Unavailable(TargetUnavailable::Failed(format!(
                        "label target task failed: {error}"
                    )));
                }
                Ok(resolution) => {
                    if let Some(target) = target_from_label_resolution(resolution) {
                        return target;
                    }
                }
            }
        }

        let started = std::time::Instant::now();
        let cursor_binding = session
            .bind_cursor(self, uri, (version, text.clone()), word, cursor_byte)
            .await;
        let binding_parse_us = started.elapsed().as_micros();
        let Some(cursor_binding) = cursor_binding else {
            timer.observation.parse_us += binding_parse_us;
            return RequestTarget::Unavailable(TargetUnavailable::Failed(
                "cursor target parse unavailable".into(),
            ));
        };
        timer.observation.parse_us += cursor_binding.parse_us;
        timer.observation.binding_us = cursor_binding.binding_us;
        timer.observation.cache_hit = cursor_binding.cache_hit;

        if cursor_binding.syntax.domain == parser::LookupDomain::Member {
            return match self
                .resolve_bound_members(
                    session,
                    uri,
                    (version, text),
                    &cursor_binding.syntax,
                    word,
                    timer,
                )
                .await
            {
                member_navigation::MemberTargetResolution::Found(members) => {
                    RequestTarget::Members(members)
                }
                member_navigation::MemberTargetResolution::Unresolved { reason } => {
                    RequestTarget::Unavailable(TargetUnavailable::Unresolved {
                        domain: parser::LookupDomain::Member,
                        reason,
                    })
                }
                member_navigation::MemberTargetResolution::Unsupported => {
                    RequestTarget::Unavailable(TargetUnavailable::Unsupported {
                        domain: parser::LookupDomain::Member,
                    })
                }
                member_navigation::MemberTargetResolution::Failed(reason) => {
                    RequestTarget::Unavailable(TargetUnavailable::Failed(reason))
                }
            };
        }

        match cursor_binding.resolution {
            query::BindingResolution::Resolved(binding) => RequestTarget::Local(LocalTarget {
                parsed: cursor_binding.parsed,
                binding,
            }),
            query::BindingResolution::UnresolvedWithinDomain {
                reason: query::BindingReason::NoLocalBinding,
                ..
            } => RequestTarget::Workspace(cursor_binding.syntax),
            query::BindingResolution::UnresolvedWithinDomain { domain, reason } => {
                RequestTarget::Unavailable(TargetUnavailable::Unresolved { domain, reason })
            }
            query::BindingResolution::Unsupported { domain_hint } => {
                RequestTarget::Unavailable(TargetUnavailable::Unsupported {
                    domain: domain_hint,
                })
            }
            query::BindingResolution::Ambiguous(bindings) => {
                RequestTarget::Unavailable(TargetUnavailable::Ambiguous {
                    domain: cursor_binding.syntax.domain,
                    candidate_count: bindings.len(),
                })
            }
        }
    }
}

fn target_from_label_resolution(
    resolution: LabelTargetResolution<(usize, usize)>,
) -> Option<RequestTarget> {
    match resolution {
        LabelTargetResolution::Found((start_byte, end_byte)) => {
            Some(RequestTarget::Label(LabelTarget {
                start_byte,
                end_byte,
            }))
        }
        LabelTargetResolution::MissingDefinition => {
            Some(RequestTarget::Unavailable(TargetUnavailable::MissingLabel))
        }
        LabelTargetResolution::Failed(reason) => Some(RequestTarget::Unavailable(
            TargetUnavailable::Failed(reason),
        )),
        LabelTargetResolution::NotLabelSyntax => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LabelTargetResolution<T> {
    NotLabelSyntax,
    MissingDefinition,
    Found(T),
    Failed(String),
}

pub(super) fn label_target_byte_range(
    text: &str,
    word: &str,
    cursor_byte: usize,
    language: SourceLanguage,
) -> LabelTargetResolution<(usize, usize)> {
    let Some((query_start, query_end)) = identifier_byte_range_at(text, cursor_byte) else {
        return LabelTargetResolution::NotLabelSyntax;
    };
    if text.get(query_start..query_end) != Some(word)
        || !label_target_syntax_hint_for_range(text, query_start, query_end)
    {
        return LabelTargetResolution::NotLabelSyntax;
    }

    let parser = parser::ParserHandle::new();
    let tree = match parser.parse_with_language(language.tree_sitter_language(), text, None) {
        Ok(Some(tree)) => tree,
        Ok(None) => {
            return LabelTargetResolution::Failed("label syntax parse unavailable".into());
        }
        Err(()) => {
            return LabelTargetResolution::Failed("label syntax parse failed".into());
        }
    };
    let Some(context) = label_context_node_at(tree.root_node(), query_start, query_end) else {
        return LabelTargetResolution::NotLabelSyntax;
    };
    let Some(scope) = enclosing_label_scope(context) else {
        return LabelTargetResolution::NotLabelSyntax;
    };

    let target = if context.kind() == "labeled_statement" {
        context.child_by_field_name("label")
    } else {
        label_definition_in_scope(scope, text, word)
    };
    target.map_or(LabelTargetResolution::MissingDefinition, |target| {
        LabelTargetResolution::Found((target.start_byte(), target.end_byte()))
    })
}

pub(super) fn label_target_syntax_hint(text: &str, word: &str, cursor_byte: usize) -> bool {
    let Some((start, end)) = identifier_byte_range_at(text, cursor_byte) else {
        return false;
    };
    text.get(start..end) == Some(word) && label_target_syntax_hint_for_range(text, start, end)
}

fn label_target_syntax_hint_for_range(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    let mut after = end;
    while bytes
        .get(after)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        after += 1;
    }
    if bytes.get(after) == Some(&b':') {
        return true;
    }

    let mut before = start;
    while before > 0 && bytes[before - 1].is_ascii_whitespace() {
        before -= 1;
    }
    let previous_end = before;
    while before > 0 && is_ascii_identifier_byte(bytes[before - 1]) {
        before -= 1;
    }
    text.get(before..previous_end) == Some("goto")
        && (before == 0 || !is_ascii_identifier_byte(bytes[before - 1]))
}

fn identifier_byte_range_at(text: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut anchor = cursor_byte.min(bytes.len());
    if anchor == bytes.len()
        || !bytes
            .get(anchor)
            .is_some_and(|byte| is_ascii_identifier_byte(*byte))
    {
        if anchor == 0 || !is_ascii_identifier_byte(bytes[anchor - 1]) {
            return None;
        }
        anchor -= 1;
    }

    let mut start = anchor;
    while start > 0 && is_ascii_identifier_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = anchor + 1;
    while end < bytes.len() && is_ascii_identifier_byte(bytes[end]) {
        end += 1;
    }
    (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_').then_some((start, end))
}

fn is_ascii_identifier_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn label_context_node_at<'tree>(
    root: tree_sitter::Node<'tree>,
    query_start: usize,
    query_end: usize,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_byte() > query_start || node.end_byte() < query_end {
            continue;
        }
        if matches!(node.kind(), "goto_statement" | "labeled_statement") {
            if let Some(label) = node.child_by_field_name("label") {
                if label.kind() == "statement_identifier"
                    && label.start_byte() == query_start
                    && label.end_byte() == query_end
                {
                    return Some(node);
                }
            }
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                stack.push(child);
            }
        }
    }
    None
}

fn enclosing_label_scope(context: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut current = context.parent();
    while let Some(node) = current {
        if is_label_scope_node(node) {
            return Some(node);
        }
        current = node.parent();
    }
    None
}

fn label_definition_in_scope<'tree>(
    scope: tree_sitter::Node<'tree>,
    text: &str,
    word: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if node.id() != scope.id() && is_label_scope_node(node) {
            continue;
        }
        if node.kind() == "labeled_statement" {
            if let Some(label) = node.child_by_field_name("label") {
                if label.kind() == "statement_identifier"
                    && text.get(label.start_byte()..label.end_byte()) == Some(word)
                {
                    return Some(label);
                }
            }
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                stack.push(child);
            }
        }
    }
    None
}

fn is_label_scope_node(node: tree_sitter::Node<'_>) -> bool {
    matches!(node.kind(), "function_definition" | "lambda_expression")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_label_target_stops_before_workspace_fallback() {
        let target = target_from_label_resolution(LabelTargetResolution::Failed(
            "injected label failure".into(),
        ))
        .expect("failed label probe must produce a terminal target result");
        assert!(matches!(
            target,
            RequestTarget::Unavailable(TargetUnavailable::Failed(reason))
                if reason == "injected label failure"
        ));
    }
}
