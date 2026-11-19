// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the SCHEDULE EXPRESSION a caller hands a service that fires work on a
//! schedule, and the ONE wording of a refusal of one that cannot be read.
//!
//! Several services of this emulator take a schedule as a caller-supplied string in the same
//! grammar — `rate(value unit)` and `cron(fields)`, beside the one-shot
//! `at(yyyy-MM-dd'T'HH:mm:ss)` — and the recorded refusals of them read the same two parts:
//! the service's own statement that the value is not a schedule, and the PARSER's statement
//! of what stopped it, which names the offset into the expression body it stopped at and the
//! token it stopped on ([`invalid_schedule_expression`], [`cron_parse_failure`]). So the
//! grammar and those statements live here once rather than per service crate, and each
//! service decides only which of its own members carries the expression and which error class
//! it answers with.
//!
//! The CRON half is the six-field format those services document — "[Minutes] [Hours]
//! [Day_of_Month] [Month] [Day_of_Week] [Year]" — read through the maintained `cron` crate
//! rather than by a grammar written here: a caller-controlled expression is handed to a
//! parser this workspace already parses cron with. What this module adds around it is only
//! WHERE a refused body stopped the parse, which that crate's error does not carry: each
//! field is parsed in its own place, so the field that cannot be read is the one reported.
//!
//! Not every service's cron parser reports that the same way, so the two readings recorded
//! for them live here side by side under the one envelope: the six-field reading names an
//! offset into the body and quotes the token it stopped on ([`parse_cron`]), while the
//! SECONDS-FIRST reading — the parser a service hands the documented six fields to with the
//! seconds supplied — names the rule the field broke and the position inside that field the
//! rule is judged at ([`seconds_first_cron_failure`]).
//!
//! The work is linear in the LENGTH of the expression and takes at most one field parse per
//! field the format declares, plus one for the body as a whole: the expression itself is a
//! modelled member whose own `@length` bound the validating walk enforces before a handler
//! reads it, so nothing here grows with anything but that bounded string.

/// How many whitespace-separated fields a `cron(…)` schedule carries: "The cron format
/// consists of six fields separated by white spaces: [Minutes] [Hours] [Day_of_Month]
/// [Month] [Day_of_Week] [Year]."
pub const CRON_FIELDS: usize = 6;

/// The name of each cron field, in the order the format declares them — what a refusal names
/// when the expression does not carry that many.
pub const CRON_FIELD_NAMES: [&str; CRON_FIELDS] = [
    "Minutes",
    "Hours",
    "Day_of_Month",
    "Month",
    "Day_of_Week",
    "Year",
];

/// The six cron fields as ONE phrase, the way the format states them.
pub const CRON_FIELD_LAYOUT: &str =
    "[Minutes] [Hours] [Day_of_Month] [Month] [Day_of_Week] [Year]";

/// The units a `rate(value unit)` expression accepts, and what each is worth in seconds:
/// "value is a positive integer and unit is minute | minutes | hour | hours | day | days."
///
/// The one place the unit set lives — [`parse`] reads membership and the multiplier from it,
/// and a service's own suites pin the set through it.
pub const RATE_UNITS: &[(&str, u64)] = &[
    ("minute", 60),
    ("minutes", 60),
    ("hour", 3_600),
    ("hours", 3_600),
    ("day", 86_400),
    ("days", 86_400),
];

/// The detail an expression that is not one of the documented forms is refused with: the
/// grammar a schedule expression has to have, stated back to the caller.
///
/// A caller whose value is not a schedule at all is told the syntax, not which member failed
/// which constraint. A detail is a sentence of its own, so it carries its own full stop
/// ([`invalid_schedule_expression`]).
pub const SCHEDULE_SYNTAX_DETAIL: &str = "Schedule expressions must have the following syntax: \
     rate(<number>\\s?(minutes?|hours?|days?)), cron(<cron_expression>) or \
     at(yyyy-MM-dd'T'HH:mm:ss).";

/// The parser's statement about six fields that each parse where they stand but do not
/// combine into one expression, which belongs to no single token of the body.
const UNSUPPORTED_CRON_COMBINATION: &str = "Unsupported combination of CRON fields.";

