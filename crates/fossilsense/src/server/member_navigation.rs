//! Protocol adaptation of the shared owner/member resolver.
use super::*;
use crate::candidate_service::member_resolution::{
    MemberPolicy, MemberResolutionService, MemberRootQueryContext, OwnerMemberRef,
};
use crate::model;
use tower_lsp::lsp_types::{HoverContents, MarkupContent, MarkupKind, Position, Range};

pub(super) enum MemberTargetResolution {
    Found(Vec<OwnerMemberRef>),
    Unresolved { reason: query::BindingReason },
    Unsupported,
    Failed(String),
}

impl MemberTargetResolution {
    #[cfg(test)]
    fn into_members(self) -> Vec<OwnerMemberRef> {
        match self {
            Self::Found(members) => members,
            Self::Unresolved { .. } | Self::Unsupported | Self::Failed(_) => Vec::new(),
        }
    }
}

impl Backend {
    pub(super) async fn member_entity_locations(
        &self,
        session: &super::query_session::QuerySession,
        members: Vec<OwnerMemberRef>,
        declaration: bool,
    ) -> Vec<Location> {
        let fallback = locations(&members);
        if !members
            .iter()
            .any(|selected| selected.member.kind == parser::MemberKind::Method)
        {
            return fallback;
        }
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &session.root,
                session.context.engine.clone(),
                session.documents.clone(),
            )
            .await;
        let root = session.root.clone();
        let engine = session.context.engine.clone();
        let task_fallback = fallback.clone();
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Location>> {
            let declaration_read = engine.declaration_read_context()?;
            let family = members
                .first()
                .map(|member| member.family)
                .unwrap_or(crate::semantic_model::SemanticFamily::CFamily);
            let service = crate::candidate_service::CandidateQueryService::new_for_family(
                declaration_read.as_ref(),
                &overlay,
                "",
                None,
                engine.reach_graph.as_deref(),
                family,
            );
            let subjects = service.member_entity_subjects(&members)?;
            if subjects.is_empty() {
                return Ok(task_fallback);
            }
            let related = service.entity_locations_at(&subjects, declaration, None)?;
            Ok(related
                .candidates()
                .iter()
                .filter_map(|candidate| candidate_to_location(&root, candidate))
                .collect())
        })
        .await;
        match result {
            Ok(Ok(locations)) => locations,
            Ok(Err(error)) => {
                self.client
                    .log_message(
                        tower_lsp::lsp_types::MessageType::ERROR,
                        format!("member navigation declaration read failed: {error:#}"),
                    )
                    .await;
                fallback
            }
            Err(error) => {
                self.client
                    .log_message(
                        tower_lsp::lsp_types::MessageType::ERROR,
                        format!("member navigation task failed: {error}"),
                    )
                    .await;
                fallback
            }
        }
    }

    pub(super) async fn member_hover(
        &self,
        members: Vec<OwnerMemberRef>,
        timer: &mut super::query_session::BindingTimer<'_>,
    ) -> Option<Hover> {
        let (result, hydration_us, render_us) = tokio::task::spawn_blocking(move || {
            let mut comments = Vec::new();
            let mut hydration_us = 0;
            let mut render_us = 0;
            for selected in members.iter().take(query::HOVER_CANDIDATE_LIMIT) {
                let member = &selected.member;
                let started = std::time::Instant::now();
                let source = selected
                    .captured_source
                    .clone()
                    .filter(|source| {
                        source.len() as u64 <= super::hover::HOVER_SOURCE_FILE_BYTE_LIMIT
                    })
                    .or_else(|| {
                        if selected.captured_source.is_some() {
                            None
                        } else {
                            super::hover::candidate_source_text_for_path(
                                &selected.root,
                                "",
                                "",
                                &member.owner_path,
                                if Path::new(&member.owner_path).is_absolute() {
                                    "external"
                                } else {
                                    "workspace"
                                },
                            )
                            .map(Arc::<str>::from)
                        }
                    })
                    .filter(|source| {
                        member
                            .owner_revision_hash
                            .as_deref()
                            .is_some_and(|expected| {
                                blake3::hash(source.as_bytes()).to_hex().as_str() == expected
                            })
                    });
                hydration_us += started.elapsed().as_micros();
                let started = std::time::Instant::now();
                comments.push(source.and_then(|source| {
                    query::comment_documentation_for_candidate_symbol(
                        &source,
                        &member.name,
                        member.handle.start_line,
                        &model::CandidateRange {
                            start_line: member.handle.start_line,
                            start_col: member.handle.start_col,
                            end_line: member.handle.end_line,
                            end_col: member.handle.end_col,
                        },
                    )
                    .map(|comment| comment.markdown)
                }));
                render_us += started.elapsed().as_micros();
            }
            let started = std::time::Instant::now();
            let result = hover(&members, &comments);
            render_us += started.elapsed().as_micros();
            (result, hydration_us, render_us)
        })
        .await
        .ok()?;
        timer.observation.hydration_us += hydration_us;
        timer.observation.render_us += render_us;
        result
    }

    pub(super) async fn resolve_bound_members(
        &self,
        session: &super::query_session::QuerySession,
        uri: &Url,
        document: (i32, Arc<str>),
        syntax: &parser::CursorSyntax,
        word: &str,
        timer: &mut super::query_session::BindingTimer<'_>,
    ) -> MemberTargetResolution {
        let (version, text) = document;
        let Some(path) = uri_to_path(uri) else {
            return MemberTargetResolution::Failed("member URI is not a file path".into());
        };
        let selection = session
            .context
            .engine
            .workspace_semantics
            .selection_for_uri(uri, &text);
        let identity_path = if selection.language == crate::config::SourceLanguage::Go {
            PathBuf::from(pathing::relative_slash_path(&session.root, &path).unwrap_or_default())
        } else {
            path.clone()
        };
        let started = std::time::Instant::now();
        let Some(parsed) = self
            .get_or_parse_captured_document_with_selection(
                uri,
                &identity_path,
                version,
                &text,
                parser::ParseFacts::MEMBER
                    | parser::ParseFacts::LOCAL_DECLS
                    | parser::ParseFacts::CURSOR,
                selection,
            )
            .await
        else {
            return MemberTargetResolution::Failed("member document parse unavailable".into());
        };
        timer.observation.parse_us += started.elapsed().as_micros();
        let root = session.root.clone();
        let current_path = pathing::relative_slash_path(&root, &path).unwrap_or_default();
        let started = std::time::Instant::now();
        let overlay = self
            .candidate_overlay_snapshot_from_documents(
                &root,
                session.context.engine.clone(),
                session.documents.clone(),
            )
            .await;
        timer.observation.overlay_us += started.elapsed().as_micros();
        let engine = session.context.engine.clone();
        let expected_generation = engine.semantic_generation;
        let syntax = syntax.clone();
        let word = word.to_owned();
        let reads = timer.reads.clone();
        let started = std::time::Instant::now();
        let result = tokio::task::spawn_blocking(
            move || -> anyhow::Result<query::BindingResolution<OwnerMemberRef>> {
                let _reads = crate::call_service::ReadSessionProbe::enter(reads);
                let mut contexts = HashMap::new();
                contexts.insert(
                    root.clone(),
                    MemberRootQueryContext {
                        declaration_read: engine.declaration_read_context()?.map(Arc::new),
                        overlay,
                        current_path: current_path.clone(),
                        reach_graph: engine.reach_graph.clone(),
                        semantic_generation: engine.semantic_generation,
                        semantic_family: selection.semantic_family(),
                    },
                );
                let roots = [root.clone()];
                let mut resolver =
                    MemberResolutionService::new(&roots, &contexts, MemberPolicy::BoundNavigation);
                let declaration = parsed.members.iter().find(|member| {
                    member.name == word
                        && member.start_byte <= syntax.start_byte
                        && syntax.end_byte <= member.end_byte
                });
                let (owners, chain) = if let Some(member) = declaration {
                    let Some(record) = parsed
                        .records
                        .iter()
                        .find(|record| record.record_key == member.record_key)
                    else {
                        return Ok(query::BindingResolution::UnresolvedWithinDomain {
                            domain: parser::LookupDomain::Member,
                            reason: query::BindingReason::DedicatedDomain,
                        });
                    };
                    (
                        vec![(
                            root.clone(),
                            vec![query::RecordCandidate::from_overlay(
                                current_path.clone(),
                                record.clone(),
                                model::ScopeTier::Current,
                            )],
                        )],
                        Vec::new(),
                    )
                } else if let Some(owner) = syntax.owner_type.as_ref() {
                    let resolved = resolver.resolve_owner_spelling(
                        owner,
                        parsed.language == crate::semantic_model::SemanticLanguage::C,
                    )?;
                    if resolved.budget_exhausted {
                        return Ok(query::BindingResolution::Unsupported {
                            domain_hint: parser::LookupDomain::Member,
                        });
                    }
                    (resolved.candidates, Vec::new())
                } else {
                    let end = super::navigation::source_position_for_byte(&text, syntax.end_byte);
                    let line = text.lines().nth(end.line as usize).unwrap_or_default();
                    let Some(chain) = query::member_access_chain_at(line, end.character) else {
                        return Ok(query::BindingResolution::Unsupported {
                            domain_hint: parser::LookupDomain::Member,
                        });
                    };
                    if chain.has_subscript {
                        return Ok(query::BindingResolution::Unsupported {
                            domain_hint: parser::LookupDomain::Member,
                        });
                    }
                    let resolved =
                        resolver.resolve_receiver(&parsed, &chain.receiver, syntax.start_byte)?;
                    if resolved.budget_exhausted {
                        return Ok(query::BindingResolution::Unsupported {
                            domain_hint: parser::LookupDomain::Member,
                        });
                    }
                    (resolved.candidates, chain.completed_members)
                };
                resolver.resolve_selected(owners, &chain, &word)
            },
        )
        .await;
        timer.observation.query_us += started.elapsed().as_micros();
        match result {
            Ok(Ok(query::BindingResolution::Resolved(member))) => {
                let members = vec![member]
                    .into_iter()
                    .filter(|selected| {
                        selected.generation == expected_generation
                            && selected.family == selection.semantic_family()
                    })
                    .collect::<Vec<_>>();
                if members.is_empty() {
                    MemberTargetResolution::Failed("member target snapshot changed".into())
                } else {
                    MemberTargetResolution::Found(members)
                }
            }
            Ok(Ok(query::BindingResolution::Ambiguous(members))) => {
                let members = members
                    .into_iter()
                    .filter(|selected| {
                        selected.generation == expected_generation
                            && selected.family == selection.semantic_family()
                    })
                    .collect::<Vec<_>>();
                if members.is_empty() {
                    MemberTargetResolution::Failed("member target snapshot changed".into())
                } else {
                    MemberTargetResolution::Found(members)
                }
            }
            Ok(Ok(query::BindingResolution::UnresolvedWithinDomain { reason, .. })) => {
                MemberTargetResolution::Unresolved { reason }
            }
            Ok(Ok(query::BindingResolution::Unsupported { .. })) => {
                MemberTargetResolution::Unsupported
            }
            Ok(Err(error)) => MemberTargetResolution::Failed(format!(
                "member target resolution failed: {error:#}"
            )),
            Err(error) => MemberTargetResolution::Failed(format!("member task failed: {error}")),
        }
    }

    #[cfg(test)]
    pub(super) async fn bound_members(
        &self,
        session: &super::query_session::QuerySession,
        uri: &Url,
        document: (i32, Arc<str>),
        syntax: &parser::CursorSyntax,
        word: &str,
        timer: &mut super::query_session::BindingTimer<'_>,
    ) -> Vec<OwnerMemberRef> {
        self.resolve_bound_members(session, uri, document, syntax, word, timer)
            .await
            .into_members()
    }
}

