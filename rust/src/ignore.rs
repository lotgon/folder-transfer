//! Ignore patterns: a byte-for-byte port of `Convert-GlobToRegex` +
//! `Test-IgnoredRel` from the PowerShell scripts. The client and server MUST
//! agree so the mirror step never deletes ignored content. See spec section 6.7.
//!
//! Rules:
//! - Patterns are `;`/`,`-separated. `\` is normalised to `/`. A trailing `/`
//!   means directories only.
//! - A body WITHOUT `/` is a NAME pattern: it matches ANY path segment, with
//!   `*` (within a segment) and `?` (one char), case-insensitive.
//! - A body WITH `/` is a PATH pattern anchored at the root: `*` stays within a
//!   segment, `**` spans any depth, `?` one char; anchored `^...$`, case-insensitive.
//! - An item is ignored if it OR any ancestor directory matches.

use regex::Regex;

enum Kind {
    /// No `/` in the body: match against a single segment.
    Name(Regex),
    /// `/` in the body: match against a `/`-joined prefix of the segments.
    Path(Regex),
}

struct Pattern {
    dir_only: bool,
    kind: Kind,
}

/// A parsed, reusable set of ignore patterns.
pub struct IgnoreSet {
    patterns: Vec<Pattern>,
}

impl IgnoreSet {
    /// Parse a `;`/`,`-separated specification (empty entries dropped).
    pub fn parse(spec: &str) -> Self {
        let mut patterns = Vec::new();
        for raw in spec.split([';', ',']) {
            let p = raw.trim();
            if p.is_empty() {
                continue;
            }
            let body = p.replace('\\', "/");
            let dir_only = body.ends_with('/');
            let body = body.trim_matches('/');
            if body.is_empty() {
                continue;
            }
            let kind = if body.contains('/') {
                Kind::Path(glob_to_regex(body))
            } else {
                Kind::Name(name_to_regex(body))
            };
            patterns.push(Pattern { dir_only, kind });
        }
        IgnoreSet { patterns }
    }

    /// Is `rel` (or any ancestor directory of it) ignored? `rel` is relative to
    /// the shared folder's parent (its first segment is the shared folder name).
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let rel = rel.replace('\\', "/");
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            return false;
        }
        let segs: Vec<&str> = rel.split('/').collect();
        for pat in &self.patterns {
            match &pat.kind {
                Kind::Path(rx) => {
                    // Test each prefix segs[0..i] for i = 1..=len.
                    for i in 1..=segs.len() {
                        let is_seg_dir = i < segs.len() || is_dir;
                        if pat.dir_only && !is_seg_dir {
                            continue;
                        }
                        let candidate = segs[..i].join("/");
                        if rx.is_match(&candidate) {
                            return true;
                        }
                    }
                }
                Kind::Name(rx) => {
                    for (i, seg) in segs.iter().enumerate() {
                        let is_seg_dir = i < segs.len() - 1 || is_dir;
                        if pat.dir_only && !is_seg_dir {
                            continue;
                        }
                        if rx.is_match(seg) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

/// glob with `/` separators -> anchored, case-insensitive regex.
/// `*` = within a segment (`[^/]*`), `**` = any depth (`.*`), `?` = one char (`[^/]`).
fn glob_to_regex(glob: &str) -> Regex {
    let chars: Vec<char> = glob.chars().collect();
    let mut pat = String::from("^");
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if i + 1 < chars.len() && chars[i + 1] == '*' {
                pat.push_str(".*");
                i += 2;
            } else {
                pat.push_str("[^/]*");
                i += 1;
            }
        } else if c == '?' {
            pat.push_str("[^/]");
            i += 1;
        } else {
            pat.push_str(&regex::escape(&c.to_string()));
            i += 1;
        }
    }
    pat.push('$');
    build_ci(&pat)
}

/// PowerShell `-like` on a single segment: `*` = any, `?` = one char.
fn name_to_regex(body: &str) -> Regex {
    let mut pat = String::from("^");
    for c in body.chars() {
        match c {
            '*' => pat.push_str(".*"),
            '?' => pat.push('.'),
            _ => pat.push_str(&regex::escape(&c.to_string())),
        }
    }
    pat.push('$');
    build_ci(&pat)
}

fn build_ci(pattern: &str) -> Regex {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .expect("internally generated regex must compile")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_ignores_nothing() {
        let s = IgnoreSet::parse("");
        assert!(s.patterns.is_empty());
        assert!(!s.is_ignored("Share/file.txt", false));
    }

    #[test]
    fn name_pattern_matches_any_segment() {
        let s = IgnoreSet::parse("*.log");
        assert!(s.is_ignored("Share/sub/app.log", false));
        assert!(!s.is_ignored("Share/sub/app.txt", false));
        // case-insensitive
        assert!(s.is_ignored("Share/APP.LOG", false));
    }

    #[test]
    fn name_pattern_dir_match_via_ancestor() {
        let s = IgnoreSet::parse("cache");
        // a file under a 'cache' directory is ignored (ancestor segment matches)
        assert!(s.is_ignored("Share/cache/x.bin", false));
        assert!(s.is_ignored("Share/cache", true));
    }

    #[test]
    fn dir_only_pattern() {
        let s = IgnoreSet::parse("log/");
        // 'log' as a directory (or ancestor) is ignored...
        assert!(s.is_ignored("Share/log/a.txt", false));
        assert!(s.is_ignored("Share/log", true));
        // ...but a FILE named 'log' is not (dir-only)
        assert!(!s.is_ignored("Share/log", false));
    }

    #[test]
    fn path_pattern_anchored_with_doublestar() {
        let s = IgnoreSet::parse("Share/**/cache/");
        assert!(s.is_ignored("Share/a/b/cache/x", false));
        // PowerShell-literal `**` -> `.*` needs an intermediate segment, so
        // "Share/cache" (zero dirs between) does NOT match this pattern.
        assert!(!s.is_ignored("Share/cache/x", false));
        // anchored at root: 'Other/.../cache' does not match the 'Share/' prefix
        assert!(!s.is_ignored("Other/a/cache/x", false));
    }

    #[test]
    fn path_pattern_single_star_within_segment() {
        let s = IgnoreSet::parse("Share/*/temp/");
        assert!(s.is_ignored("Share/one/temp/f", false));
        // '*' does not span depth
        assert!(!s.is_ignored("Share/one/two/temp/f", false));
    }

    #[test]
    fn semicolon_and_comma_separated() {
        let s = IgnoreSet::parse("*.log ; cache, *.tmp");
        assert!(s.is_ignored("S/a.log", false));
        assert!(s.is_ignored("S/cache/x", false));
        assert!(s.is_ignored("S/a.tmp", false));
        assert!(!s.is_ignored("S/a.dat", false));
    }
}
