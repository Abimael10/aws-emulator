// SPDX-License-Identifier: CC0-1.0

//! What one instance's GUEST reads as the TIME, and the program it reads it with.
//!
//! # Why a guest needs this
//!
//! A script run in a machine stamps what it does: it reads the clock before an operation and after
//! it, so that whoever reads the output knows the window the work happened in — which is how a
//! client then asks CloudWatch for the datapoints of exactly that window
//! (`date +%s` around the work, and `GetMetricStatistics` over the two instants). A guest whose
//! clock answers nothing leaves a successful script with no window at all, and nothing a reader can
//! correlate its output with.
//!
//! # What is real here
//!
//! The reading is this process's own clock, which is the clock every other instant of this emulator
//! is read off, rendered on the civil calendar in UTC — the zone a machine of this emulator runs in,
//! as the published machine facts report one. The program is a program OF THE GUEST, performed by
//! the same bounded, in-process shell session the guest's own boot script runs in
//! ([`super::guest_console`]): it starts no process and reaches no path of the host.
//!
//! An invocation asking for a form this emulator does not render — a format the civil calendar
//! cannot be written in, or a date to be PARSED out of prose — answers as a command that could not
//! be run, rather than an instant nothing stands behind.

use ::std::fmt::Write as _;

use ::chrono::format::{Item, StrftimeItems};
use ::chrono::{DateTime, TimeZone, Utc};

use ::service_common::posix_shell_session::{
    EnvironmentPrograms, ProgramFiles, ProgramOutcome,
};

/// The program a guest reads the time with.
pub const CLOCK_PROGRAM: &str = "date";

/// The status a program of this guest exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The form `date` writes an instant in when the invocation asks for none: "the default is
/// `'+%a %b %e %H:%M:%S %Z %Y'`" (`date(1)`).
const DEFAULT_FORMAT: &str = "%a %b %e %H:%M:%S %Z %Y";

/// The form `-R`/`--rfc-email` asks for: "output date and time in RFC 5322 format"
/// (`date(1)`), which is that standard's own date-time.
const EMAIL_FORMAT: &str = "%a, %d %b %Y %H:%M:%S %z";

/// The forms `-I`/`--iso-8601` asks for, by the precision its value names: "output date/time in ISO
/// 8601 format … FMT='date' for date only (the default), 'hours', 'minutes', 'seconds', or 'ns'"
/// (`date(1)`).
const ISO_FORMATS: &[(&str, &str)] = &[
    ("date", "%Y-%m-%d"),
    ("hours", "%Y-%m-%dT%H%:z"),
    ("minutes", "%Y-%m-%dT%H:%M%:z"),
    ("seconds", "%Y-%m-%dT%H:%M:%S%:z"),
    ("ns", "%Y-%m-%dT%H:%M:%S,%f%:z"),
];

/// The prefix the `-d`/`--date` operand names an instant by its EPOCH SECONDS with: "@" followed by
/// "seconds since the Epoch" (`date(1)`, which documents `@` as one of the forms a date string
/// takes). Every other date string is prose this emulator does not parse.
const EPOCH_PREFIX: &str = "@";

/// The flag a specifier carries when it is one a calendar can be READ in but not written in — a
/// form this guest renders nothing of ([`Reading::written`]).
const PARSE_ONLY_FLAG: &str = "%#";

/// The programs a guest reads the time with, for a session that runs a program of that guest.
#[must_use]
pub fn clock_programs() -> Box<dyn EnvironmentPrograms> {
    Box::new(ClockPrograms)
}

/// The clock programs of one machine's guest.
///
/// It holds nothing: the reading is the clock itself, taken when the program runs.
#[derive(Debug)]
struct ClockPrograms;

