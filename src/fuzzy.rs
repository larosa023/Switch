//! Fuzzy matching for type-to-search.
//!
//! Every whitespace-separated token in the query must match the window's
//! app name, title or exe, either as a substring or as an in-order
//! subsequence ("vsc" -> "Visual Studio Code"). Matches at word starts,
//! consecutive runs and prefixes score higher. The matched characters of
//! the title are returned as UTF-16 ranges so they can be highlighted.

pub struct Match {
    pub score: i32,
    /// (start, len) in UTF-16 code units of the title.
    pub title_ranges: Vec<(u32, u32)>,
}

fn is_boundary(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()),
    }
}

/// Score `token` (lower-case) against `text`. Returns the score and the
/// matched char indices.
fn score(token: &[char], text: &str) -> Option<(i32, Vec<usize>)> {
    if token.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let n = token.len();

    // 1. Contiguous substring: best occurrence (prefer word starts).
    let mut best: Option<(i32, usize)> = None;
    if lower.len() >= n {
        for i in 0..=lower.len() - n {
            if lower[i..i + n] == *token {
                let mut s = 100 + 8 * n as i32;
                if i == 0 {
                    s += 40;
                } else if is_boundary(Some(chars[i - 1]), chars[i]) {
                    s += 25;
                }
                s -= (i as i32).min(30) / 3;
                if best.map(|(b, _)| s > b).unwrap_or(true) {
                    best = Some((s, i));
                }
            }
        }
    }
    if let Some((s, i)) = best {
        return Some((s, (i..i + n).collect()));
    }
    if n < 2 {
        return None;
    }

    // 2. Subsequence, greedily preferring word starts.
    let mut idx = Vec::with_capacity(n);
    let mut pos = 0usize;
    let mut s = 0i32;
    for (ti, &tc) in token.iter().enumerate() {
        // Prefer the next word-start occurrence, else the next occurrence.
        let mut found = None;
        for j in pos..lower.len() {
            if lower[j] == tc && is_boundary(if j == 0 { None } else { Some(chars[j - 1]) }, chars[j]) {
                found = Some(j);
                break;
            }
        }
        let next_plain = (pos..lower.len()).find(|&j| lower[j] == tc);
        // Stay consecutive if possible.
        if let (Some(&last), Some(p)) = (idx.last(), next_plain) {
            if p == last + 1 {
                found = Some(p);
            }
        }
        let j = found.or(next_plain)?;
        s += 10;
        if is_boundary(if j == 0 { None } else { Some(chars[j - 1]) }, chars[j]) {
            s += 15;
        }
        if let Some(&last) = idx.last() {
            if j == last + 1 {
                s += 10;
            } else {
                s -= ((j - last - 1) as i32).min(10);
            }
        } else if ti == 0 {
            s -= (j as i32).min(15);
        }
        idx.push(j);
        pos = j + 1;
    }
    Some((s, idx))
}

fn to_utf16_ranges(text: &str, idx: &[usize]) -> Vec<(u32, u32)> {
    let mut offsets = Vec::with_capacity(text.len());
    let mut off = 0u32;
    for c in text.chars() {
        offsets.push((off, c.len_utf16() as u32));
        off += c.len_utf16() as u32;
    }
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &i in idx {
        let Some(&(o, l)) = offsets.get(i) else { continue };
        match out.last_mut() {
            Some((s, len)) if *s + *len == o => *len += l,
            _ => out.push((o, l)),
        }
    }
    out
}

pub fn match_window(query: &str, title: &str, app: &str, exe: &str) -> Option<Match> {
    let mut total = 0;
    let mut title_idx: Vec<usize> = Vec::new();
    for tok in query.split_whitespace() {
        let t: Vec<char> = tok.to_lowercase().chars().collect();
        let in_app = score(&t, app).map(|(s, _)| s + 20); // app names are a strong signal
        let in_exe = score(&t, exe.trim_end_matches(".exe")).map(|(s, _)| s);
        let in_title = score(&t, title);
        let best_other = in_app.max(in_exe);
        match (&in_title, best_other) {
            (None, None) => return None,
            (Some((ts, ti)), other) => {
                total += (*ts).max(other.unwrap_or(i32::MIN));
                title_idx.extend(ti);
            }
            (None, Some(o)) => total += o,
        }
    }
    title_idx.sort_unstable();
    title_idx.dedup();
    Some(Match { score: total, title_ranges: to_utf16_ranges(title, &title_idx) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_and_subsequence() {
        assert!(match_window("chr", "New Tab", "Google Chrome", "chrome.exe").is_some());
        assert!(match_window("vsc", "main.rs - Switch", "Visual Studio Code", "code.exe").is_some());
        assert!(match_window("xyz", "Inbox", "Outlook", "olk.exe").is_none());
        let m = match_window("inb", "Inbox - Outlook", "Outlook", "olk.exe").unwrap();
        assert_eq!(m.title_ranges, vec![(0, 3)]);
    }

    #[test]
    fn ranking_prefers_word_starts() {
        let a = match_window("note", "Untitled - Notepad", "Notepad", "notepad.exe").unwrap().score;
        let b = match_window("note", "Keynote draft", "Word", "winword.exe").unwrap().score;
        assert!(a > b);
    }

    #[test]
    fn multiple_tokens_all_required() {
        assert!(match_window("chrome git", "GitHub - Chrome", "Google Chrome", "chrome.exe").is_some());
        assert!(match_window("chrome slack", "GitHub - Chrome", "Google Chrome", "chrome.exe").is_none());
    }

    #[test]
    fn utf16_ranges() {
        let m = match_window("caf", "café — menu", "App", "app.exe").unwrap();
        assert_eq!(m.title_ranges, vec![(0, 3)]);
    }
}
