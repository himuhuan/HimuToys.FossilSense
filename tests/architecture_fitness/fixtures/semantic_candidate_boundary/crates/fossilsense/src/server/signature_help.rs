fn legacy_signature_help(store: &Store, name: &str) {
    let records = store.symbols_by_name(name);
    let _ = rank_function_signature_candidates(records);
}
