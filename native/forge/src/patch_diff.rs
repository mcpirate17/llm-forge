//! Strict unified diff with bounded, unique line-offset recovery.
use anyhow::{bail, ensure, Context, Result};
use conductor_native::syntax::MAX_SOURCE_BYTES;
use std::path::Path;

struct Hunk {
    old_start: usize,
    new_start: usize,
    old: Vec<String>,
    new: Vec<String>,
}

pub(super) fn apply(source: &str, patch: &str, path: &Path, offset: usize) -> Result<String> {
    ensure!(
        patch.len() <= MAX_SOURCE_BYTES && !patch.contains('\0'),
        "diff byte/NUL limit exceeded"
    );
    ensure!(offset <= 4096, "max_offset_lines must be 0..4096");
    let hunks = parse(patch, path)?;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    let mut declared_end = 0;
    let mut delta: i64 = 0;
    let mut comparison_budget = 32 * MAX_SOURCE_BYTES;
    for hunk in hunks {
        ensure!(
            hunk.old_start >= declared_end,
            "diff hunks overlap or are out of order"
        );
        ensure!(
            hunk.new_start as i64 == hunk.old_start as i64 + delta,
            "inconsistent new hunk location"
        );
        let position = locate(&lines, &hunk, cursor, offset, &mut comparison_budget)?;
        append_lines(&mut output, lines[cursor..position].iter().copied())?;
        append_lines(&mut output, hunk.new.iter().map(String::as_str))?;
        cursor = position + hunk.old.len();
        declared_end = hunk.old_start + hunk.old.len();
        delta += hunk.new.len() as i64 - hunk.old.len() as i64;
    }
    append_lines(&mut output, lines[cursor..].iter().copied())?;
    Ok(output)
}

fn append_lines<'a>(out: &mut String, lines: impl Iterator<Item = &'a str>) -> Result<()> {
    for line in lines {
        ensure!(
            out.len() + line.len() <= MAX_SOURCE_BYTES,
            "patched source byte limit exceeded"
        );
        out.push_str(line);
    }
    Ok(())
}

