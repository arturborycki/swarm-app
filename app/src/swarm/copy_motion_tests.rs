use super::*;

fn row(s: &str) -> Vec<char> {
    s.chars().collect()
}

#[test]
fn word_forward_skips_the_word_and_blanks() {
    let r = row("cargo build --release  ");
    assert_eq!(word_forward(&r, 0), Some(6));
    assert_eq!(word_forward(&r, 3), Some(6));
    assert_eq!(word_forward(&r, 6), Some(12), "punctuation starts a word");
    assert_eq!(word_forward(&r, 12), Some(14));
    assert_eq!(word_forward(&r, 14), None, "the last word: next row");
    assert_eq!(word_forward(&row("   x"), 0), Some(3));
}

#[test]
fn word_backward_goes_to_word_starts() {
    let r = row("cargo build --release");
    assert_eq!(word_backward(&r, 8), Some(6));
    assert_eq!(word_backward(&r, 6), Some(0));
    assert_eq!(word_backward(&r, 14), Some(12));
    assert_eq!(word_backward(&r, 0), None);
    assert_eq!(word_backward(&row("   x"), 3), None);
}

#[test]
fn word_end_goes_to_word_ends() {
    let r = row("cargo build --release");
    assert_eq!(word_end(&r, 0), Some(4));
    assert_eq!(word_end(&r, 4), Some(10));
    assert_eq!(word_end(&r, 10), Some(13));
    assert_eq!(word_end(&r, 20), None);
    assert_eq!(first_word_end(&row("  ab cd")), Some(3));
    assert_eq!(first_word_end(&row("x")), Some(0));
    assert_eq!(first_word_end(&row("   ")), None);
}

#[test]
fn line_positions() {
    let r = row("  ls -la   ");
    assert_eq!(first_non_blank(&r), 2);
    assert_eq!(last_non_blank(&r), 7);
    assert_eq!(first_non_blank(&row("    ")), 0);
    assert!(is_blank(&row("  \0 ")));
    assert!(!is_blank(&r));
}
