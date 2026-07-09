use std::collections::HashSet;

use crate::includes;

use super::parent_slash;

#[derive(Debug, Clone, Default)]
pub(in crate::server) struct CurrentIncludeEvidence {
    pub(super) source_dir: Option<String>,
    pub(super) recent_targets: HashSet<String>,
    pub(super) recent_basenames: HashSet<String>,
}

impl CurrentIncludeEvidence {
    pub(in crate::server) fn from_text(text: &str, current_rel_path: Option<&str>) -> Self {
        let source_dir = current_rel_path.and_then(parent_slash);
        let mut evidence = Self {
            source_dir,
            recent_targets: HashSet::new(),
            recent_basenames: HashSet::new(),
        };
        for line in text.lines() {
            let Some((_form, target)) = includes::parse_include_line(line) else {
                continue;
            };
            let target = target.replace('\\', "/");
            let target_lower = target.to_ascii_lowercase();
            evidence.recent_targets.insert(target_lower.clone());
            if let Some(dir) = &evidence.source_dir {
                if !target.contains('/') {
                    evidence
                        .recent_targets
                        .insert(format!("{dir}/{target}").to_ascii_lowercase());
                }
            }
            if let Some(name) = target.rsplit('/').next() {
                evidence.recent_basenames.insert(name.to_ascii_lowercase());
            }
        }
        evidence
    }
}