/// A schedule expression this module read.
///
/// One variant per documented form. A `rate` expression is reported as the pair it was
/// written as rather than as a span: how long an interval a service admits, and what it does
/// to an instant, is the calling service's own rule.
#[derive(Debug, Clone)]
pub enum ScheduleExpression {
    /// One occurrence, at this LOCAL date and time — the time zone the expression is read in
    /// belongs to the resource, so the wall-clock reading is what this carries.
    At(::chrono::NaiveDateTime),
    /// An occurrence every `value` units, each unit worth `unit_seconds`
    /// ([`RATE_UNITS`]).
    Rate {
        /// The positive integer the expression named.
        value: u64,
        /// What one of its units is worth in seconds.
        unit_seconds: u64,
    },
    /// An occurrence at every instant the cron expression names. Boxed because a parsed cron
    /// expression is an order of magnitude larger than the other two forms.
    Cron(Box<::cron::Schedule>),
}

/// Wrap one parse detail as the message a schedule expression is refused with: the statement
/// that the value is not a schedule, with the detail of what stopped the parse.
///
/// The detail is a statement in its own right and carries its own punctuation, so the
/// envelope adds none: a cron body the parser refused is stated as the parser states it
/// ([`cron_parse_failure`]), down to the quoted token it ends on.
#[must_use]
pub fn invalid_schedule_expression(detail: &str) -> String {
    format!("Invalid schedule expression. Details: {detail}")
}

/// Read `expression`, or the message a value that is not a schedule is refused with.
///
/// The error is the whole statement ([`invalid_schedule_expression`]); a service wraps it in
/// the refusal class and the parameter sentence its own API words.
pub fn parse(expression: &str) -> Result<ScheduleExpression, String> {
    let body = |keyword: &str| {
        expression
            .strip_prefix(keyword)
            .and_then(|rest| rest.strip_prefix('('))
            .and_then(|rest| rest.strip_suffix(')'))
    };

    if let Some(instant) = body("at") {
        let parsed = ::chrono::NaiveDateTime::parse_from_str(instant.trim(), "%Y-%m-%dT%H:%M:%S")
            .map_err(|_| invalid_schedule_expression(SCHEDULE_SYNTAX_DETAIL))?;
        return Ok(ScheduleExpression::At(parsed));
    }
    if let Some(rate) = body("rate") {
        let mut parts = rate.split_whitespace();
        let (Some(value), Some(unit), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid_schedule_expression(SCHEDULE_SYNTAX_DETAIL));
        };
        let value: u64 = value
            .parse()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid_schedule_expression(SCHEDULE_SYNTAX_DETAIL))?;
        let unit_seconds = RATE_UNITS
            .iter()
            .find(|(name, _)| *name == unit)
            .map(|(_, seconds)| *seconds)
            .ok_or_else(|| invalid_schedule_expression(SCHEDULE_SYNTAX_DETAIL))?;
        return Ok(ScheduleExpression::Rate {
            value,
            unit_seconds,
        });
    }
    if let Some(fields) = body("cron") {
        return Ok(ScheduleExpression::Cron(Box::new(
            parse_cron(fields).map_err(|detail| invalid_schedule_expression(&detail))?,
        )));
    }
    Err(invalid_schedule_expression(SCHEDULE_SYNTAX_DETAIL))
}

