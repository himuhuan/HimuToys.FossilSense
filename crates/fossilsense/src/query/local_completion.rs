use std::collections::HashSet;

use crate::model::ScopeTier;
use crate::parser::{LocalBinding, LocalBindingKind};
use crate::resolver;

use super::{byte_offset_at, completion_word_score};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCompletionCandidate {
    pub name: String,
    pub kind: LocalBindingKind,
    pub detail: String,
    pub score: i32,
    pub match_score: i32,
    pub decl_start_byte: usize,
}

pub fn local_completion_candidates(
    bindings: &[LocalBinding],
    text: &str,
    line: u32,
    character: u32,
    prefix: &str,
    limit: usize,
) -> Vec<LocalCompletionCandidate> {
    let byte_offset = byte_offset_at(text, line, character).min(text.len());
    let mut hits: Vec<LocalCompletionCandidate> = bindings
        .iter()
        .filter(|binding| {
            binding.function_start_byte < byte_offset
                && byte_offset <= binding.function_end_byte
                && binding.decl_start_byte < byte_offset
        })
        .filter_map(|binding| {
            let base_match = completion_word_score(prefix, &binding.name, 0)?;
            Some(LocalCompletionCandidate {
                name: binding.name.clone(),
                kind: binding.kind,
                detail: local_binding_detail(binding),
                score: resolver::pack_score(ScopeTier::Current, base_match, 0),
                match_score: base_match,
                decl_start_byte: binding.decl_start_byte,
            })
        })
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.decl_start_byte.cmp(&a.decl_start_byte))
            .then_with(|| a.name.cmp(&b.name))
    });
    dedup_by_name_keep_first(&mut hits);
    hits.truncate(limit);
    hits
}

fn local_binding_detail(binding: &LocalBinding) -> String {
    let role = match binding.kind {
        LocalBindingKind::Parameter => "parameter",
        LocalBindingKind::LocalVariable => "local",
    };
    match binding.type_text.as_deref() {
        Some(type_text) if !type_text.is_empty() => format!("{role}: {type_text}"),
        _ => role.to_string(),
    }
}

fn dedup_by_name_keep_first(hits: &mut Vec<LocalCompletionCandidate>) {
    let mut seen = HashSet::new();
    hits.retain(|hit| seen.insert(hit.name.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{LocalBinding, LocalBindingKind};

    fn binding(
        name: &str,
        kind: LocalBindingKind,
        decl_start_byte: usize,
        function_start_byte: usize,
        function_end_byte: usize,
    ) -> LocalBinding {
        LocalBinding {
            name: name.to_string(),
            kind,
            type_text: Some("int".to_string()),
            decl_start_byte,
            function_start_byte,
            function_end_byte,
        }
    }

    #[test]
    fn local_completion_keeps_parameters_and_prior_locals() {
        let text = "int f(int count) {\n    int cursor_limit;\n    cur\n}\n";
        let cursor = text.find("cur\n").expect("cursor");
        let bindings = vec![
            binding(
                "count",
                LocalBindingKind::Parameter,
                text.find("count").unwrap(),
                0,
                text.len(),
            ),
            binding(
                "cursor_limit",
                LocalBindingKind::LocalVariable,
                text.find("cursor_limit").unwrap(),
                0,
                text.len(),
            ),
        ];
        let hits = local_completion_candidates(&bindings, text, 2, 7, "cur", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "cursor_limit");
        assert!(hits[0].score > 0);
        assert!(bindings[1].decl_start_byte < cursor);
    }

    #[test]
    fn local_completion_excludes_later_declarations() {
        let text = "int f(void) {\n    fut\n    int future_value;\n}\n";
        let bindings = vec![binding(
            "future_value",
            LocalBindingKind::LocalVariable,
            text.find("future_value").unwrap(),
            0,
            text.len(),
        )];
        let hits = local_completion_candidates(&bindings, text, 1, 7, "fut", 10);
        assert!(hits.is_empty());
    }

    #[test]
    fn local_completion_requires_cursor_inside_function_range() {
        let text = "int f(void) { int local_value; }\nloc\n";
        let bindings = vec![binding(
            "local_value",
            LocalBindingKind::LocalVariable,
            18,
            0,
            31,
        )];
        let hits = local_completion_candidates(&bindings, text, 1, 3, "loc", 10);
        assert!(hits.is_empty());
    }

    #[test]
    fn parsed_local_completion_requires_cursor_inside_function_body() {
        let text = "int f(int count, int cou) {\n    cou\n}\n";
        let parsed = crate::parser::parse(std::path::Path::new("a.c"), text);

        let signature_hits =
            local_completion_candidates(&parsed.local_bindings, text, 0, 24, "cou", 10);
        assert!(
            signature_hits.is_empty(),
            "local bindings must not activate while editing the function signature"
        );

        let body_hits = local_completion_candidates(&parsed.local_bindings, text, 1, 7, "cou", 10);
        assert!(body_hits.iter().any(|hit| hit.name == "count"));
    }

    #[test]
    fn local_completion_preserves_short_prefix_noise_gate() {
        let text = "void f(void) {\n    int Foobar;\n    int FooBar;\n    ba\n}\n";
        let bindings = vec![
            binding(
                "Foobar",
                LocalBindingKind::LocalVariable,
                text.find("Foobar").unwrap(),
                0,
                text.len(),
            ),
            binding(
                "FooBar",
                LocalBindingKind::LocalVariable,
                text.find("FooBar").unwrap(),
                0,
                text.len(),
            ),
        ];
        let hits = local_completion_candidates(&bindings, text, 3, 6, "ba", 10);
        assert_eq!(
            hits.iter().map(|hit| hit.name.as_str()).collect::<Vec<_>>(),
            vec!["FooBar"]
        );
    }

    #[test]
    fn local_completion_keeps_nearest_same_name_and_formats_detail() {
        let text = "int f(int value) {\n    long value;\n    val\n}\n";
        let bindings = vec![
            LocalBinding {
                name: "value".to_string(),
                kind: LocalBindingKind::Parameter,
                type_text: Some("int".to_string()),
                decl_start_byte: text.find("value").unwrap(),
                function_start_byte: 0,
                function_end_byte: text.len(),
            },
            LocalBinding {
                name: "value".to_string(),
                kind: LocalBindingKind::LocalVariable,
                type_text: Some("long".to_string()),
                decl_start_byte: text.rfind("value").unwrap(),
                function_start_byte: 0,
                function_end_byte: text.len(),
            },
        ];

        let hits = local_completion_candidates(&bindings, text, 2, 7, "val", 10);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, LocalBindingKind::LocalVariable);
        assert_eq!(hits[0].detail, "local: long");
    }
}
