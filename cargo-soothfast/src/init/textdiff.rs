//! A line-based unified diff, enough to show what `init` would change.

const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq)]
enum Op<'a> {
    Keep(&'a str),
    Del(&'a str),
    Add(&'a str),
}

/// The unified diff turning `old` into `new`, hunks only; empty when the
/// lines are the same.
pub fn unified(old: &str, new: &str) -> String {
    let ops = edits(
        &old.lines().collect::<Vec<_>>(),
        &new.lines().collect::<Vec<_>>(),
    );
    let changed: Vec<usize> = (0..ops.len())
        .filter(|&i| !matches!(ops[i], Op::Keep(_)))
        .collect();
    let mut out = String::new();
    let mut next = 0;
    while next < changed.len() {
        let start = changed[next].saturating_sub(CONTEXT);
        let mut end = changed[next] + CONTEXT + 1;
        while next < changed.len() && changed[next] <= end + CONTEXT {
            end = changed[next] + CONTEXT + 1;
            next += 1;
        }
        hunk(&ops, start, end.min(ops.len()), &mut out);
    }
    out
}

/// The shortest edit script between `old` and `new`, from the longest
/// common subsequence of their lines.
fn edits<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Op<'a>> {
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

fn hunk(ops: &[Op], start: usize, end: usize, out: &mut String) {
    let before =
        |upto: usize, keep: fn(&Op) -> bool| ops[..upto].iter().filter(|o| keep(o)).count();
    let in_old = |o: &Op| !matches!(o, Op::Add(_));
    let in_new = |o: &Op| !matches!(o, Op::Del(_));
    let old_len = ops[start..end].iter().filter(|o| in_old(o)).count();
    let new_len = ops[start..end].iter().filter(|o| in_new(o)).count();
    let first = |len: usize, at: usize| if len == 0 { at } else { at + 1 };
    out.push_str(&format!(
        "@@ -{},{old_len} +{},{new_len} @@\n",
        first(old_len, before(start, in_old)),
        first(new_len, before(start, in_new)),
    ));
    for op in &ops[start..end] {
        match op {
            Op::Keep(l) => out.push_str(&format!(" {l}\n")),
            Op::Del(l) => out.push_str(&format!("-{l}\n")),
            Op::Add(l) => out.push_str(&format!("+{l}\n")),
        }
    }
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
    fn an_added_line_to_an_empty_file_starts_at_zero() {
        assert_eq!(unified("", "x\n"), "@@ -0,0 +1,1 @@\n+x\n");
    }
}
