//! Node-ID normalization — matches Graphify-Labs/graphify `ids.py`.
//!
//! Recipe: replace non-word runs with `_`, collapse repeated `_`, strip
//! edges, casefold. Full NFKC is not required for code IDs (ASCII/Unicode
//! alphanumerics dominate); this keeps the crate dependency-light while
//! producing IDs compatible with Graphify's skill schema.

/// Normalize a single ID string to its canonical form.
///
/// Idempotent: `normalize_id(normalize_id(s)) == normalize_id(s)`.
///
/// Matches Graphify `ids.py`: non-word runs → `_`, then collapse `_+` → `_`,
/// strip edges, casefold. Underscores are word chars, so raw `__` is collapsed
/// by the second pass (same as Python `re.sub(r"_+", "_", s)`).
pub fn normalize_id(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_us = false;
    for ch in s.chars() {
        // Alphanumerics only in the first pass — treat `_` like other
        // separators so runs of underscores collapse consistently.
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Build a canonical node ID from one or more name parts.
pub fn make_id(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| p.trim_matches(|c| c == '_' || c == '.'))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    normalize_id(&joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_id_basic() {
        assert_eq!(make_id(&["Auth", "login"]), "auth_login");
        assert_eq!(make_id(&["foo.bar"]), "foo_bar");
        assert_eq!(normalize_id("Foo__Bar!!"), "foo_bar");
        assert_eq!(normalize_id(normalize_id("A::B").as_str()), "a_b");
    }
}