impl EnvironmentPrograms for ClockPrograms {
    /// `date` writes the instant it was run at, in the form the invocation asks for.
    ///
    /// A program named by its absolute path is the same program, as a shell resolving a name on
    /// `PATH` finds. An invocation carrying an option or a format this program does not render
    /// answers `None` — a form this guest performs nothing of.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let _ = files;
        if name.rsplit('/').next().unwrap_or(name) != CLOCK_PROGRAM {
            return None;
        }
        let reading = Reading::of(arguments)?;
        Some(ProgramOutcome {
            status: SUCCESS,
            output: vec![reading.written(Utc::now())?],
        })
    }
}

/// What one `date` invocation asks for: which instant, written in which form.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reading {
    /// The `strftime` form the instant is written in.
    format: String,
    /// The instant itself, for an invocation that named one by its epoch seconds — `None` for one
    /// that asks for NOW, which is what `date` with no date operand reads.
    at_secs: Option<i64>,
}

impl Reading {
    /// What `arguments` ask for, or `None` for an invocation this guest performs nothing of.
    fn of(arguments: &[String]) -> Option<Self> {
        let mut reading = Self {
            format: DEFAULT_FORMAT.to_string(),
            at_secs: None,
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            if let Some(format) = argument.strip_prefix('+') {
                reading.format = format.to_string();
                continue;
            }
            if let Some(name) = argument.strip_prefix("--") {
                let (name, inline) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (name, None),
                };
                match name {
                    // The zone a machine of this emulator runs in is UTC already, so asking for
                    // the universal time asks for the reading this clock takes.
                    "utc" | "universal" => {}
                    "rfc-email" | "rfc-2822" => reading.format = EMAIL_FORMAT.to_string(),
                    "iso-8601" => {
                        reading.format = iso_format(inline.as_deref().unwrap_or("date"))?;
                    }
                    "date" => {
                        let value = match inline {
                            Some(value) => value,
                            None => arguments.next()?.clone(),
                        };
                        reading.at_secs = Some(epoch_secs_of(&value)?);
                    }
                    _ => return None,
                }
                continue;
            }
            if let Some(letters) = argument.strip_prefix('-').filter(|rest| !rest.is_empty()) {
                let mut letters = letters.chars().peekable();
                while let Some(letter) = letters.next() {
                    match letter {
                        'u' => {}
                        'R' => reading.format = EMAIL_FORMAT.to_string(),
                        'I' => {
                            // `-I[FMT]`: the precision is glued to the letter, which is therefore
                            // the last of its own word.
                            let precision: String = letters.by_ref().collect();
                            reading.format = iso_format(if precision.is_empty() {
                                "date"
                            } else {
                                precision.trim_start_matches('=')
                            })?;
                        }
                        'd' => {
                            // `-d STRING`: the instant is the word after this one, so the letter is
                            // the last of its own word.
                            if letters.peek().is_some() {
                                return None;
                            }
                            reading.at_secs = Some(epoch_secs_of(arguments.next()?)?);
                        }
                        _ => return None,
                    }
                }
                continue;
            }
            // An operand that is neither an option nor a format is one this program takes none of.
            return None;
        }
        Some(reading)
    }

    /// The instant this reading is OF, given that `now` is the clock: the one the invocation named,
    /// or the clock itself.
    fn instant(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self.at_secs {
            Some(at_secs) => Utc.timestamp_opt(at_secs, 0).single(),
            None => Some(now),
        }
    }

    /// The line this reading writes, or `None` for a format the civil calendar cannot be written in
    /// — which is a form this guest performs nothing of.
    ///
    /// The format is caller-supplied text, so it is rendered through a writer and a renderer that
    /// REFUSES is answered as that form: neither a specifier the calendar cannot be read in nor one
    /// it can be read in but not written in produces anything but `None`.
    fn written(&self, now: DateTime<Utc>) -> Option<String> {
        if self.format.contains(PARSE_ONLY_FLAG) {
            return None;
        }
        if StrftimeItems::new(&self.format).any(|item| matches!(item, Item::Error)) {
            return None;
        }
        let instant = self.instant(now)?;
        let mut line = String::new();
        write!(
            &mut line,
            "{}",
            instant.format_with_items(StrftimeItems::new(&self.format))
        )
        .ok()?;
        Some(line)
    }
}

