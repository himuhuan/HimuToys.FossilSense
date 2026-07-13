fn unbounded_excerpt(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}
