// SPDX-License-Identifier: CC0-1.0

//! The DATETIME PATTERN notation of the JVM's datetime formatter, as the AWS APIs that take
//! such a pattern state one: `yyyy-MM-dd'T'HH:mm:ss`, where a run of one letter names one
//! field of the datetime, the run's length says how that field is written, and text between
//! single quotes is literal.
//!
//! Several services take a pattern in that notation from the caller — a log-event datetime
//! converter's match patterns and target format, a partition-projection `format` property —
//! and all of them have to read and write the same datetime under it. So the notation is
//! translated ONCE, here, into the `strftime`-like format the `chrono` library parses and
//! formats with: a letter one service translates cannot name a different field in another,
//! and the FIELDS a pattern names are reported beside the format so a caller can tell a
//! pattern that names a date from one that names only a time of day.
//!
//! A letter this module gives no field to makes its pattern name NOTHING
//! ([`chrono_format`] answers `None`) rather than silently matching something else, because
//! translating it to another field would read a different datetime than the pattern states.

/// How many letters of one run are read. A run says how wide the field is written, and the
/// widest run any letter of this notation is given meaning for is nine (`SSSSSSSSS`, the
/// nanosecond), so a longer run is a pattern this module does not translate.
const MAX_LETTER_RUN: usize = 9;

/// THE EMULATOR'S OWN CEILING on the bytes of one pattern it translates, not a limit AWS
/// publishes: a pattern is caller-supplied, and one longer than any datetime format is
/// written in translates to nothing rather than being walked.
pub const MAX_PATTERN_BYTES: usize = 256;

/// One translated pattern: the `chrono` format it became, and which parts of a datetime it
/// names — which is what decides how the text it reads resolves to an instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JvmPattern {
    /// The format string, in the `strftime`-like notation `chrono` parses and formats with.
    pub format: String,
    /// Whether the pattern names a calendar date.
    pub date: bool,
    /// Whether the pattern names a time of day.
    pub time: bool,
    /// Whether the pattern names the offset from UTC of its own text.
    pub offset: bool,
}

/// `pattern`, written in the letter notation above, translated into the `chrono` format that
/// reads and writes the same datetime.
///
/// A run of one letter is one field; text between single quotes is literal, and a doubled
/// quote is one literal quote. `None` for a pattern naming a letter this module gives no
/// field to — an era, a week-of-year, a quarter — and for one longer than
/// [`MAX_PATTERN_BYTES`].
#[must_use]
pub fn chrono_format(pattern: &str) -> Option<JvmPattern> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return None;
    }
    let mut translated = JvmPattern::default();
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\'' {
            // "Text can be quoted using single quotes to avoid interpretation"; two quotes
            // in a row are one literal quote.
            if characters.peek() == Some(&'\'') {
                characters.next();
                literal('\'', &mut translated.format);
                continue;
            }
            for quoted in characters.by_ref() {
                if quoted == '\'' {
                    break;
                }
                literal(quoted, &mut translated.format);
            }
            continue;
        }
        if !character.is_ascii_alphabetic() {
            literal(character, &mut translated.format);
            continue;
        }
        let mut run = 1;
        while characters.peek() == Some(&character) {
            characters.next();
            run += 1;
        }
        if run > MAX_LETTER_RUN {
            return None;
        }
        let field = field(character, run)?;
        translated.format.push_str(field.format);
        translated.date |= field.date;
        translated.time |= field.time;
        translated.offset |= field.offset;
    }
    (!translated.format.is_empty()).then_some(translated)
}

/// One literal character of a pattern, as the `chrono` format carries it: a per-cent sign
/// introduces a field there, so a literal one is doubled.
fn literal(character: char, into: &mut String) {
    if character == '%' {
        into.push('%');
    }
    into.push(character);
}

/// What one run of one pattern letter names: the `chrono` field it translates to, and which
/// part of a datetime that is.
struct Field {
    /// The `chrono` field.
    format: &'static str,
    /// Whether it is a field of the calendar date.
    date: bool,
    /// Whether it is a field of the time of day.
    time: bool,
    /// Whether it names the text's own offset from UTC.
    offset: bool,
}

