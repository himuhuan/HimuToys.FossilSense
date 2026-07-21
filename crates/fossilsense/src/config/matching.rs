pub(super) fn path_matches_glob_entry(rel_slash_path: &str, entry: &str) -> bool {
    wildcard_match(
        rel_slash_path.to_ascii_lowercase().as_bytes(),
        entry.to_ascii_lowercase().as_bytes(),
    )
}

/// Match normalized source-language override globs with path-separator-aware
/// semantics. `*` and `?` stay inside one component; `**` consumes zero or
/// more complete components, so `src/**/*.h` also matches `src/api.h`.
pub(super) fn language_override_glob_matches(path: &str, pattern: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    let path_segments: Vec<&str> = path.split('/').collect();
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    language_glob_segments_match(&path_segments, &pattern_segments)
}

fn language_glob_segments_match(path: &[&str], pattern: &[&str]) -> bool {
    let Some((head, tail)) = pattern.split_first() else {
        return path.is_empty();
    };
    if *head == "**" {
        return language_glob_segments_match(path, tail)
            || (!path.is_empty() && language_glob_segments_match(&path[1..], pattern));
    }
    let Some((path_head, path_tail)) = path.split_first() else {
        return false;
    };
    wildcard_match(path_head.as_bytes(), head.as_bytes())
        && language_glob_segments_match(path_tail, tail)
}

fn wildcard_match(path: &[u8], pattern: &[u8]) -> bool {
    let mut path_idx = 0usize;
    let mut pattern_idx = 0usize;
    let mut star: Option<usize> = None;
    let mut star_path_idx = 0usize;

    while path_idx < path.len() {
        if pattern_idx < pattern.len() {
            match pattern[pattern_idx] {
                b'?' => {
                    path_idx += 1;
                    pattern_idx += 1;
                    continue;
                }
                b'*' => {
                    star = Some(pattern_idx);
                    pattern_idx += 1;
                    star_path_idx = path_idx;
                    continue;
                }
                b'[' => {
                    if let Some(next_pattern_idx) =
                        char_class_matches(path[path_idx], pattern, pattern_idx)
                    {
                        path_idx += 1;
                        pattern_idx = next_pattern_idx;
                        continue;
                    }
                }
                literal if literal == path[path_idx] => {
                    path_idx += 1;
                    pattern_idx += 1;
                    continue;
                }
                _ => {}
            }
        }

        if let Some(star_idx) = star {
            pattern_idx = star_idx + 1;
            star_path_idx += 1;
            path_idx = star_path_idx;
        } else {
            return false;
        }
    }

    while pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
        pattern_idx += 1;
    }
    pattern_idx == pattern.len()
}

fn char_class_matches(ch: u8, pattern: &[u8], start: usize) -> Option<usize> {
    let mut idx = start + 1;
    if idx >= pattern.len() {
        return None;
    }

    let negated = matches!(pattern[idx], b'!' | b'^');
    if negated {
        idx += 1;
    }

    let mut matched = false;
    let mut saw_end = false;
    while idx < pattern.len() {
        if pattern[idx] == b']' {
            saw_end = true;
            break;
        }

        if idx + 2 < pattern.len() && pattern[idx + 1] == b'-' && pattern[idx + 2] != b']' {
            let start_ch = pattern[idx];
            let end_ch = pattern[idx + 2];
            if start_ch <= ch && ch <= end_ch {
                matched = true;
            }
            idx += 3;
        } else {
            if pattern[idx] == ch {
                matched = true;
            }
            idx += 1;
        }
    }

    if saw_end && matched != negated {
        Some(idx + 1)
    } else {
        None
    }
}
