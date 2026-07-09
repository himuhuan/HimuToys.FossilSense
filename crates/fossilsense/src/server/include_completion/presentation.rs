use std::collections::HashSet;

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};

pub(super) fn push_include_candidate(
    name: String,
    is_dir: bool,
    base_score: i32,
    seg: &str,
    seen: &mut HashSet<String>,
    scored: &mut Vec<(i32, String, CompletionItem)>,
) {
    if !seen.insert(name.to_ascii_lowercase()) {
        return;
    }
    let kind = if is_dir {
        CompletionItemKind::FOLDER
    } else {
        CompletionItemKind::FILE
    };
    let mut score = base_score;
    if name.eq_ignore_ascii_case(seg) {
        score += 100;
    } else if name
        .to_ascii_lowercase()
        .starts_with(&seg.to_ascii_lowercase())
    {
        score += 50;
    }
    scored.push((
        score,
        name.clone(),
        CompletionItem {
            label: name,
            kind: Some(kind),
            sort_text: Some(format!("{:06}", 10000 - score)),
            ..Default::default()
        },
    ));
}
