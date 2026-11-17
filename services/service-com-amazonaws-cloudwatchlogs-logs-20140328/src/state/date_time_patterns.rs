// SPDX-License-Identifier: CC0-1.0

//! Reading and writing a datetime under the PATTERNS a `dateTimeConverter` processor states.
//!
//! `DateTimeConverter` "converts a datetime string into a format that you specify": it reads
//! "a list of patterns to match against the `source` field" (`matchPatterns`), in "the time
//! zone of the source field" (`sourceTimezone`, "if you omit this, the default used is the
//! UTC zone"), and writes "the datetime format to use for the converted data in the target
//! field" (`targetFormat`, defaulting to [`DEFAULT_TARGET_FORMAT`]) in "the time zone of the
//! target field" (`targetTimezone`, UTC when omitted).
//!
//! Both kinds of pattern are written in the letter notation the pattern members of the
//! JVM's datetime formatter use — `yyyy-MM-dd'T'HH:mm:ss`, where a run of one letter names
//! one field of the datetime, a run's length says how it is written, and text between single
//! quotes is literal. [`chrono_format`] translates one such pattern into the `chrono` format
//! the parsing and formatting below are done with, so the reading of the letters lives in
//! ONE place and a letter this module does not translate makes its pattern name nothing
//! rather than silently matching something else.
//!
//! What one conversion costs is bounded BEFORE any event is read: a [`Conversion`] is
//! prepared once from the stored configuration — the two zones resolved, and every pattern
//! translated, at most [`MAX_MATCH_PATTERNS`] of them and each at most
//! [`MAX_PATTERN_BYTES`] long — so applying it to one event is at most that many parse
//! attempts and one rendering, however many events a batch carries and however long the
//! patterns the configuration states are.
//!
//! The zone names are the IANA zone ids the zone database holds (`America/New_York`), so a
//! conversion out of a zone that observes daylight saving shifts by that zone's offset AT
//! THE INSTANT the source names rather than by one fixed offset.

use std::fmt::Write as _;

use ::chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use ::chrono_tz::Tz;

/// The format a conversion writes when the processor states no `targetFormat`: "if you omit
/// this, the default of `yyyy-MM-dd'T'HH:mm:ss.SSS'Z` is used"
/// (`DateTimeConverter$targetFormat`) — the ISO-8601 instant to the millisecond, with the
/// zone designator as literal text.
pub const DEFAULT_TARGET_FORMAT: &str = "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'";

/// How many letters of one run are read. A run says how wide the field is written, and the
/// widest run any letter of this notation is given meaning for is nine (`SSSSSSSSS`, the
/// nanosecond), so a longer run is a pattern this module does not translate.
const MAX_LETTER_RUN: usize = 9;

/// How many patterns one conversion reads its source text under: the ceiling the model sets
/// on `DateTimeConverter$matchPatterns` (`MatchPatterns`, len 1..5), so a configuration
/// cannot make one event cost more parse attempts than that.
const MAX_MATCH_PATTERNS: usize = 5;

/// THE EMULATOR'S OWN CEILING on the bytes of one pattern it translates, not a limit AWS
/// publishes: `MatchPattern` carries no maximum length of its own, so a pattern longer than
/// any datetime format is written in translates to nothing rather than being walked.
/// `TargetFormat` is modelled at 64 characters, and this leaves generous room beyond it.
const MAX_PATTERN_BYTES: usize = 256;

/// One `dateTimeConverter`'s conversion, PREPARED from its configuration: the zones it reads
/// and writes in, the patterns it reads its source text under, and the format it writes.
///
/// Prepared once per stored processor ([`Conversion::prepared`]) rather than once per event,
/// because every part of it is a property of the configuration and none of any event.
#[derive(Debug)]
pub struct Conversion {
    /// The patterns the source text is read under, in the order the processor lists them,
    /// translated; at most [`MAX_MATCH_PATTERNS`] of them.
    patterns: Vec<Translated>,
    /// "The time zone of the source field."
    source: Tz,
    /// "The time zone of the target field."
    target: Tz,
    /// "The datetime format to use for the converted data in the target field."
    format: Translated,
}

impl Conversion {
    /// The conversion `match_patterns`, the two zone names and `target_format` configure.
    ///
    /// `None` when there is no conversion to make: a zone name the zone database does not
    /// hold, a target format naming a field this notation is not translated for, and a
    /// configuration no pattern of which translates.
    pub fn prepared(
        match_patterns: &[String],
        source_timezone: Option<&str>,
        target_timezone: Option<&str>,
        target_format: Option<&str>,
    ) -> Option<Self> {
        let patterns: Vec<Translated> = match_patterns
            .iter()
            .take(MAX_MATCH_PATTERNS)
            .filter_map(|pattern| chrono_format(pattern))
            .collect();
        if patterns.is_empty() {
            return None;
        }
        Some(Self {
            patterns,
            source: zone(source_timezone)?,
            target: zone(target_timezone)?,
            format: chrono_format(target_format.unwrap_or(DEFAULT_TARGET_FORMAT))?,
        })
    }