pub(super) fn locations(members: &[OwnerMemberRef]) -> Vec<Location> {
    members
        .iter()
        .filter_map(|selected| {
            let member = &selected.member;
            let uri = Url::from_file_path(selected.root.join(&member.owner_path)).ok()?;
            Some(Location {
                uri,
                range: Range {
                    start: Position::new(member.handle.start_line, member.handle.start_col),
                    end: Position::new(member.handle.end_line, member.handle.end_col),
                },
            })
        })
        .collect()
}

fn hover(members: &[OwnerMemberRef], comments: &[Option<String>]) -> Option<Hover> {
    if members.is_empty() {
        return None;
    }
    let mut text = String::new();
    for (index, selected) in members
        .iter()
        .take(query::HOVER_CANDIDATE_LIMIT)
        .enumerate()
    {
        let member = &selected.member;
        use std::fmt::Write;
        let _ = writeln!(
            text,
            "```\n{}\n```\n{}:{}\n\nowner_member_candidate; confidence={:?}; ambiguous={}",
            member.signature,
            member.owner_path,
            member.handle.start_line + 1,
            member.confidence,
            members.len() > 1
        );
        if let Some(Some(comment)) = comments.get(index) {
            text.push_str("\n\n");
            text.push_str(comment);
            text.push('\n');
        }
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text,
        }),
        range: None,
    })
}
