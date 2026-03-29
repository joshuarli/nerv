/// Minimal line-level unified diff (replaces `similar` crate).
/// Implements Myers diff algorithm for line sequences.
use std::fmt::Write;

pub fn unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let edits = diff_lines(&old_lines, &new_lines);
    format_unified(&edits, old_label, new_label, 3)
}

#[derive(Debug, Clone)]
enum Edit<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// O(ND) Myers diff producing a minimal edit script.
/// Uses a single flat buffer for trace storage to minimize allocations.
fn diff_lines<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Edit<'a>> {
    let n = old.len();
    let m = new.len();

    if n == 0 {
        return new.iter().map(|l| Edit::Insert(l)).collect();
    }
    if m == 0 {
        return old.iter().map(|l| Edit::Delete(l)).collect();
    }

    let max_d = n + m;
    let vsize = 2 * max_d + 1;
    let offset = max_d;

    // Single flat buffer: trace[step * vsize .. (step+1) * vsize]
    let mut trace: Vec<usize> = Vec::new();
    let mut v = vec![0usize; vsize];

    let mut found_d = 0;
    'search: for d in 0..=max_d {
        trace.extend_from_slice(&v);

        let lo = -(d as isize);
        let hi = d as isize;
        let mut k = lo;
        while k <= hi {
            let ki = (k + offset as isize) as usize;
            let mut x = if k == lo || (k != hi && v[ki - 1] < v[ki + 1]) {
                v[ki + 1]
            } else {
                v[ki - 1] + 1
            };
            let mut y = (x as isize - k) as usize;

            while x < n && y < m && old[x] == new[y] {
                x += 1;
                y += 1;
            }

            v[ki] = x;
            if x >= n && y >= m {
                found_d = d;
                break 'search;
            }
            k += 2;
        }
    }

    // Backtrack from (n, m) to (0, 0)
    let mut x = n;
    let mut y = m;
    let mut edits: Vec<Edit<'a>> = Vec::with_capacity(n + m);

    for d in (1..=found_d).rev() {
        let prev_v = &trace[d * vsize..(d + 1) * vsize];
        let k = x as isize - y as isize;
        let ki = (k + offset as isize) as usize;

        let prev_k = if k == -(d as isize) || (k != d as isize && prev_v[ki - 1] < prev_v[ki + 1]) {
            k + 1
        } else {
            k - 1
        };

        let pki = (prev_k + offset as isize) as usize;
        let prev_x = prev_v[pki];
        let prev_y = (prev_x as isize - prev_k) as usize;

        while x > prev_x + if prev_k < k { 1 } else { 0 }
            && y > prev_y + if prev_k > k { 1 } else { 0 }
        {
            x -= 1;
            y -= 1;
            edits.push(Edit::Equal(old[x]));
        }

        if prev_k < k {
            x -= 1;
            edits.push(Edit::Delete(old[x]));
        } else {
            y -= 1;
            edits.push(Edit::Insert(new[y]));
        }
    }

    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        edits.push(Edit::Equal(old[x]));
    }

    edits.reverse();
    edits
}

fn format_unified(edits: &[Edit], old_label: &str, new_label: &str, context: usize) -> String {
    if edits.iter().all(|e| matches!(e, Edit::Equal(_))) {
        return format!("--- {}\n+++ {}\n", old_label, new_label);
    }

    let mut out = String::with_capacity(256);
    let _ = write!(out, "--- {}\n+++ {}\n", old_label, new_label);

    // Find hunk boundaries
    let mut change_ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < edits.len() {
        if !matches!(edits[i], Edit::Equal(_)) {
            let start = i.saturating_sub(context);
            let mut end = i;
            while end < edits.len() {
                if !matches!(edits[end], Edit::Equal(_)) {
                    end += 1;
                    continue;
                }
                let gap_start = end;
                while end < edits.len() && matches!(edits[end], Edit::Equal(_)) {
                    end += 1;
                }
                if end < edits.len() && (end - gap_start) <= context * 2 {
                    continue;
                }
                end = (gap_start + context).min(edits.len());
                break;
            }
            change_ranges.push((start, end));
            i = end;
        } else {
            i += 1;
        }
    }

    for (start, end) in change_ranges {
        let mut old_start = 1;
        let mut new_start = 1;
        for e in &edits[..start] {
            match e {
                Edit::Equal(_) | Edit::Delete(_) => old_start += 1,
                _ => {}
            }
            match e {
                Edit::Equal(_) | Edit::Insert(_) => new_start += 1,
                _ => {}
            }
        }

        let old_count = edits[start..end]
            .iter()
            .filter(|e| matches!(e, Edit::Equal(_) | Edit::Delete(_)))
            .count();
        let new_count = edits[start..end]
            .iter()
            .filter(|e| matches!(e, Edit::Equal(_) | Edit::Insert(_)))
            .count();

        let _ = writeln!(out, "@@ -{},{} +{},{} @@", old_start, old_count, new_start, new_count,);

        for e in &edits[start..end] {
            let (prefix, text) = match e {
                Edit::Equal(l) => (' ', *l),
                Edit::Delete(l) => ('-', *l),
                Edit::Insert(l) => ('+', *l),
            };
            out.push(prefix);
            out.push_str(text);
            out.push('\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_files() {
        let text = "line1\nline2\nline3\n";
        let diff = unified_diff(text, text, "a/file", "b/file");
        assert_eq!(diff, "--- a/file\n+++ b/file\n");
    }

    #[test]
    fn simple_replacement() {
        let old = "aaa\nbbb\nccc\n";
        let new = "aaa\nBBB\nccc\n";
        let diff = unified_diff(old, new, "a/f", "b/f");
        assert!(diff.contains("-bbb"), "diff missing -bbb: {}", diff);
        assert!(diff.contains("+BBB"), "diff missing +BBB: {}", diff);
    }

    #[test]
    fn insertion() {
        let old = "a\nc\n";
        let new = "a\nb\nc\n";
        let diff = unified_diff(old, new, "a/f", "b/f");
        assert!(diff.contains("+b"), "diff missing +b: {}", diff);
    }

    #[test]
    fn deletion() {
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        let diff = unified_diff(old, new, "a/f", "b/f");
        assert!(diff.contains("-b"), "diff missing -b: {}", diff);
    }

    #[test]
    fn empty_to_content() {
        let diff = unified_diff("", "hello\n", "a/f", "b/f");
        assert!(diff.contains("+hello"), "diff missing +hello: {}", diff);
    }

    #[test]
    fn content_to_empty() {
        let diff = unified_diff("hello\n", "", "a/f", "b/f");
        assert!(diff.contains("-hello"), "diff missing -hello: {}", diff);
    }

    #[test]
    fn multi_hunk() {
        let old = (0..20).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
        let new = old.replace("line3", "LINE3").replace("line17", "LINE17");
        let diff = unified_diff(&old, &new, "a/f", "b/f");
        let hunk_count = diff.matches("@@").count();
        assert!(hunk_count >= 4, "expected 2+ hunks (4+ @@), got {}: {}", hunk_count / 2, diff);
    }
}
