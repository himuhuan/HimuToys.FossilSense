//! Index-build-only fact growth checkpoints. Tree-sitter allocations remain
//! covered by conservative input/workspace reservations, not this counter.
use std::cell::Cell;

thread_local! {
    static LIMIT: Cell<usize> = const { Cell::new(usize::MAX) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static EXCEEDED: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn active() -> bool {
    LIMIT.get() != usize::MAX
}
pub(super) fn exceeded() -> bool {
    EXCEEDED.get()
}
pub(super) fn check(bytes: usize) -> bool {
    PEAK.set(PEAK.get().max(bytes));
    if bytes > LIMIT.get() {
        EXCEEDED.set(true);
    }
    !exceeded()
}

struct Restore(usize, bool, usize);
impl Drop for Restore {
    fn drop(&mut self) {
        LIMIT.set(self.0);
        EXCEEDED.set(self.1);
        PEAK.set(self.2);
    }
}

pub(super) fn with_limit<T>(bytes: usize, operation: impl FnOnce() -> T) -> Result<T, usize> {
    let restore = Restore(
        LIMIT.replace(bytes),
        EXCEEDED.replace(false),
        PEAK.replace(0),
    );
    let result = operation();
    let exceeded = exceeded();
    let peak = PEAK.get();
    drop(restore);
    if exceeded {
        Err(peak)
    } else {
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fact_budget_failure_never_returns_partial_facts_and_restores_thread_state() {
        let source: String = (0..3000).map(|i| format!("int v{i};\n")).collect();
        let path = std::path::Path::new("dense.c");
        let selection =
            crate::config::LanguageSelection::explicit(crate::config::SourceLanguage::C);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert!(super::super::parse_thread_local_with_selection_budget(
            path,
            &source,
            selection,
            super::super::ParseFacts::INDEX,
            &cancel,
            1024
        )
        .is_err());
        assert!(!super::active());
        assert!(!super::exceeded());
        let reference = super::super::parse_thread_local_with_selection(
            path,
            &source,
            selection,
            super::super::ParseFacts::INDEX,
        );
        let bounded = super::super::parse_thread_local_with_selection_budget(
            path,
            &source,
            selection,
            super::super::ParseFacts::INDEX,
            &cancel,
            16 * 1024 * 1024,
        )
        .unwrap()
        .unwrap();
        assert_eq!(bounded, reference);
        assert_eq!(bounded.declarations.len(), 3000);
    }

    #[test]
    fn bounded_parser_preserves_complete_c_and_go_relationship_facts() {
        use crate::config::{LanguageSelection, SourceLanguage};
        for (path, language, source) in [
            ("ordinary.c", SourceLanguage::C, "#include \"api.h\"\nstruct S { int value; }; typedef struct S Alias; int target(void) { return 1; } int call(void) { return target(); }\n"),
            ("tolerant.c", SourceLanguage::C, "int target(void) { return 1; } int call(void) { return target(); } int broken( {\n"),
            ("authorized/external/api.h", SourceLanguage::C, "#if FEATURE\nstruct S { int value; };\n#endif\nint target(void); static inline int call(void) { return target(); }\n"),
            ("pkg/main.go", SourceLanguage::Go, "package main\nimport \"strings\"\ntype S struct { Value int }\nfunc target() string { return strings.TrimSpace(\" x \"); }\nfunc call() string { return target(); }\n"),
        ] {
            let path = std::path::Path::new(path);
            let selection = LanguageSelection::explicit(language);
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let reference = super::super::parse_thread_local_with_selection(path, source, selection, super::super::ParseFacts::INDEX);
            let bounded = super::super::parse_thread_local_with_selection_budget(path, source, selection,
                super::super::ParseFacts::INDEX, &cancel, 16 * 1024 * 1024).unwrap().unwrap();
            assert!(!reference.declarations.is_empty());
            assert!(!reference.call_sites.is_empty(), "fixture must exercise relationships: {path:?}");
            assert_eq!(bounded, reference, "complete fact mismatch for {path:?}");
        }
    }
}