/// Parse the six-field cron expression these services document into a [`cron::Schedule`], or
/// the parser's own statement of what stopped it ([`cron_parse_failure`]).
///
/// "The cron format consists of six fields separated by white spaces: [Minutes] [Hours]
/// [Day_of_Month] [Month] [Day_of_Week] [Year]" — a minute-resolution expression, which the
/// `cron` crate reads in its seven-field form (seconds first), so a literal `0` seconds field
/// is prepended. A `?` ("no specific value") in the day-of-month or day-of-week field is the
/// same statement as `*` in the grammar the crate parses; `?` in any OTHER field states
/// nothing, so it is a token that stops the parse.
///
/// Every POSITION a refusal names is an offset into the `cron(…)` BODY. Which token stopped
/// the parse is decided field by field: each field is parsed in its own place (the other five
/// held at `*`, which every field accepts), so the field that cannot be read is the one
/// reported rather than the whole expression.
pub fn parse_cron(body: &str) -> Result<::cron::Schedule, String> {
    let fields = whitespace_fields(body);
    for (index, (at, field)) in fields.iter().enumerate().take(CRON_FIELDS) {
        if !cron_field_parses(index, field) {
            return Err(cron_parse_failure(
                body_offset(*at),
                &illegal_characters(field),
            ));
        }
    }
    if let Some((at, field)) = fields.get(CRON_FIELDS) {
        return Err(cron_parse_failure(
            body_offset(*at),
            &format!(
                "Unexpected token '{}' after the {} field, the CRON format consists of \
                 {CRON_FIELDS} fields ({CRON_FIELD_LAYOUT}).",
                field.to_uppercase(),
                CRON_FIELD_NAMES[CRON_FIELDS - 1]
            ),
        ));
    }
    if let Some(missing) = CRON_FIELD_NAMES.get(fields.len()) {
        return Err(cron_parse_failure(
            body_offset(body.chars().count()),
            &format!(
                "Unexpected end of expression, the {missing} field is missing, the CRON format \
                 consists of {CRON_FIELDS} fields ({CRON_FIELD_LAYOUT}).",
            ),
        ));
    }
    let normalised: Vec<String> = ::std::iter::once("0".to_string())
        .chain(
            fields
                .iter()
                .enumerate()
                .map(|(index, (_, field))| normalised_cron_field(index, field).to_string()),
        )
        .collect();
    // Every field parses in its own place, so what is left is a combination of fields the
    // grammar does not allow, which belongs to no single token: the parse stops where the
    // expression begins.
    normalised
        .join(" ")
        .parse::<::cron::Schedule>()
        .map_err(|_| cron_parse_failure(0, UNSUPPORTED_CRON_COMBINATION))
}

/// How the parser states where a refused `cron(…)` body stopped it: the position it stopped
/// at, and its own statement of what stopped it.
///
/// One envelope for every cron refusal, so a caller that surfaces or parses the detail reads
/// the same two parts whichever field was at fault. What the POSITION counts is the reporting
/// parser's own: the six-field reading below names an offset into the BODY ([`parse_cron`]),
/// while the seconds-first reading names an offset inside the FIELD it stopped on and `-1`
/// for a rule about that field's value as a whole ([`seconds_first_cron_failure`]) — which is
/// why it is signed.
#[must_use]
pub fn cron_parse_failure(position: i64, detail: &str) -> String {
    format!("Invalid CRON expression: error at position {position}, details: {detail}")
}

/// The detail a field stating "no specific value" where that states nothing is refused with:
/// `?` stands for the day of the month or the day of the week and for no other field.
pub const NO_SPECIFIC_VALUE_DETAIL: &str =
    "'?' can only be specified for Day-of-Month or Day-of-Week.";

/// The position such a failure is reported at: `?` is one character long, so the parse
/// stopped one character into the field it stood in.
pub const NO_SPECIFIC_VALUE_POSITION: i64 = 1;

/// The detail a body that runs out of fields before the seconds-first parse has read the ones
/// it needs is refused with.
pub const UNEXPECTED_END_DETAIL: &str = "Unexpected end of expression.";

/// The position a failure about a field's VALUE RANGE is reported at: the range is judged once
/// the whole field has been read, so the parse names no character of it.
pub const WHOLE_FIELD_POSITION: i64 = -1;

/// The position a failure about the CHARACTERS of a field is reported at: the parse stopped
/// where that field began.
pub const FIELD_START_POSITION: i64 = 0;

/// The fields the SECONDS-FIRST cron parser reads, in ITS own order — seconds, then the six
/// [`CRON_FIELD_NAMES`] documents — each with the sentence stating the values it admits and
/// the range those values fall in.
///
/// A service whose cron parser reads this form is handed the six documented fields with the
/// seconds supplied, so a body it will not read that way is read ONCE MORE as the parser's own
/// fields: that is the layout the position and the detail of such a refusal are about, and it
/// is why a `?` standing in the documented day-of-month field is reported against the hour
/// the parser read it as ([`seconds_first_cron_failure`]). The range is taken only against a
/// field stating a plain NUMBER, so one naming a month or a weekday is judged by the grammar
/// instead ([`cron_field_parses`]).
const SECONDS_FIRST_FIELDS: &[(&str, (i64, i64))] = &[
    ("Minute and Second values must be between 0 and 59.", (0, 59)),
    ("Minute and Second values must be between 0 and 59.", (0, 59)),
    ("Hour values must be between 0 and 23.", (0, 23)),
    ("Day of month values must be between 1 and 31.", (1, 31)),
    ("Month values must be between 1 and 12.", (1, 12)),
    ("Day-of-Week values must be between 1 and 7.", (1, 7)),
    ("Year values must be between 1970 and 2099.", (1970, 2099)),
];