fn matches(lines: &[&str], hunk: &Hunk, position: usize, budget: &mut usize) -> Result<bool> {
    let Some(actual) = lines.get(position..position.saturating_add(hunk.old.len())) else {
        return Ok(false);
    };
    for (actual, expected) in actual.iter().zip(&hunk.old) {
        let cost = actual.len().min(expected.len()).max(1);
        ensure!(
            *budget >= cost,
            "diff comparison budget exceeded; reduce offset or hunk size"
        );
        *budget -= cost;
        if *actual != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn locate(
    lines: &[&str],
    hunk: &Hunk,
    cursor: usize,
    offset: usize,
    budget: &mut usize,
) -> Result<usize> {
    if hunk.old_start >= cursor && matches(lines, hunk, hunk.old_start, budget)? {
        return Ok(hunk.old_start);
    }
    ensure!(
        offset != 0 && !hunk.old.is_empty(),
        "hunk does not match declared location"
    );
    let low = hunk.old_start.saturating_sub(offset).max(cursor);
    let high = hunk.old_start.saturating_add(offset).min(lines.len());
    let mut found = None;
    for position in low..=high {
        if matches(lines, hunk, position, budget)? {
            ensure!(
                found.is_none(),
                "ambiguous offset match; supply more exact context"
            );
            found = Some(position);
        }
    }
    found.context("no matching hunk in offset window")
}

fn parse(patch: &str, path: &Path) -> Result<Vec<Hunk>> {
    let mut lines = patch.split_inclusive('\n').peekable();
    if lines.peek().is_some_and(|line| line.starts_with("--- ")) {
        check_path(lines.next().unwrap(), "--- ", "a/", path)?;
        check_path(
            lines.next().context("missing new diff path")?,
            "+++ ",
            "b/",
            path,
        )?;
    }
    let mut hunks = Vec::new();
    while let Some(header) = lines.next() {
        ensure!(hunks.len() < 1024, "diff hunk limit exceeded");
        let (old_start, old_count, new_start, new_count) = header_ranges(header)?;
        let mut hunk = Hunk {
            old_start,
            new_start,
            old: Vec::new(),
            new: Vec::new(),
        };
        let mut previous = None;
        while let Some(line) = lines.peek().copied() {
            if line.starts_with("@@ ") {
                break;
            }
            lines.next();
            if line.trim_end_matches(['\r', '\n']) == "\\ No newline at end of file" {
                strip_newline(
                    &mut hunk,
                    previous.take().context("misplaced no-newline marker")?,
                )?;
                continue;
            }
            let (tag, content) = line.split_at_checked(1).context("empty diff line")?;
            match tag {
                " " => {
                    hunk.old.push(content.to_owned());
                    hunk.new.push(content.to_owned());
                }
                "-" => hunk.old.push(content.to_owned()),
                "+" => hunk.new.push(content.to_owned()),
                _ => bail!("unsupported diff record; expected context, addition or removal"),
            }
            previous = Some(tag.as_bytes()[0]);
            ensure!(
                hunk.old.len() <= old_count && hunk.new.len() <= new_count,
                "hunk line counts exceed header"
            );
        }
        ensure!(
            hunk.old.len() == old_count && hunk.new.len() == new_count,
            "hunk line counts differ from header"
        );
        ensure!(old_count != 0 || new_count != 0, "empty hunk");
        hunks.push(hunk);
    }
    ensure!(!hunks.is_empty(), "diff contains no hunks");
    Ok(hunks)
}

fn strip_newline(hunk: &mut Hunk, tag: u8) -> Result<()> {
    for lines in [&mut hunk.old, &mut hunk.new]
        .into_iter()
        .enumerate()
        .filter_map(|(i, lines)| {
            ((i == 0 && tag != b'+') || (i == 1 && tag != b'-')).then_some(lines)
        })
    {
        let last = lines.last_mut().context("no preceding diff content")?;
        ensure!(last.ends_with('\n'), "duplicate no-newline marker");
        last.pop();
    }
    Ok(())
}

fn check_path(line: &str, tag: &str, prefix: &str, expected: &Path) -> Result<()> {
    let raw = line
        .trim_end_matches(['\r', '\n'])
        .strip_prefix(tag)
        .context("invalid diff file header")?;
    let name = raw.split_once('\t').map_or(raw, |(name, _)| name);
    let name = name.strip_prefix(prefix).unwrap_or(name);
    ensure!(
        Path::new(name) == expected,
        "diff path differs from requested file"
    );
    Ok(())
}

fn header_ranges(header: &str) -> Result<(usize, usize, usize, usize)> {
    let body = header
        .strip_prefix("@@ ")
        .context("expected unified diff hunk header")?;
    let (ranges, suffix) = body.split_once(" @@").context("unclosed hunk header")?;
    ensure!(
        suffix.is_empty() || suffix.starts_with([' ', '\r', '\n']),
        "malformed hunk suffix"
    );
    let (old, new) = ranges.split_once(' ').context("missing new hunk range")?;
    let (old_start, old_count) = range(old, '-')?;
    let (new_start, new_count) = range(new, '+')?;
    Ok((old_start, old_count, new_start, new_count))
}

fn range(text: &str, prefix: char) -> Result<(usize, usize)> {
    let text = text
        .strip_prefix(prefix)
        .context("incorrect range prefix")?;
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    ensure!(
        !start.is_empty()
            && !count.is_empty()
            && start
                .bytes()
                .chain(count.bytes())
                .all(|b| b.is_ascii_digit()),
        "invalid range integer"
    );
    let start = start.parse::<usize>()?;
    let count = count.parse::<usize>()?;
    ensure!(
        start <= MAX_SOURCE_BYTES && count <= MAX_SOURCE_BYTES,
        "hunk range limit exceeded"
    );
    if count == 0 {
        Ok((start, count))
    } else {
        Ok((
            start
                .checked_sub(1)
                .context("nonempty hunk starts at zero")?,
            count,
        ))
    }
}
