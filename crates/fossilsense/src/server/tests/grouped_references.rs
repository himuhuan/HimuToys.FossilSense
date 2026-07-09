use super::*;

// --- R6 section 4: grouped references role exposure --------------------

#[test]
fn grouped_reference_items_preserve_role_and_order() {
    use crate::parser::SyntacticRole;
    use crate::references::{self, ReferenceHit};
    let dir = tempdir().expect("tempdir");
    let mut hits = vec![
        ReferenceHit {
            rel_path: "a.c".into(),
            line: 9,
            start_col_utf16: 0,
            end_col_utf16: 3,
            role: SyntacticRole::Read,
        },
        ReferenceHit {
            rel_path: "b.c".into(),
            line: 2,
            start_col_utf16: 0,
            end_col_utf16: 3,
            role: SyntacticRole::Definition,
        },
    ];
    references::sort_hits_by_role(&mut hits);
    let items = grouped_reference_items(dir.path(), &hits);
    assert_eq!(items.len(), 2);
    // Definition group first; each item carries its role label for the client.
    assert_eq!(items[0].role, "definition");
    assert_eq!(items[1].role, "read");
}