/// The indexes of the two fields `?` is a statement in among [`SECONDS_FIRST_FIELDS`]: that
/// reading's day-of-month and day-of-week.
const SECONDS_FIRST_NO_SPECIFIC_VALUE_FIELDS: [usize; 2] = [3, 5];

/// What a SECONDS-FIRST cron parser answers for a `cron(…)` body it will not read: the whole
/// refusal message, position and detail ([`cron_parse_failure`]).
///
/// The body is read field by field in that parser's own layout ([`SECONDS_FIRST_FIELDS`]) and
/// the FIRST field those rules refuse is what the failure is about — the parse stops there, so
/// nothing later in the expression is reported. The rules, in the order they are taken: a `?`
/// standing in a field it states nothing in ([`NO_SPECIFIC_VALUE_DETAIL`]), a field the
/// grammar cannot read where it stands whose value is a plain number outside the range that
/// field admits (the range sentence, at [`WHOLE_FIELD_POSITION`]), and any other field the
/// grammar cannot read there ([`illegal_characters`], at [`FIELD_START_POSITION`]). A body
/// carrying fewer fields than the documented format declares stopped at the body's end
/// instead, and one every field of which reads carries more fields than the format declares,
/// which is where the parse ran past.
///
/// Call it for a body already known unreadable: a body this reading finds nothing wrong with
/// is reported as having run past its last field.
#[must_use]
pub fn seconds_first_cron_failure(body: &str) -> String {
    let body = body.trim();
    let fields = whitespace_fields(body);
    let ends_at = body_offset(body.chars().count());
    if fields.len() < CRON_FIELDS {
        return cron_parse_failure(ends_at, UNEXPECTED_END_DETAIL);
    }
    for (index, (_, field)) in fields.iter().enumerate().take(SECONDS_FIRST_FIELDS.len()) {
        if let Some((position, detail)) = seconds_first_field_failure(index, field) {
            return cron_parse_failure(position, &detail);
        }
    }
    cron_parse_failure(ends_at, UNEXPECTED_END_DETAIL)
}

/// Why the seconds-first parser refuses `field` standing at `index` among the fields it reads,
/// or `None` when it reads it there ([`seconds_first_cron_failure`] states the rules).
fn seconds_first_field_failure(index: usize, field: &str) -> Option<(i64, String)> {
    let (range_detail, (low, high)) = SECONDS_FIRST_FIELDS.get(index).copied()?;
    if field == "?" && !SECONDS_FIRST_NO_SPECIFIC_VALUE_FIELDS.contains(&index) {
        return Some((
            NO_SPECIFIC_VALUE_POSITION,
            NO_SPECIFIC_VALUE_DETAIL.to_string(),
        ));
    }
    // The seconds field and the minutes field admit the same values and the five after them
    // are the documented fields one place along, so the reading of a field in its place is the
    // six-field one shifted by that place.
    if cron_field_parses(index.saturating_sub(1), field) {
        return None;
    }
    if field
        .parse::<i64>()
        .is_ok_and(|value| value < low || value > high)
    {
        return Some((WHOLE_FIELD_POSITION, range_detail.to_string()));
    }
    Some((FIELD_START_POSITION, illegal_characters(field)))
}

/// A character count as the position a refusal reports it.
///
/// A cron body is a modelled member whose own `@length` bound the validating walk enforces
/// before a handler reads it, so no body has more characters than this can count; a count
/// past it names no character of the body at all ([`WHOLE_FIELD_POSITION`]).
fn body_offset(at: usize) -> i64 {
    i64::try_from(at).unwrap_or(WHOLE_FIELD_POSITION)
}

/// The parser's statement about a token it cannot read where that token stands.
///
/// The expression is read in UPPER CASE — a named month or day of week is one of a fixed set
/// of names, matched whatever case it was written in — so the token is quoted as the parser
/// saw it rather than as the caller typed it.
#[must_use]
pub fn illegal_characters(token: &str) -> String {
    format!(
        "Illegal characters for this position: '{}'",
        token.to_uppercase()
    )
}