/// The datetime field a run of `run` copies of `letter` names, and `None` for a letter this
/// module translates no field for.
///
/// The letters are the ones the JVM datetime formatter's pattern notation gives these
/// fields: `u`/`y` the year, `M`/`L` the month, `d` the day of the month, `D` the day of the
/// year, `E` the day of the week, `H` the hour of the day, `h` the clock hour with `a` the
/// half-of-day, `m` the minute, `s` the second, `S` the fraction of a second, `z` the zone
/// name and `Z`/`X`/`x` the offset from UTC.
fn field(letter: char, run: usize) -> Option<Field> {
    /// A field of the calendar date.
    const DATE: fn(&'static str) -> Option<Field> = |format| {
        Some(Field {
            format,
            date: true,
            time: false,
            offset: false,
        })
    };
    /// A field of the time of day.
    const TIME: fn(&'static str) -> Option<Field> = |format| {
        Some(Field {
            format,
            date: false,
            time: true,
            offset: false,
        })
    };
    /// A field naming the text's own offset from UTC.
    const OFFSET: fn(&'static str) -> Option<Field> = |format| {
        Some(Field {
            format,
            date: false,
            time: false,
            offset: true,
        })
    };
    /// A field that names neither the date nor the time nor an offset: the day of the week,
    /// and a zone NAME, which no reader of this notation resolves to an offset.
    const NEITHER: fn(&'static str) -> Option<Field> = |format| {
        Some(Field {
            format,
            date: false,
            time: false,
            offset: false,
        })
    };
    match (letter, run) {
        // The year: two letters is the two-digit year, anything else the full year.
        ('u' | 'y', 2) => DATE("%y"),
        ('u' | 'y', _) => DATE("%Y"),
        // The month: one or two letters is the number, three the short name, more the full
        // name.
        ('M' | 'L', 1 | 2) => DATE("%m"),
        ('M' | 'L', 3) => DATE("%b"),
        ('M' | 'L', _) => DATE("%B"),
        ('d', _) => DATE("%d"),
        ('D', _) => DATE("%j"),
        // The day of the week names no day of any month, so it constrains no date.
        ('E', 1..=3) => NEITHER("%a"),
        ('E', _) => NEITHER("%A"),
        ('H', _) => TIME("%H"),
        ('h', _) => TIME("%I"),
        ('a', _) => TIME("%p"),
        ('m', _) => TIME("%M"),
        ('s', _) => TIME("%S"),
        // The fraction of a second, written to the width the run asks for: the three widths
        // the renderer writes are the milli-, micro- and nanosecond.
        ('S', 1..=3) => TIME("%3f"),
        ('S', 4..=6) => TIME("%6f"),
        ('S', _) => TIME("%9f"),
        // The zone NAME ("PST") names no offset a reader can resolve, so it is read and
        // written as the name it is.
        ('z', _) => NEITHER("%Z"),
        // The offset from UTC: three or more letters write it with the colon
        // (`+01:00`), fewer without (`+0100`).
        ('Z' | 'X' | 'x', 1 | 2) => OFFSET("%z"),
        ('Z' | 'X' | 'x', _) => OFFSET("%:z"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{chrono_format, MAX_PATTERN_BYTES};

    /// RULE: a run of one letter translates to the field that letter names, written to the
    /// width the run asks for, and quoted text stays literal.
    ///
    /// The rows differ along the field the letter names, the length of its run, the quoting
    /// around literal text, and whether the pattern names a date, a time or an offset.
    #[test]
    fn each_letter_run_translates_to_the_field_it_names() {
        // (the pattern, the format it becomes, (names a date, a time, an offset))
        let cases: Vec<(&str, &str, (bool, bool, bool))> = vec![
            ("yyyy-MM-dd", "%Y-%m-%d", (true, false, false)),
            ("yy/M/d", "%y/%m/%d", (true, false, false)),
            ("yyyy-MMM-dd", "%Y-%b-%d", (true, false, false)),
            ("HH:mm:ss", "%H:%M:%S", (false, true, false)),
            (
                "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'",
                "%Y-%m-%dT%H:%M:%S.%3fZ",
                (true, true, false),
            ),
            (
                "yyyy-MM-dd'T'HH:mm:ssXXX",
                "%Y-%m-%dT%H:%M:%S%:z",
                (true, true, true),
            ),
            ("hh:mm a", "%I:%M %p", (false, true, false)),
        ];

        for (pattern, expected, (date, time, offset)) in cases {
            let translated = chrono_format(pattern)
                .unwrap_or_else(|| panic!("case {pattern:?}: expected a translation"));
            assert_eq!(
                translated.format, expected,
                "case {pattern:?}: expected the format {expected:?}, got {:?}",
                translated.format
            );
            assert_eq!(
                (translated.date, translated.time, translated.offset),
                (date, time, offset),
                "case {pattern:?}: unexpected fields in {translated:?}"
            );
        }
    }

    /// RULE: a pattern this notation gives no field for names nothing at all, rather than
    /// matching something else.
    #[test]
    fn a_pattern_naming_an_untranslated_field_names_nothing() {
        let over_long = "y".repeat(MAX_PATTERN_BYTES + 1);
        // (case, the pattern)
        let cases: Vec<(&str, &str)> = vec![
            ("a quarter, which names no field here", "yyyy-QQQ"),
            ("an era", "G yyyy"),
            ("a week of the year", "yyyy-ww"),
            ("a run past the widest one read", "SSSSSSSSSS"),
            ("nothing at all", ""),
            ("a pattern past the ceiling", over_long.as_str()),
        ];

        for (case, pattern) in cases {
            let translated = chrono_format(pattern);
            assert!(
                translated.is_none(),
                "case {case:?}: expected {pattern:?} to name nothing, got {translated:?}"
            );
        }
    }
}
