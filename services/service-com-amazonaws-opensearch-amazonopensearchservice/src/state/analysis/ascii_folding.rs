// SPDX-License-Identifier: CC0-1.0

//! The `asciifolding` token filter: "converts alphabetic, numeric, and symbolic characters
//! that are not in the Basic Latin Unicode block (first 127 ASCII characters) to their ASCII
//! equivalent, if one exists" — which is what makes an accent-insensitive index possible, a
//! query for `cafe` finding a document that holds `café`.
//!
//! The equivalent is taken from PUBLISHED Unicode data rather than from a character table
//! written here: a character's compatibility decomposition names the ASCII characters it is
//! composed of, and the combining marks of that decomposition are what an accent IS, so a
//! letter whose decomposition is one or more ASCII characters plus marks folds to those
//! characters. A character whose decomposition reaches no ASCII character at all has no ASCII
//! equivalent and is LEFT AS IT IS, which is what the filter documents for one.
//!
//! The Latin letters and ligatures that are a letter in their own right decompose to nothing
//! — they are not a base letter plus a mark — so each names the characters it stands for
//! ([`EQUIVALENTS`]).

use ::unicode_normalization::char::{canonical_combining_class, decompose_compatible};

use super::{Token, TOKEN_CEILING};

/// The characters outside Basic Latin whose ASCII equivalent is not a decomposition: a Latin
/// letter or ligature that stands for ASCII letters rather than being one of them carrying a
/// mark.
///
/// Each entry is the letter and the letters it stands for; the upper-case form stands for the
/// upper-case letters, as the folding of a letter keeps its case.
const EQUIVALENTS: [(char, &str); 25] = [
    ('Æ', "AE"),
    ('æ', "ae"),
    ('Œ', "OE"),
    ('œ', "oe"),
    ('Ø', "O"),
    ('ø', "o"),
    ('Đ', "D"),
    ('đ', "d"),
    ('Ð', "D"),
    ('ð', "d"),
    ('Þ', "TH"),
    ('þ', "th"),
    ('ß', "ss"),
    ('ẞ', "SS"),
    ('Ł', "L"),
    ('ł', "l"),
    ('Ħ', "H"),
    ('ħ', "h"),
    ('Ŋ', "N"),
    ('ŋ', "n"),
    ('Ŧ', "T"),
    ('ŧ', "t"),
    ('ı', "i"),
    ('ĸ', "k"),
    ('ſ', "s"),
];

/// `text` with every character outside Basic Latin replaced by its ASCII equivalent, and one
/// having none left as it is.
pub(super) fn folded_to_ascii(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii() {
            folded.push(ch);
            continue;
        }
        match equivalent(ch) {
            Some(ascii) => folded.push_str(&ascii),
            None => folded.push(ch),
        }
    }
    folded
}

/// The ASCII equivalent of ONE character outside Basic Latin, where it has one.
///
/// The decomposition is what names it: the ASCII characters it is composed of are kept and
/// the combining marks are dropped, while a decomposition reaching a character that is
/// neither — a letter of another script, a symbol with no ASCII counterpart — means this
/// character has no ASCII equivalent at all.
fn equivalent(ch: char) -> Option<String> {
    if let Some((_, ascii)) = EQUIVALENTS.iter().find(|(letter, _)| *letter == ch) {
        return Some((*ascii).to_string());
    }
    let mut ascii = String::new();
    let mut unmapped = false;
    decompose_compatible(ch, |part| {
        if part.is_ascii() {
            ascii.push(part);
        } else if canonical_combining_class(part) == 0 {
            // Not a mark: a character of the decomposition that is not ASCII either, so what
            // this character stands for is not spelled in ASCII.
            unmapped = true;
        }
    });
    (!unmapped && !ascii.is_empty()).then_some(ascii)
}

/// `tokens` with each token's text folded.
///
/// With `preserve_original` the token as it was written is kept BESIDE the folded one, at the
/// same position and offsets — which is what that option is for: a query for the accented
/// spelling and one for the folded spelling both find the document. The stream is bounded as
/// it is built ([`TOKEN_CEILING`]), so preserving the originals cannot take it past what any
/// other filter may produce.
pub(super) fn applied(tokens: Vec<Token>, preserve_original: bool) -> Vec<Token> {
    let mut produced: Vec<Token> = Vec::new();
    for token in tokens {
        if produced.len() >= TOKEN_CEILING {
            return produced;
        }
        let folded = folded_to_ascii(&token.token);
        if folded.is_empty() {
            continue;
        }
        let original = (preserve_original && folded != token.token).then(|| token.clone());
        produced.push(Token { token: folded, ..token });
        if let Some(original) = original {
            if produced.len() >= TOKEN_CEILING {
                return produced;
            }
            produced.push(original);
        }
    }
    produced
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a character outside Basic Latin folds to the ASCII characters it stands for,
    /// and one standing for none is left as it is.
    #[test]
    fn a_character_folds_to_its_ascii_equivalent_or_is_left_alone() {
        // (the text, what it folds to) — rows spanning the forms the rule covers: a
        // precomposed accented letter, a letter that stands for several, a ligature, a
        // compatibility form, a combining mark standing on its own (which is no character
        // outside Basic Latin standing for an ASCII one, so it stays), and a script with no
        // ASCII equivalent at all.
        let rows: [(&str, &str); 8] = [
            ("café", "cafe"),
            ("cafe\u{301}", "cafe\u{301}"),
            ("ÀÉÎÕÜ", "AEIOU"),
            ("Straße", "Strasse"),
            ("Æon", "AEon"),
            ("ﬁnance", "finance"),
            ("naïve-piñata", "naive-pinata"),
            ("日本語", "日本語"),
        ];
        for (text, expected) in rows {
            let folded = folded_to_ascii(text);
            assert_eq!(
                folded, expected,
                "case {text:?}: expected {expected:?}, got {folded:?}"
            );
        }
    }
}