/// The whitespace-separated fields of `text`, each with the CHARACTER position it starts at —
/// the position a parse refusal reports, which `split_whitespace` alone does not carry.
fn whitespace_fields(text: &str) -> Vec<(usize, &str)> {
    let mut fields = Vec::new();
    let mut start: Option<(usize, usize)> = None;
    for (position, (at, character)) in text.char_indices().enumerate() {
        match (character.is_whitespace(), start) {
            (false, None) => start = Some((position, at)),
            (true, Some((position, from))) => {
                fields.push((position, &text[from..at]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some((position, from)) = start {
        fields.push((position, &text[from..]));
    }
    fields
}

/// What the `cron` crate's grammar reads a field as: `?` ("no specific value") is `*` in the
/// day-of-month and day-of-week fields, and means nothing anywhere else.
fn normalised_cron_field(index: usize, field: &str) -> &str {
    /// The indexes of the day-of-month and day-of-week fields in [`CRON_FIELD_NAMES`].
    const NO_SPECIFIC_VALUE_FIELDS: [usize; 2] = [2, 4];

    if field == "?" && NO_SPECIFIC_VALUE_FIELDS.contains(&index) {
        "*"
    } else {
        field
    }
}

/// Whether `field` is a legal cron field in the place `index` names among the six
/// [`CRON_FIELD_NAMES`] documents, judged by parsing it THERE with every other field held at
/// `*` — so the answer is about that field and not about the rest of the expression.
///
/// Public, so a service whose own cron dialect adds forms this grammar does not read (a
/// last-day or nth-weekday wildcard, say) judges the ORDINARY fields of such an expression
/// through this one reading rather than through a copy of it.
#[must_use]
pub fn cron_field_parses(index: usize, field: &str) -> bool {
    // The crate's seven-field form: seconds, then the six the format documents.
    let mut expression = ["0", "*", "*", "*", "*", "*", "*"];
    expression[index + 1] = normalised_cron_field(index, field);
    expression
        .join(" ")
        .parse::<::cron::Schedule>()
        .is_ok()
}

#[cfg(test)]
mod tests {
    //! The grammar and the two parts of a refusal, over the forms and the fields.

    use super::*;

    /// RULE: every documented form reads, whichever field of a cron expression carries a
    /// value and whichever unit a rate names.
    #[test]
    fn every_documented_form_reads() {
        // (case, the expression)
        let rows = [
            ("a cron with ? for the day of week", "cron(0 10 * * ? *)"),
            ("a cron with a step", "cron(0/15 * * * ? *)"),
            ("a cron with a list and a named month", "cron(0 8,20 1 JAN ? *)"),
            ("a cron with a day-of-week range", "cron(30 9 ? * MON-FRI *)"),
            ("a cron naming a year", "cron(15 2 1 * ? 2030)"),
            ("a rate of one unit", "rate(1 day)"),
            ("a rate of several units", "rate(15 minutes)"),
            ("a one-shot instant", "at(2030-01-02T03:04:05)"),
        ];

        for (case, expression) in rows {
            let read = parse(expression);
            assert!(
                read.is_ok(),
                "case {case:?}: expected {expression:?} to read, got {read:?}"
            );
        }
    }

    /// RULE: a cron body the parser cannot read is refused where it stopped — the offset into
    /// the body, counted from zero — and quotes the token it stopped on in upper case.
    #[test]
    fn a_refused_cron_body_states_where_in_the_body_it_stopped() {
        // (case, the expression, the offset into the body, the token it stopped on)
        let rows = [
            ("a minutes field that is not a minute", "cron(not valid)", 0, "NOT"),
            ("a named minutes field", "cron(sixty 10 * * ? *)", 0, "SIXTY"),
            ("an hours field out of range", "cron(0 99 * * ? *)", 2, "99"),
            ("a day of month that is not a day", "cron(0 10 someday * ? *)", 5, "SOMEDAY"),
            ("a month that is not a month", "cron(0 10 * SMARCH ? *)", 7, "SMARCH"),
            ("a day of week in lower case", "cron(0 10 ? * funday *)", 9, "FUNDAY"),
            ("a year that is not a year", "cron(0 10 * * ? someday)", 11, "SOMEDAY"),
        ];

        for (case, expression, position, token) in rows {
            let expected = invalid_schedule_expression(&cron_parse_failure(
                position,
                &illegal_characters(token),
            ));
            let refused = parse(expression).err();
            assert_eq!(
                refused.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {refused:?}"
            );
        }
    }

    /// RULE: a body carrying too few fields stops at the end of the body, where the field it
    /// is missing would have been; one carrying too many stops at the extra token. A value
    /// that is not one of the documented forms is refused with the grammar instead.
    #[test]
    fn a_body_of_the_wrong_field_count_and_a_value_that_is_no_schedule() {
        // (case, the expression, the opening of the detail the refusal carries)
        let rows = [
            (
                "five fields, so no year",
                "cron(0 10 * * ?)",
                "Invalid CRON expression: error at position 10, details: Unexpected end of \
                 expression, the Year field is missing,",
            ),
            (
                "no fields at all",
                "cron()",
                "Invalid CRON expression: error at position 0, details: Unexpected end of \
                 expression, the Minutes field is missing,",
            ),
            (
                "seven fields",
                "cron(0 0 10 * * ? *)",
                "Invalid CRON expression: error at position 11, details: Illegal characters \
                 for this position: '?'",
            ),
            (
                "not a schedule at all",
                "nightly",
                SCHEDULE_SYNTAX_DETAIL,
            ),
            (
                "a rate of no unit",
                "rate(5)",
                SCHEDULE_SYNTAX_DETAIL,
            ),
            (
                "a rate of zero",
                "rate(0 hours)",
                SCHEDULE_SYNTAX_DETAIL,
            ),
        ];

        for (case, expression, detail) in rows {
            let refused = parse(expression).err();
            let opening = invalid_schedule_expression(detail);
            assert!(
                refused
                    .as_deref()
                    .is_some_and(|message| message.starts_with(&opening)),
                "case {case:?}: expected a refusal opening with {opening:?}, got {refused:?}"
            );
        }
    }

    /// RULE: the SECONDS-FIRST reading of a body it will not read names the first field its
    /// own rules refuse — the rule that field broke, at the position that rule is judged at.
    /// The rows differ along which field fails, which kind of rule it breaks and how many
    /// fields the body carries.
    #[test]
    fn a_seconds_first_reading_states_which_field_stopped_it_and_why() {
        // (case, the cron body, the position reported, the detail reported)
        let rows: [(&str, &str, i64, &str); 9] = [
            (
                "no specific value where it reads an hour",
                "0 25 ? * MON *",
                NO_SPECIFIC_VALUE_POSITION,
                NO_SPECIFIC_VALUE_DETAIL,
            ),
            (
                "no specific value where it reads a second",
                "? 8 1 * MON *",
                NO_SPECIFIC_VALUE_POSITION,
                NO_SPECIFIC_VALUE_DETAIL,
            ),
            (
                "a second past the values a second admits",
                "99 8 1 * MON *",
                WHOLE_FIELD_POSITION,
                "Minute and Second values must be between 0 and 59.",
            ),
            (
                "an hour past the values an hour admits",
                "0 0 99 * MON *",
                WHOLE_FIELD_POSITION,
                "Hour values must be between 0 and 23.",
            ),
            (
                "a day of the month past the values it admits",
                "0 0 0 99 MON *",
                WHOLE_FIELD_POSITION,
                "Day of month values must be between 1 and 31.",
            ),
            (
                "a month past the values a month admits",
                "0 0 0 1 99 *",
                WHOLE_FIELD_POSITION,
                "Month values must be between 1 and 12.",
            ),
            (
                "characters the grammar does not read, quoted as the parser read them",
                "zzz 8 1 * MON *",
                FIELD_START_POSITION,
                "Illegal characters for this position: 'ZZZ'",
            ),
            (
                "fewer fields than the format declares",
                "0 8 1 * ?",
                9,
                UNEXPECTED_END_DETAIL,
            ),
            ("one field", "0", 1, UNEXPECTED_END_DETAIL),
        ];

        for (case, body, position, detail) in rows {
            let expected = cron_parse_failure(position, detail);
            let refused = seconds_first_cron_failure(body);
            assert_eq!(
                refused, expected,
                "case {case:?}: expected {expected:?}, got {refused:?}"
            );
        }
    }
}
