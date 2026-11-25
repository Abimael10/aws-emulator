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
//! quotes is literal. That notation is translated into the `chrono` format the parsing and
//! formatting below are done with by the ONE translation of it this workspace has
//! ([`service_common::java_date_pattern`]), so a letter this service reads cannot name a
//! different field in another.
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
use ::service_common::java_date_pattern::{chrono_format, JvmPattern};

/// The format a conversion writes when the processor states no `targetFormat`: "if you omit
/// this, the default of `yyyy-MM-dd'T'HH:mm:ss.SSS'Z` is used"
/// (`DateTimeConverter$targetFormat`) — the ISO-8601 instant to the millisecond, with the
/// zone designator as literal text.
pub const DEFAULT_TARGET_FORMAT: &str = "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'";

/// How many patterns one conversion reads its source text under: the ceiling the model sets
/// on `DateTimeConverter$matchPatterns` (`MatchPatterns`, len 1..5), so a configuration
/// cannot make one event cost more parse attempts than that.
const MAX_MATCH_PATTERNS: usize = 5;

/// One `dateTimeConverter`'s conversion, PREPARED from its configuration: the zones it reads
/// and writes in, the patterns it reads its source text under, and the format it writes.
///
/// Prepared once per stored processor ([`Conversion::prepared`]) rather than once per event,
/// because every part of it is a property of the configuration and none of any event.
#[derive(Debug)]
pub struct Conversion {
    /// The patterns the source text is read under, in the order the processor lists them,
    /// translated; at most [`MAX_MATCH_PATTERNS`] of them.
    patterns: Vec<JvmPattern>,
    /// "The time zone of the source field."
    source: Tz,
    /// "The time zone of the target field."
    target: Tz,
    /// "The datetime format to use for the converted data in the target field."
    format: JvmPattern,
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
        let patterns: Vec<JvmPattern> = match_patterns
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
fn instant_of(text: &str, pattern: &JvmPattern, zone: Tz) -> Option<DateTime<Utc>> {
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
fn formatted(instant: DateTime<Utc>, zone: Tz, pattern: &JvmPattern) -> Option<String> {
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

#[cfg(test)]
mod tests {
    // The ceiling on the bytes of one pattern is the shared translation's own
    // ([`::service_common::java_date_pattern`]), which is what does the walking.
    use ::service_common::java_date_pattern::MAX_PATTERN_BYTES;

    use super::{Conversion, DEFAULT_TARGET_FORMAT, MAX_MATCH_PATTERNS};

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
