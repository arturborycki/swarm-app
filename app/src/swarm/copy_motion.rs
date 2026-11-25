//! Copy mode's motions within one row of text, as in vi: `w`, `b`, `e`, `0`, `^` and `$`.
//! A motion that would leave the row returns `None`, and copy mode continues on the next or
//! previous row.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() || c == '\0' {
        Class::Blank
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// `w`: the start of the next word after `col`.
pub fn word_forward(row: &[char], col: usize) -> Option<usize> {
    let mut i = col;
    if let Some(&c) = row.get(i) {
        let start = class(c);
        if start != Class::Blank {
            while row.get(i).is_some_and(|&c| class(c) == start) {
                i += 1;
            }
        }
    }
    while row.get(i).is_some_and(|&c| class(c) == Class::Blank) {
        i += 1;
    }
    (i < row.len()).then_some(i)
}

/// `b`: the start of the word before `col`, or of the word `col` is inside.
pub fn word_backward(row: &[char], col: usize) -> Option<usize> {
    let mut i = col.min(row.len());
    while i > 0 && class(row[i - 1]) == Class::Blank {
        i -= 1;
    }
    if i == 0 {
        return None;
    }
    let word = class(row[i - 1]);
    while i > 0 && class(row[i - 1]) == word {
        i -= 1;
    }
    Some(i)
}

/// `e`: the end of the word after `col`, or of the word `col` is inside.
pub fn word_end(row: &[char], col: usize) -> Option<usize> {
    end_from(row, col + 1)
}

/// `e` arriving from the row above: the end of the row's first word.
pub fn first_word_end(row: &[char]) -> Option<usize> {
    end_from(row, 0)
}

fn end_from(row: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while row.get(i).is_some_and(|&c| class(c) == Class::Blank) {
        i += 1;
    }
    let word = class(*row.get(i)?);
    while row.get(i + 1).is_some_and(|&c| class(c) == word) {
        i += 1;
    }
    Some(i)
}

/// `^`: the first character that isn't blank (0 on a blank row).
pub fn first_non_blank(row: &[char]) -> usize {
    row.iter().position(|&c| class(c) != Class::Blank).unwrap_or(0)
}

/// `$`: the last character that isn't blank (0 on a blank row).
pub fn last_non_blank(row: &[char]) -> usize {
    row.iter().rposition(|&c| class(c) != Class::Blank).unwrap_or(0)
}

/// Whether the row has nothing but blanks.
pub fn is_blank(row: &[char]) -> bool {
    row.iter().all(|&c| class(c) == Class::Blank)
}

#[cfg(test)]
#[path = "copy_motion_tests.rs"]
mod tests;
