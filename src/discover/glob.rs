//! Tiny glob matcher supporting `*` (within a path segment) and `**` (any
//! number of segments). Recursive implementation chosen for clarity over the
//! iterative two-pointer version, which had a subtle backtracking bug when
//! `**` and `*` interleaved.

pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");
    matches(pat.as_bytes(), path.as_bytes())
}

fn matches(pat: &[u8], path: &[u8]) -> bool {
    // Empty pattern matches only empty path.
    if pat.is_empty() {
        return path.is_empty();
    }

    // `**` or `**/` — match zero or more segments.
    if pat.starts_with(b"**") {
        let rest = if pat.get(2) == Some(&b'/') { &pat[3..] } else { &pat[2..] };
        // Try consuming 0 chars, then 1, then 2, ... up to and including the full path.
        if matches(rest, path) {
            return true;
        }
        for i in 0..path.len() {
            if matches(rest, &path[i + 1..]) {
                return true;
            }
        }
        return false;
    }

    // `*` — match any chars within a single segment (no `/`).
    if pat[0] == b'*' {
        let rest = &pat[1..];
        if matches(rest, path) {
            return true;
        }
        for i in 0..path.len() {
            if path[i] == b'/' {
                break;
            }
            if matches(rest, &path[i + 1..]) {
                return true;
            }
        }
        return false;
    }

    // `?` — match exactly one non-`/` char.
    if pat[0] == b'?' {
        if path.is_empty() || path[0] == b'/' {
            return false;
        }
        return matches(&pat[1..], &path[1..]);
    }

    // Literal char.
    if path.is_empty() || pat[0] != path[0] {
        return false;
    }
    matches(&pat[1..], &path[1..])
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn literal_matches() {
        assert!(glob_match("foo.js", "foo.js"));
        assert!(!glob_match("foo.js", "bar.js"));
    }

    #[test]
    fn star_within_segment() {
        assert!(glob_match("*.js", "app.js"));
        assert!(!glob_match("*.js", "sub/app.js"));
        assert!(glob_match("foo/*.js", "foo/app.js"));
    }

    #[test]
    fn double_star_any_segments() {
        assert!(glob_match("**/*.js", "app.js"));
        assert!(glob_match("**/*.js", "a/app.js"));
        assert!(glob_match("**/*.js", "a/b/c/app.js"));
        assert!(glob_match("assets/**/*.png", "assets/icons/x.png"));
        assert!(glob_match("assets/**/*.png", "assets/x.png"));
    }

    #[test]
    fn double_star_then_single_star_backtracks() {
        // The previous iterative impl failed on this — the `*` overwrote
        // the `**` backtrack slot and lost the ability to retry.
        assert!(glob_match("**/foo/*.js", "a/foo/x.js"));
        assert!(glob_match("**/foo/*.js", "a/b/foo/x.js"));
        assert!(!glob_match("**/foo/*.js", "a/b/x.js"));
    }

    #[test]
    fn question_mark() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn empty_pattern() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }
}
