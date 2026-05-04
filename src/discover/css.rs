//! Single-pass `url(...)` extraction-and-rewrite for inline `<style>` text.
//!
//! Returns the rewritten CSS plus the list of raw URL strings that were seen,
//! so callers can collect them as asset references in the same walk.

pub fn process<F>(css: &str, mut rewrite: F) -> (String, Vec<String>)
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = String::with_capacity(css.len());
    let mut urls = Vec::new();
    let bytes = css.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i..].starts_with(b"url(") {
            out.push_str("url(");
            let mut j = i + 4;

            // Skip and preserve whitespace after `(`.
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                out.push(bytes[j] as char);
                j += 1;
            }

            let quote = match bytes.get(j) {
                Some(&b'"') => Some(b'"'),
                Some(&b'\'') => Some(b'\''),
                _ => None,
            };

            let (raw_url, end_idx) = if let Some(q) = quote {
                out.push(q as char);
                j += 1;
                let start = j;
                while j < bytes.len() && bytes[j] != q {
                    j += 1;
                }
                let url = std::str::from_utf8(&bytes[start..j]).unwrap_or("").to_string();
                (url, j)
            } else {
                let start = j;
                while j < bytes.len() && bytes[j] != b')' {
                    j += 1;
                }
                let url = std::str::from_utf8(&bytes[start..j]).unwrap_or("").trim().to_string();
                (url, j)
            };

            if !raw_url.is_empty() {
                urls.push(raw_url.clone());
            }
            let new_url = rewrite(&raw_url).unwrap_or(raw_url);
            out.push_str(&new_url);

            if let Some(q) = quote {
                if end_idx < bytes.len() {
                    out.push(q as char);
                }
                j = end_idx + 1; // past closing quote
            } else {
                j = end_idx;
            }

            // Append closing `)` and advance past it.
            if j < bytes.len() && bytes[j] == b')' {
                out.push(')');
                i = j + 1;
            } else {
                i = j;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    (out, urls)
}

#[cfg(test)]
mod tests {
    use super::process;

    fn rewrite_absolute(url: &str) -> Option<String> {
        url.strip_prefix('/').map(|stripped| format!("./{}", stripped))
    }

    #[test]
    fn extracts_unquoted_url() {
        let (out, urls) = process(".a { background: url(img/bg.png); }", |_| None);
        assert_eq!(out, ".a { background: url(img/bg.png); }");
        assert_eq!(urls, vec!["img/bg.png"]);
    }

    #[test]
    fn extracts_double_quoted_url() {
        let (out, urls) = process(r#"a { src: url("img/bg.png"); }"#, |_| None);
        assert_eq!(out, r#"a { src: url("img/bg.png"); }"#);
        assert_eq!(urls, vec!["img/bg.png"]);
    }

    #[test]
    fn extracts_single_quoted_url() {
        let (_out, urls) = process(r#"a { src: url('img/bg.png'); }"#, |_| None);
        assert_eq!(urls, vec!["img/bg.png"]);
    }

    #[test]
    fn rewrites_absolute_paths() {
        let (out, urls) = process(".a { src: url(/img/bg.png); }", rewrite_absolute);
        assert_eq!(out, ".a { src: url(./img/bg.png); }");
        assert_eq!(urls, vec!["/img/bg.png"]);
    }

    #[test]
    fn rewrites_quoted_absolute_paths() {
        let (out, _urls) = process(r#"a { src: url("/img/bg.png"); }"#, rewrite_absolute);
        assert_eq!(out, r#"a { src: url("./img/bg.png"); }"#);
    }

    #[test]
    fn handles_multiple_urls() {
        let css = ".a { background: url(/a.png); } .b { background: url(b.png); }";
        let (out, urls) = process(css, rewrite_absolute);
        assert_eq!(out, ".a { background: url(./a.png); } .b { background: url(b.png); }");
        assert_eq!(urls, vec!["/a.png", "b.png"]);
    }

    #[test]
    fn passes_through_non_url_css() {
        let css = "body { color: red; }";
        let (out, urls) = process(css, |_| None);
        assert_eq!(out, css);
        assert!(urls.is_empty());
    }
}
