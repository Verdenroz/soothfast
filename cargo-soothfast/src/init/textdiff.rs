//! A line-based unified diff, enough to show what `init` would change.

const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq)]
enum Op<'a> {
    Keep(&'a str),
    Del(&'a str),
    Add(&'a str),
}

/// A hunk being written; `old` and `new` count the lines it holds.
struct Hunk {
    old_start: usize,
    new_start: usize,
    old: usize,
    new: usize,
    body: String,
}

impl Hunk {
    fn line(&mut self, mark: char, line: &str) {
        self.body.push(mark);
        self.body.push_str(line.strip_suffix('\n').unwrap_or(line));
        self.body.push('\n');
        if !line.ends_with('\n') {
            self.body.push_str("\\ No newline at end of file\n");
        }
        self.old += usize::from(mark != '+');
        self.new += usize::from(mark != '-');
    }

    fn finish(self, out: &mut String) {
        let start = |at: usize, len: usize| if len == 0 { at } else { at + 1 };
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n{}",
            start(self.old_start, self.old),
            self.old,
            start(self.new_start, self.new),
            self.new,
            self.body
        ));
    }
}

/// The unified diff turning `old` into `new`, hunks only; empty when the
/// text is the same. Line endings count, so a file that differs only by them
/// gets a note instead of a wall of identical-looking lines.
pub fn unified(old: &str, new: &str) -> String {
    if old != new && old.replace("\r\n", "\n") == new.replace("\r\n", "\n") {
        return "line endings differ (CRLF against LF)\n".into();
    }
    let old: Vec<&str> = old.split_inclusive('\n').collect();
    let new: Vec<&str> = new.split_inclusive('\n').collect();
    let mut out = String::new();
    let mut open: Option<Hunk> = None;
    let mut keeps: Vec<&str> = Vec::new();
    let (mut seen_old, mut seen_new) = (0, 0);
    for op in edits(&old, &new) {
        match op {
            Op::Keep(line) => {
                keeps.push(line);
                seen_old += 1;
                seen_new += 1;
                if keeps.len() > 2 * CONTEXT
                    && let Some(mut hunk) = open.take()
                {
                    keeps[..CONTEXT].iter().for_each(|l| hunk.line(' ', l));
                    hunk.finish(&mut out);
                }
                if open.is_none() && keeps.len() > CONTEXT {
                    keeps.drain(..keeps.len() - CONTEXT);
                }
            }
            Op::Del(line) | Op::Add(line) => {
                let hunk = open.get_or_insert_with(|| Hunk {
                    old_start: seen_old - keeps.len(),
                    new_start: seen_new - keeps.len(),
                    old: 0,
                    new: 0,
                    body: String::new(),
                });
                keeps.drain(..).for_each(|l| hunk.line(' ', l));
                if matches!(op, Op::Del(_)) {
                    hunk.line('-', line);
                    seen_old += 1;
                } else {
                    hunk.line('+', line);
                    seen_new += 1;
                }
            }
        }
    }
    if let Some(mut hunk) = open {
        keeps.iter().take(CONTEXT).for_each(|l| hunk.line(' ', l));
        hunk.finish(&mut out);
    }
    out
}

/// The shortest edit script between `old` and `new`. Lines both share at
/// either end are set aside first, so the table of longest common
/// subsequences only spans what changed.
fn edits<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Op<'a>> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut ops: Vec<Op> = old[..prefix].iter().copied().map(Op::Keep).collect();
    ops.extend(lcs_edits(
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    ));
    ops.extend(old[old.len() - suffix..].iter().copied().map(Op::Keep));
    ops
}

fn lcs_edits<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Op<'a>> {
    let width = new.len() + 1;
    let mut common = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i * width + j] = if old[i] == new[j] {
                common[(i + 1) * width + j + 1] + 1
            } else {
                common[(i + 1) * width + j].max(common[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut ops = Vec::new();
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            ops.push(Op::Keep(old[i]));
            i += 1;
            j += 1;
        } else if common[(i + 1) * width + j] >= common[i * width + j + 1] {
            ops.push(Op::Del(old[i]));
            i += 1;
        } else {
            ops.push(Op::Add(new[j]));
            j += 1;
        }
    }
    ops.extend(old[i..].iter().copied().map(Op::Del));
    ops.extend(new[j..].iter().copied().map(Op::Add));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_text_has_no_hunks() {
        assert_eq!(unified("a\nb\n", "a\nb\n"), "");
    }

    #[test]
    fn a_changed_line_shows_with_its_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\n";
        assert_eq!(
            unified(old, new),
            "@@ -2,7 +2,7 @@\n b\n c\n d\n-e\n+E\n f\n g\n h\n"
        );
    }

    #[test]
    fn distant_changes_make_separate_hunks() {
        let old: String = (1..=20).map(|n| format!("{n}\n")).collect();
        let new = old
            .replacen("2\n", "two\n", 1)
            .replacen("19\n", "nineteen\n", 1);
        let diff = unified(&old, &new);
        assert_eq!(diff.matches("@@ ").count(), 2);
        assert!(diff.contains("-2\n+two\n") && diff.contains("-19\n+nineteen\n"));
    }

    #[test]
    fn changes_a_context_apart_share_a_hunk() {
        let old: String = (1..=12).map(|n| format!("{n}\n")).collect();
        let new = old.replacen("2\n", "two\n", 1).replacen("9\n", "nine\n", 1);
        assert_eq!(unified(&old, &new).matches("@@ ").count(), 1);
    }

    #[test]
    fn an_added_line_to_an_empty_file_starts_at_zero() {
        assert_eq!(unified("", "x\n"), "@@ -0,0 +1,1 @@\n+x\n");
    }

    #[test]
    fn a_missing_final_newline_is_marked() {
        assert_eq!(
            unified("a\nb", "a\nb\n"),
            "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
        );
    }

    #[test]
    fn crlf_against_lf_gets_one_note() {
        assert_eq!(
            unified("a\r\nb\r\n", "a\nb\n"),
            "line endings differ (CRLF against LF)\n"
        );
    }

    #[test]
    fn a_line_appended_to_a_large_file_is_found_without_a_full_table() {
        let old: String = (0..20_000).map(|n| format!("{n}\n")).collect();
        let new = format!("{old}tail\n");
        assert_eq!(
            unified(&old, &new),
            "@@ -19998,3 +19998,4 @@\n 19997\n 19998\n 19999\n+tail\n"
        );
    }
}