    /// `text` read under the first of this conversion's patterns that reads it whole, in its
    /// source zone, written in its target zone under its format.
    ///
    /// `None` when no pattern reads the text — the conversion then writes nothing, and the
    /// event keeps the fields it already had.
    pub fn applied(&self, text: &str) -> Option<String> {
        let text = text.trim();
        let instant = self
            .patterns
            .iter()
            .find_map(|pattern| instant_of(text, pattern, self.source))?;
        formatted(instant, self.target, &self.format)
    }
}

/// The zone a `sourceTimezone` / `targetTimezone` names: the zone database's entry for it,
/// and UTC for a processor that names none ("if you omit this, the default used is the UTC
/// zone"). `None` for a name no zone database holds, which names no offset to shift by.
fn zone(named: Option<&str>) -> Option<Tz> {
    match named {
        None => Some(::chrono_tz::UTC),
        Some(named) => named.parse::<Tz>().ok(),
    }
}

/// The INSTANT `text` names when read under the translated `pattern` in `zone`, and `None`
/// when the pattern does not read the whole of it.
///
/// A pattern that carries the offset or the zone of its own text fixes the instant itself, so
/// `zone` is then not consulted; one that names a date and a time names a LOCAL datetime,
/// which the zone resolves — at a local time that zone skips (the gap a daylight-saving shift
/// leaves) nothing is named, and at one it repeats the EARLIER of the two instants is taken,
/// the way a reader resolving a local time to one instant has to choose.
fn instant_of(text: &str, pattern: &Translated, zone: Tz) -> Option<DateTime<Utc>> {
    if pattern.offset {
        return DateTime::parse_from_str(text, &pattern.format)
            .ok()
            .map(|instant| instant.with_timezone(&Utc));
    }
    if pattern.date && pattern.time {
        let local = NaiveDateTime::parse_from_str(text, &pattern.format).ok()?;
        return zone
            .from_local_datetime(&local)
            .earliest()
            .map(|instant| instant.with_timezone(&Utc));
    }
    if pattern.date {
        let date = NaiveDate::parse_from_str(text, &pattern.format).ok()?;
        return zone
            .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|instant| instant.with_timezone(&Utc));
    }
    // A pattern naming only a time of day names no day, so it names no instant.
    None
}

/// `instant` as `zone` sees it, written under the translated `pattern`.
fn formatted(instant: DateTime<Utc>, zone: Tz, pattern: &Translated) -> Option<String> {
    let mut written = String::new();
    // Written through `write!` rather than `to_string`, so a format the renderer rejects
    // answers `None` here instead of panicking inside a `Display`.
    write!(
        written,
        "{}",
        instant.with_timezone(&zone).format(&pattern.format)
    )
    .ok()?;
    Some(written)
}

/// One translated pattern: the `chrono` format it became, and which parts of a datetime it
/// names — which is what decides how the text it reads is resolved to an instant.
#[derive(Debug, Default, PartialEq, Eq)]
struct Translated {
    /// The format string, in the `strftime`-like notation `chrono` parses and formats with.
    format: String,
    /// Whether the pattern names a calendar date.
    date: bool,
    /// Whether the pattern names a time of day.
    time: bool,
    /// Whether the pattern names the offset from UTC of its own text.
    offset: bool,
}

/// `pattern`, written in the letter notation a `dateTimeConverter` states its patterns in,
/// translated into the `chrono` format that reads and writes the same datetime.
///
/// A run of one letter is one field; text between single quotes is literal, and a doubled
/// quote is one literal quote. `None` for a pattern naming a letter this module gives no
/// field to — an era, a week-of-year, a quarter, a localized zone name — because translating
/// it to another field would read a different datetime than the pattern states, and for one
/// longer than [`MAX_PATTERN_BYTES`].
fn chrono_format(pattern: &str) -> Option<Translated> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return None;
    }
    let mut translated = Translated::default();
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
    format: &'static str,
    date: bool,
    time: bool,
    offset: bool,
}

