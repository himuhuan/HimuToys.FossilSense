fn same_documentation_project(left: &Candidate, right: &Candidate) -> bool {
    left.project_key == right.project_key
}

fn legacy_pair(left: &Candidate, right: &Candidate) -> bool {
    same_documentation_project(left, right)
}
