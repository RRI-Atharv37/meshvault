//! Case-insensitive glob patterns for paths the scanner leaves alone. The caller supplies them.

use crate::path::{fold, RelPath};

/// Why a pattern was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IgnoreError {
    /// An empty pattern would match everything or nothing by accident.
    #[error("empty ignore pattern")]
    EmptyPattern,
}

#[derive(Clone, Debug)]
struct Pattern {
    anchored: bool,
    segments: Vec<Vec<char>>,
}

/// A set of glob patterns. `*` and `?` stay within one component, `**` crosses components. A
/// pattern without `/` matches a component at any depth; one with `/` matches from the root. A
/// path is ignored if it or any parent directory matches.
#[derive(Clone, Debug, Default)]
pub struct IgnoreSet {
    patterns: Vec<Pattern>,
}

impl IgnoreSet {
    /// Builds a set from patterns.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<IgnoreSet, IgnoreError> {
        let mut out = Vec::with_capacity(patterns.len());
        for raw in patterns {
            let folded = fold(raw.as_ref());
            let trimmed = folded.trim_matches('/');
            if trimmed.is_empty() {
                return Err(IgnoreError::EmptyPattern);
            }
            out.push(Pattern {
                anchored: trimmed.contains('/'),
                segments: trimmed.split('/').map(|s| s.chars().collect()).collect(),
            });
        }
        Ok(IgnoreSet { patterns: out })
    }

    /// Whether `p` or one of its parents matches.
    pub fn is_ignored(&self, p: &RelPath) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let folded = fold(p.as_str());
        let comps: Vec<Vec<char>> = folded.split('/').map(|c| c.chars().collect()).collect();
        self.patterns.iter().any(|pat| {
            if pat.anchored {
                (1..=comps.len()).any(|n| segments_match(&pat.segments, &comps[..n]))
            } else {
                comps.iter().any(|c| glob(&pat.segments[0], c))
            }
        })
    }
}

fn is_double_star(seg: &[char]) -> bool {
    seg == ['*', '*']
}

fn segments_match(pattern: &[Vec<char>], comps: &[Vec<char>]) -> bool {
    match pattern.split_first() {
        None => comps.is_empty(),
        Some((seg, rest)) if is_double_star(seg) => {
            (0..=comps.len()).any(|skip| segments_match(rest, &comps[skip..]))
        }
        Some((seg, rest)) => match comps.split_first() {
            Some((c, tail)) => glob(seg, c) && segments_match(rest, tail),
            None => false,
        },
    }
}

fn glob(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                backtrack = Some((p, t));
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(&c) if c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match backtrack {
                Some((bp, bt)) => {
                    backtrack = Some((bp, bt + 1));
                    p = bp + 1;
                    t = bt + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}