/// The datetime field a run of `run` copies of `letter` names, and `None` for a letter this
/// module translates no field for.
///
/// The letters are the ones the JVM datetime formatter's pattern notation gives these fields,
/// which is the notation `DateTimeConverter$matchPatterns` and `$targetFormat` are written
/// in: `u`/`y` the year, `M`/`L` the month, `d` the day of the month, `D` the day of the year,
/// `E` the day of the week, `H` the hour of the day, `h` the clock hour with `a` the
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
    use super::{Conversion, DEFAULT_TARGET_FORMAT, MAX_MATCH_PATTERNS, MAX_PATTERN_BYTES};

    /// The conversion a case prepares, which must be preparable at all.
    fn prepared(
        case: &str,
        patterns: &[&str],
        source: Option<&str>,
        target: Option<&str>,
        format: Option<&str>,
    ) -> Conversion {
        let patterns: Vec<String> = patterns.iter().map(|p| (*p).to_string()).collect();
        Conversion::prepared(&patterns, source, target, format)
            .unwrap_or_else(|| panic!("case {case:?}: the configuration prepares a conversion"))
    }

    /// A zone that observes daylight saving shifts by the offset it is at ON the instant the
    /// source names, so the same wall-clock time converts differently in summer and winter.
    #[test]
    fn shifts_by_the_zone_offset_at_the_source_instant() {
        let conversion = prepared(
            "a daylight-saving source zone",
            &["yyyy-MM-dd HH:mm:ss"],
            Some("America/New_York"),
            Some("UTC"),
            Some("yyyy-MM-dd HH:mm:ss"),
        );
        for (text, expected) in [
            ("2024-07-05 09:07:09", "2024-07-05 13:07:09"),
            ("2024-03-05 09:07:09", "2024-03-05 14:07:09"),
        ] {
            let actual = conversion.applied(text);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {text:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The format a processor stating none writes: the ISO-8601 instant to the millisecond.
    #[test]
    fn writes_the_default_target_format() {
        let actual = prepared(
            "the documented defaults",
            &["yyyy-MM-dd'T'HH:mm:ss"],
            None,
            None,
            None,
        )
        .applied("2024-03-05T09:07:09");
        assert_eq!(
            actual.as_deref(),
            Some("2024-03-05T09:07:09.000Z"),
            "the default {DEFAULT_TARGET_FORMAT:?} writes the millisecond and a literal Z, got {actual:?}"
        );
    }

    /// The first pattern that reads the text whole is the one the conversion is made under,
    /// and a configuration states at most [`MAX_MATCH_PATTERNS`] of them.
    #[test]
    fn reads_the_first_matching_pattern() {
        let patterns = ["yyyy-MM-dd HH:mm:ss", "dd/MM/yyyy HH:mm"];
        assert!(
            patterns.len() <= MAX_MATCH_PATTERNS,
            "the case states no more patterns than a configuration may, got {}",
            patterns.len()
        );
        let actual = prepared(
            "two patterns",
            &patterns,
            Some("UTC"),
            Some("UTC"),
            Some("yyyy-MM-dd'T'HH:mm:ss"),
        )
        .applied("05/03/2024 09:07");
        assert_eq!(
            actual.as_deref(),
            Some("2024-03-05T09:07:00"),
            "the second pattern reads the text, got {actual:?}"
        );
    }

    /// Text no pattern reads converts nothing, and a configuration that names no readable
    /// pattern, an unknown zone or an untranslatable target format prepares no conversion at
    /// all — so nothing of it is walked per event.
    #[test]
    fn converts_nothing_it_cannot_read() {
        let readable = prepared(
            "a readable configuration",
            &["yyyy-MM-dd HH:mm:ss"],
            Some("UTC"),
            Some("UTC"),
            Some("yyyy-MM-dd HH:mm:ss"),
        );
        let unreadable = readable.applied("last Tuesday");
        assert!(
            unreadable.is_none(),
            "case \"text no pattern reads\": expected no conversion, got {unreadable:?}"
        );

        let over_long = "y".repeat(MAX_PATTERN_BYTES + 1);
        for (case, pattern, source, format) in [
            ("an untranslated letter in a pattern", "QQQQ", "UTC", "yyyy"),
            (
                "a pattern past the ceiling",
                over_long.as_str(),
                "UTC",
                "yyyy",
            ),
            (
                "a zone no database holds",
                "yyyy-MM-dd",
                "Mars/Olympus",
                "yyyy",
            ),
            (
                "an untranslated letter in the target format",
                "yyyy-MM-dd",
                "UTC",
                "QQQQ",
            ),
        ] {
            let conversion = Conversion::prepared(
                &[pattern.to_string()],
                Some(source),
                Some("UTC"),
                Some(format),
            );
            assert!(
                conversion.is_none(),
                "case {case:?}: expected no conversion to be prepared, got {conversion:?}"
            );
        }
    }

    /// A pattern carrying the text's own offset fixes the instant itself, whatever the source
    /// zone says.
    #[test]
    fn honours_an_offset_the_text_carries() {
        let actual = prepared(
            "an offset in the text",
            &["yyyy-MM-dd'T'HH:mm:ssXXX"],
            Some("America/New_York"),
            Some("UTC"),
            Some("yyyy-MM-dd'T'HH:mm:ss"),
        )
        .applied("2024-03-05T09:07:09+02:00");
        assert_eq!(
            actual.as_deref(),
            Some("2024-03-05T07:07:09"),
            "the offset in the text names the instant, got {actual:?}"
        );
    }
}