/// The ISO 8601 form the precision `named` asks for, or `None` for a precision `date` has no form
/// for ([`ISO_FORMATS`]).
fn iso_format(named: &str) -> Option<String> {
    ISO_FORMATS
        .iter()
        .find(|(precision, _)| *precision == named)
        .map(|(_, format)| (*format).to_string())
}

/// The epoch seconds the date string `named` states, for the one form of it that names an instant
/// outright ([`EPOCH_PREFIX`]) — and `None` for prose this emulator does not parse.
fn epoch_secs_of(named: &str) -> Option<i64> {
    named.trim().strip_prefix(EPOCH_PREFIX)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the reading is the instant the program ran, written in the form the invocation asked
    /// for — over a table of the forms a script writes.
    #[test]
    fn the_reading_is_the_instant_in_the_form_asked_for() {
        // An instant of the civil calendar: 2023-11-14T22:13:20Z.
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("an instant");
        // (case, words, what the line must be)
        let rows: [(&str, &[&str], &str); 7] = [
            ("the epoch seconds", &["+%s"], "1700000000"),
            (
                "an ISO instant",
                &["-u", "+%Y-%m-%dT%H:%M:%SZ"],
                "2023-11-14T22:13:20Z",
            ),
            ("the calendar date", &["+%Y-%m-%d"], "2023-11-14"),
            ("the time of day", &["+%H:%M:%S"], "22:13:20"),
            (
                "no format at all",
                &[],
                "Tue Nov 14 22:13:20 UTC 2023",
            ),
            (
                "the standard's date-time",
                &["-R"],
                "Tue, 14 Nov 2023 22:13:20 +0000",
            ),
            ("an ISO date", &["-I"], "2023-11-14"),
        ];
        for (case, words, expected) in rows {
            let arguments: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
            let reading = Reading::of(&arguments)
                .unwrap_or_else(|| panic!("case {case:?}: the guest performs it"));
            let written = reading
                .written(now)
                .unwrap_or_else(|| panic!("case {case:?}: the instant is written"));
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: an instant the invocation NAMES is the one written, and a form this guest renders
    /// nothing of is performed by nothing.
    #[test]
    fn a_named_instant_is_written_and_an_unknown_form_is_refused() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("an instant");
        let named: Vec<String> = ["-d", "@1500000000", "+%Y-%m-%d"]
            .iter()
            .map(|word| (*word).to_string())
            .collect();
        let reading = Reading::of(&named).expect("the guest performs it");
        assert_eq!(
            reading.written(now).as_deref(),
            Some("2017-07-14"),
            "expected the instant the invocation named, got {reading:?}"
        );
        // (case, words)
        let rows: [(&str, &[&str]); 3] = [
            ("an option it does not read", &["-Q"]),
            ("a date string it does not parse", &["-d", "yesterday"]),
            ("an operand that is no format", &["now"]),
        ];
        for (case, words) in rows {
            let arguments: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
            assert!(
                Reading::of(&arguments).is_none(),
                "case {case:?}: expected the guest to perform nothing of it"
            );
        }
        // ... and a format the calendar cannot be WRITTEN in is answered the same way, rather than
        // by a renderer that gives up part-way.
        // (case, the format)
        let formats = [
            ("a specifier the calendar is only read in", "+%#z"),
            ("a specifier that is no specifier", "+%"),
        ];
        for (case, format) in formats {
            let arguments = vec![format.to_string()];
            let rendered = Reading::of(&arguments).and_then(|reading| reading.written(now));
            assert!(
                rendered.is_none(),
                "case {case:?}: expected the guest to render nothing of it, got {rendered:?}"
            );
        }
    }
}
