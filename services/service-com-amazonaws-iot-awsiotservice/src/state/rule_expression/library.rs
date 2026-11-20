// SPDX-License-Identifier: CC0-1.0

//! The FUNCTIONS of the AWS IoT SQL library this engine computes, over arguments
//! [`super::evaluate`] has already read off the message.
//!
//! The AWS IoT SQL reference gives the library as a set of SCALAR functions — "the following
//! functions can be used in a SELECT or WHERE clause", and they are the same functions a
//! substitution template in an action parameter is written with — so each one here is a value
//! computed from this walk's own inputs: the arguments' values, the topic the message was
//! published to, the clock, and this account's own id. Nothing else is reachable from one: no
//! socket, no subprocess, no file, no process environment.
//!
//! Three readings every function shares, so no two of them disagree:
//!
//! * an argument this walk could not decide makes the call undecided, and an argument the
//!   message has no value for makes it SQL's unknown — "a function with an Undefined argument
//!   returns Undefined";
//! * an argument of the wrong kind for the function (text where a number is required) is a
//!   call this engine does not compute rather than one it guesses a conversion for, except for
//!   the one conversion the reference documents between a number and the string that spells it
//!   ([`super::as_number`]);
//! * a whole numeric result is reported as an integer, because that is the value the expression
//!   has: a JSON document carries no distinction between `3` and `3.0`.
//!
//! A function the reference lists and this module does not implement is
//! [`Evaluated::Undecided`], which the rules engine answers by performing NO action rather than
//! by approximating one.

use ::serde_json::Value;

use super::{Evaluated, Message};

/// "topic(Decimal)": "the topic to which the message was sent", whole with no argument and one
/// 1-based SEGMENT of it with one.
const TOPIC: &str = "topic";

/// "timestamp()": "the current timestamp in milliseconds".
const TIMESTAMP: &str = "timestamp";

/// "newuuid()": "a random 16-byte UUID".
const NEWUUID: &str = "newuuid";

/// "accountid()": "the ID of the account that owns this rule".
const ACCOUNTID: &str = "accountid";

/// "parse_time(String, Long, [String])": "uses a pattern string to format a timestamp".
const PARSE_TIME: &str = "parse_time";

/// The separator between two levels of an MQTT topic name, which is what [`TOPIC`] numbers the
/// segments of.
const TOPIC_LEVEL_SEPARATOR: char = '/';

/// The one time zone this engine formats a time in: the zone
/// [`PARSE_TIME`] uses when the call names none ("the default is UTC"). A call naming another
/// zone is one this engine does not compute, rather than one it formats in the wrong zone.
const UTC_ZONES: [&str; 5] = ["UTC", "Z", "GMT", "+00:00", "Etc/UTC"];

/// How long a text value one of these functions COMPUTES may be.
///
/// A function can make text longer than its arguments — `concat` adds them up, `replace` can put
/// a long replacement in place of each occurrence of a short one, and a date pattern can ask for
/// a field to be padded — so the result is the growth site a message's own members drive. Far
/// above any address, key or name an action parameter carries (an object key is at most 1024
/// characters), and past it the call is one this engine does not compute rather than a value it
/// builds.
const MAX_COMPUTED_TEXT_CHARACTERS: usize = 64 * 1024;

/// The value the call of `name` over `arguments` has for `message`, or [`Evaluated::Undecided`]
/// for a call this engine does not compute.
///
/// `arguments` are already evaluated, in the order the call wrote them.
pub(super) fn call(name: &str, arguments: &[Evaluated], message: Message<'_>) -> Evaluated {
    // The three calls whose value is not a function of an argument's VALUE: the topic the
    // message was published to, the clock and this account's id.
    if name.eq_ignore_ascii_case(TOPIC) {
        return topic(arguments, message);
    }
    if name.eq_ignore_ascii_case(TIMESTAMP) {
        return match arguments {
            [] => number(epoch_milliseconds() as f64, true),
            _ => Evaluated::Undecided,
        };
    }
    if name.eq_ignore_ascii_case(NEWUUID) {
        return match arguments {
            [] => Evaluated::Known(Value::String(super::super::fresh_uuid())),
            _ => Evaluated::Undecided,
        };
    }
    if name.eq_ignore_ascii_case(ACCOUNTID) {
        return match arguments {
            [] => Evaluated::Known(Value::String(
                ::engine::emulator_account_id().to_string(),
            )),
            _ => Evaluated::Undecided,
        };
    }

    // Everything below is a function OF its arguments, so an argument that is not a value
    // decides the call before the function is reached.
    if let Some(unknown) = propagated(arguments) {
        return unknown;
    }
    let values: Vec<&Value> = arguments
        .iter()
        .filter_map(|argument| match argument {
            Evaluated::Known(value) => Some(value),
            _ => None,
        })
        .collect();
    if values.len() != arguments.len() {
        return Evaluated::Undecided;
    }
    if name.eq_ignore_ascii_case(PARSE_TIME) {
        return parse_time(&values);
    }
    match name.to_ascii_lowercase().as_str() {
        // Text.
        "concat" => concat(&values),
        "length" => match values.as_slice() {
            [value] => text_of(value).map_or(Evaluated::Undecided, |text| {
                number(text.chars().count() as f64, true)
            }),
            _ => Evaluated::Undecided,
        },
        "upper" => mapped_text(&values, |text| text.to_uppercase()),
        "lower" => mapped_text(&values, |text| text.to_lowercase()),
        "trim" => mapped_text(&values, |text| text.trim().to_string()),
        "substring" => substring(&values),
        "replace" => replace(&values),
        "startswith" => pair_of_text(&values, |text, part| text.starts_with(part)),
        "endswith" => pair_of_text(&values, |text, part| text.ends_with(part)),
        "indexof" => index_of(&values),
        // Numbers.
        "floor" => mapped_number(&values, f64::floor),
        "ceil" => mapped_number(&values, f64::ceil),
        "round" => mapped_number(&values, f64::round),
        "abs" => mapped_number(&values, f64::abs),
        "sqrt" => mapped_number(&values, f64::sqrt),
        "sign" => mapped_number(&values, f64::signum),
        "mod" => pair_of_numbers(&values, |left, right| left % right),
        "power" => pair_of_numbers(&values, f64::powf),
        // Collections.
        "get" => get(&values),
        _ => Evaluated::Undecided,
    }
}

/// What a call answers for an argument that is not a VALUE: undecided where one is undecided,
/// and SQL's unknown where one is the message's missing attribute — "a function with an
/// Undefined argument returns Undefined". `None` when every argument is a value.
fn propagated(arguments: &[Evaluated]) -> Option<Evaluated> {
    if arguments
        .iter()
        .any(|argument| matches!(argument, Evaluated::Undecided))
    {
        return Some(Evaluated::Undecided);
    }
    arguments
        .iter()
        .any(|argument| matches!(argument, Evaluated::Absent))
        .then_some(Evaluated::Absent)
}

/// [`TOPIC`]: the whole topic with no argument, and that 1-based segment of it with one. A
/// number outside the topic's segments names no segment, which is SQL's unknown rather than an
/// invented value.
fn topic(arguments: &[Evaluated], message: Message<'_>) -> Evaluated {
    match arguments {
        [] => Evaluated::Known(Value::String(message.topic.to_string())),
        [Evaluated::Known(value)] => {
            let Some(number) = value.as_u64().filter(|number| *number >= 1) else {
                return Evaluated::Undecided;
            };
            let index = usize::try_from(number - 1).unwrap_or(usize::MAX);
            message
                .topic
                .split(TOPIC_LEVEL_SEPARATOR)
                .nth(index)
                .map_or(Evaluated::Absent, |segment| {
                    Evaluated::Known(Value::String(segment.to_string()))
                })
        }
        _ => Evaluated::Undecided,
    }
}

/// "concat(...)": the text of every argument, in order — which is how a template builds one
/// value out of several.
fn concat(values: &[&Value]) -> Evaluated {
    let mut joined = String::new();
    for value in values {
        match text_of(value) {
            Some(text) => joined.push_str(&text),
            None => return Evaluated::Undecided,
        }
        if joined.chars().count() > MAX_COMPUTED_TEXT_CHARACTERS {
            return Evaluated::Undecided;
        }
    }
    computed_text(joined)
}

/// One text argument mapped through `mapping`.
fn mapped_text(values: &[&Value], mapping: impl FnOnce(&str) -> String) -> Evaluated {
    match values {
        [value] => text_of(value).map_or(Evaluated::Undecided, |text| {
            computed_text(mapping(&text))
        }),
        _ => Evaluated::Undecided,
    }
}

/// One numeric argument mapped through `mapping`.
fn mapped_number(values: &[&Value], mapping: impl FnOnce(f64) -> f64) -> Evaluated {
    match values {
        [value] => super::as_number(value).map_or(Evaluated::Undecided, |number| {
            let whole = super::as_integer(value).is_some();
            self::number(mapping(number), whole)
        }),
        _ => Evaluated::Undecided,
    }
}

/// Two numeric arguments combined through `combining`.
fn pair_of_numbers(values: &[&Value], combining: impl FnOnce(f64, f64) -> f64) -> Evaluated {
    match values {
        [left, right] => match (super::as_number(left), super::as_number(right)) {
            (Some(left_number), Some(right_number)) => {
                let whole =
                    super::as_integer(left).is_some() && super::as_integer(right).is_some();
                number(combining(left_number, right_number), whole)
            }
            _ => Evaluated::Undecided,
        },
        _ => Evaluated::Undecided,
    }
}

/// Two text arguments decided through `deciding`.
fn pair_of_text(values: &[&Value], deciding: impl FnOnce(&str, &str) -> bool) -> Evaluated {
    match values {
        [text, part] => match (text_of(text), text_of(part)) {
            (Some(text), Some(part)) => Evaluated::Known(Value::Bool(deciding(&text, &part))),
            _ => Evaluated::Undecided,
        },
        _ => Evaluated::Undecided,
    }
}

/// "substring(String, Int, [Int])": the characters of the first argument from the second
/// position up to (and not including) the third, or to its end where the call names none.
///
/// Both positions are CHARACTER positions and both are clamped to the text, so a position
/// outside it shortens the result rather than making the call undecided — and a start past the
/// end is the empty text.
fn substring(values: &[&Value]) -> Evaluated {
    let (text, from, to) = match values {
        [text, from] => (text_of(text), super::as_number(from), None),
        [text, from, to] => (
            text_of(text),
            super::as_number(from),
            Some(super::as_number(to)),
        ),
        _ => return Evaluated::Undecided,
    };
    let (Some(text), Some(from)) = (text, from) else {
        return Evaluated::Undecided;
    };
    let characters: Vec<char> = text.chars().collect();
    let start = position(from, characters.len());
    let end = match to {
        Some(Some(to)) => position(to, characters.len()),
        Some(None) => return Evaluated::Undecided,
        None => characters.len(),
    };
    let taken: String = characters
        .get(start..end.max(start))
        .unwrap_or_default()
        .iter()
        .collect();
    Evaluated::Known(Value::String(taken))
}

/// One character position of a text of `length` characters, clamped into it: a negative
/// position is its start and one past the end is its end.
fn position(stated: f64, length: usize) -> usize {
    if stated <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let stated = stated.trunc() as usize;
    stated.min(length)
}

/// "replace(String, String, String)": every occurrence of the second argument in the first
/// replaced by the third.
fn replace(values: &[&Value]) -> Evaluated {
    match values {
        [text, find, replacement] => {
            match (text_of(text), text_of(find), text_of(replacement)) {
                (Some(text), Some(find), Some(replacement)) if !find.is_empty() => {
                    // The replacement is made only where the result stays inside the bound, so a
                    // long replacement of a frequent occurrence cannot grow without limit.
                    let occurrences = text.matches(&find).count();
                    if occurrences.saturating_mul(replacement.chars().count())
                        > MAX_COMPUTED_TEXT_CHARACTERS
                    {
                        return Evaluated::Undecided;
                    }
                    computed_text(text.replace(&find, &replacement))
                }
                _ => Evaluated::Undecided,
            }
        }
        _ => Evaluated::Undecided,
    }
}

/// "indexof(String, String)": the character position of the first occurrence of the second
/// argument in the first, and `-1` where it does not occur.
fn index_of(values: &[&Value]) -> Evaluated {
    match values {
        [text, part] => match (text_of(text), text_of(part)) {
            (Some(text), Some(part)) => {
                let found = text.find(&part).map_or(-1.0, |byte| {
                    text.get(..byte).map_or(-1.0, |before| {
                        before.chars().count() as f64
                    })
                });
                number(found, true)
            }
            _ => Evaluated::Undecided,
        },
        _ => Evaluated::Undecided,
    }
}

/// "get(collection, index)": one item of an array by its 0-based position, or one member of an
/// object by its name. A position or a name the collection does not carry is SQL's unknown.
fn get(values: &[&Value]) -> Evaluated {
    let [collection, index] = values else {
        return Evaluated::Undecided;
    };
    match (collection, index) {
        (Value::Array(items), index) => match super::as_number(index) {
            Some(position) if position >= 0.0 => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let position = position.trunc() as usize;
                items
                    .get(position)
                    .map_or(Evaluated::Absent, |item| Evaluated::Known(item.clone()))
            }
            _ => Evaluated::Undecided,
        },
        (Value::Object(members), Value::String(name)) => members
            .get(name)
            .map_or(Evaluated::Absent, |member| Evaluated::Known(member.clone())),
        (Value::String(text), index) => match super::as_number(index) {
            Some(position) if position >= 0.0 => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let position = position.trunc() as usize;
                text.chars().nth(position).map_or(Evaluated::Absent, |character| {
                    Evaluated::Known(Value::String(character.to_string()))
                })
            }
            _ => Evaluated::Undecided,
        },
        _ => Evaluated::Undecided,
    }
}

/// [`PARSE_TIME`]: the instant the second argument names, in milliseconds, written out through
/// the pattern the first argument gives.
///
/// The pattern is the one the reference documents, "a Java `SimpleDateFormat` pattern": a RUN of
/// one pattern letter is one field of the instant, text inside single quotes is literal, and
/// every other character is itself ([`formatted`]). A pattern letter this engine does not write
/// makes the call undecided rather than producing a time with a field missing.
///
/// The third argument is the time zone, which "defaults to UTC"; a call naming another zone is
/// one this engine does not compute.
fn parse_time(values: &[&Value]) -> Evaluated {
    let (pattern, instant, zone) = match values {
        [pattern, instant] => (pattern, instant, None),
        [pattern, instant, zone] => (pattern, instant, Some(zone)),
        _ => return Evaluated::Undecided,
    };
    let (Some(pattern), Some(milliseconds)) = (text_of(pattern), super::as_number(instant))
    else {
        return Evaluated::Undecided;
    };
    if let Some(zone) = zone {
        let named = text_of(zone).unwrap_or_default();
        if !UTC_ZONES
            .iter()
            .any(|utc| named.eq_ignore_ascii_case(utc))
        {
            return Evaluated::Undecided;
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    let Some(instant) = ::chrono::DateTime::from_timestamp_millis(milliseconds.trunc() as i64)
    else {
        return Evaluated::Undecided;
    };
    formatted(&pattern, instant).map_or(Evaluated::Undecided, computed_text)
}

/// `instant` written out through the `SimpleDateFormat` pattern `pattern`, or `None` for a
/// pattern carrying a letter this engine does not write.
///
/// One pass over the pattern's characters, each read through [`slice::get`] and each advancing the
/// position, so the walk is linear in the pattern — which is itself bounded, by the statement or
/// the template the pattern came out of — and indexes nothing out of bounds.
fn formatted(pattern: &str, instant: ::chrono::DateTime<::chrono::Utc>) -> Option<String> {
    let characters: Vec<char> = pattern.chars().collect();
    let mut written = String::with_capacity(characters.len());
    let mut at = 0_usize;
    while let Some(character) = characters.get(at).copied() {
        // Text inside single quotes is literal, and a doubled quote is one quote.
        if character == '\'' {
            at += 1;
            if characters.get(at) == Some(&'\'') {
                written.push('\'');
                at += 1;
                continue;
            }
            while let Some(quoted) = characters.get(at).copied().filter(|held| *held != '\'') {
                written.push(quoted);
                at += 1;
            }
            at += 1;
            continue;
        }
        if !character.is_ascii_alphabetic() {
            written.push(character);
            at += 1;
            continue;
        }
        let mut count = 0_usize;
        while characters.get(at + count) == Some(&character) {
            count += 1;
        }
        written.push_str(&field(character, count, instant)?);
        at += count;
    }
    Some(written)
}

/// One field of `instant`, as a run of `count` of the pattern letter `letter` writes it.
fn field(
    letter: char,
    count: usize,
    instant: ::chrono::DateTime<::chrono::Utc>,
) -> Option<String> {
    use ::chrono::{Datelike, Timelike};

    let padded = |value: u32| format!("{value:0count$}", count = count);
    Some(match letter {
        'y' | 'Y' => {
            let year = instant.year();
            if count == 2 {
                format!("{:02}", year.rem_euclid(100))
            } else {
                format!("{:0count$}", year, count = count)
            }
        }
        'M' => match count {
            3 => MONTH_ABBREVIATIONS
                .get((instant.month0()) as usize)?
                .to_string(),
            4.. => MONTH_NAMES.get((instant.month0()) as usize)?.to_string(),
            _ => padded(instant.month()),
        },
        'd' => padded(instant.day()),
        'D' => padded(instant.ordinal()),
        'H' => padded(instant.hour()),
        'h' => padded(match instant.hour() % 12 {
            0 => 12,
            hour => hour,
        }),
        'm' => padded(instant.minute()),
        's' => padded(instant.second()),
        'S' => {
            let milliseconds = format!("{:03}", instant.timestamp_subsec_millis());
            match count {
                0..=3 => milliseconds.get(..count.max(1))?.to_string(),
                _ => format!("{milliseconds:0<count$}"),
            }
        }
        'a' => if instant.hour() < 12 { "AM" } else { "PM" }.to_string(),
        'E' => match count {
            4.. => WEEKDAY_NAMES
                .get(instant.weekday().num_days_from_monday() as usize)?
                .to_string(),
            _ => WEEKDAY_ABBREVIATIONS
                .get(instant.weekday().num_days_from_monday() as usize)?
                .to_string(),
        },
        'z' => UTC_ZONES[0].to_string(),
        'Z' => "+0000".to_string(),
        'X' => "Z".to_string(),
        // A pattern letter this engine does not write.
        _ => return None,
    })
}

/// The month names a `MMM`/`MMMM` run writes, in the English forms `SimpleDateFormat` writes
/// them in.
const MONTH_ABBREVIATIONS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// See [`MONTH_ABBREVIATIONS`].
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// The weekday names an `E`/`EEEE` run writes, Monday first — the order
/// `Weekday::num_days_from_monday` counts in.
const WEEKDAY_ABBREVIATIONS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
/// See [`WEEKDAY_ABBREVIATIONS`].
const WEEKDAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The TEXT a value carries where a function takes text: a string is its own characters, and a
/// number or a boolean the text the document form writes it as — the conversion the reference
/// gives between a scalar and a string. A collection is not text, so a call over one is not
/// computed.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// A computed TEXT value as the value it is, or undecided where it is longer than
/// [`MAX_COMPUTED_TEXT_CHARACTERS`] — the one bound every function that builds text answers to.
fn computed_text(text: String) -> Evaluated {
    if text.chars().count() > MAX_COMPUTED_TEXT_CHARACTERS {
        return Evaluated::Undecided;
    }
    Evaluated::Known(Value::String(text))
}

/// A computed number as the value it is: an integer where the inputs were whole and the result
/// is a whole number `f64` represents exactly, and the number itself otherwise. A result that is
/// not a number a JSON document can carry (a division by zero) is undecided rather than a
/// value.
fn number(computed: f64, whole: bool) -> Evaluated {
    if whole && computed.fract() == 0.0 && computed.abs() < super::INTEGER_RESULT_CEILING {
        #[allow(clippy::cast_possible_truncation)]
        return Evaluated::Known(Value::Number(::serde_json::Number::from(computed as i64)));
    }
    ::serde_json::Number::from_f64(computed).map_or(Evaluated::Undecided, |number| {
        Evaluated::Known(Value::Number(number))
    })
}

/// The clock as [`TIMESTAMP`] reports it: milliseconds since the epoch, the unit that function's
/// own documentation gives. A clock before the epoch — which no host this runs on has — reports
/// the epoch itself rather than a negative instant.
fn epoch_milliseconds() -> i64 {
    ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The topic every row below is evaluated against.
    const TOPIC_NAME: &str = "sensors/room-1/temp";

    /// What a row expects of the call it names.
    fn called(name: &str, arguments: Vec<Evaluated>) -> Evaluated {
        let document = Value::Null;
        call(
            name,
            &arguments,
            Message {
                topic: TOPIC_NAME,
                document: &document,
            },
        )
    }

    /// One argument that IS a value.
    fn value(value: Value) -> Evaluated {
        Evaluated::Known(value)
    }

    /// One row of the table below: what the case is, the function called, its arguments and the
    /// value the call has.
    type Row<'a> = (&'a str, &'a str, Vec<Evaluated>, Evaluated);

    /// The text `text` as an argument.
    fn text(text: &str) -> Evaluated {
        value(Value::String(text.to_string()))
    }

    /// The number `number` as an argument.
    fn digits(number: i64) -> Evaluated {
        value(Value::Number(::serde_json::Number::from(number)))
    }

    #[test]
    fn each_computed_function_of_the_library_answers_what_its_reference_entry_gives() {
        // The rows differ along the FUNCTION called and what its arguments are: the text
        // functions over text and over a number's own text, the number functions over whole and
        // fractional values, the collection reader over an array, an object and a text, and
        // each function called with the wrong number of arguments or over an argument of the
        // wrong kind — which is a call this engine does not compute rather than one it guesses.
        //
        // The one instant below is a FIXED millisecond value, so what `parse_time` writes for
        // it is the pattern's own doing: 2026-01-02T03:04:05.678Z, a Friday.
        const INSTANT: i64 = 1_767_323_045_678;
        let rows: &[Row<'_>] = &[
            (
                "topic() with no argument",
                TOPIC,
                vec![],
                text(TOPIC_NAME),
            ),
            ("topic(2), one segment", TOPIC, vec![digits(2)], text("room-1")),
            (
                "topic(9), past the last segment",
                TOPIC,
                vec![digits(9)],
                Evaluated::Absent,
            ),
            (
                "accountid(), this account's own id",
                ACCOUNTID,
                vec![],
                text(::engine::emulator_account_id()),
            ),
            (
                "concat() over text and a number",
                "concat",
                vec![text("a/"), digits(7), text("/b")],
                text("a/7/b"),
            ),
            ("length() over text", "length", vec![text("abcd")], digits(4)),
            ("upper()", "upper", vec![text("aB")], text("AB")),
            ("lower()", "lower", vec![text("aB")], text("ab")),
            ("trim()", "trim", vec![text("  a  ")], text("a")),
            (
                "substring() to the end",
                "substring",
                vec![text("abcdef"), digits(2)],
                text("cdef"),
            ),
            (
                "substring() with an end position",
                "substring",
                vec![text("abcdef"), digits(1), digits(3)],
                text("bc"),
            ),
            (
                "substring() past the end, which is clamped",
                "substring",
                vec![text("abc"), digits(1), digits(99)],
                text("bc"),
            ),
            (
                "substring() from a negative position, which is the start",
                "substring",
                vec![text("abc"), digits(-5), digits(2)],
                text("ab"),
            ),
            (
                "replace()",
                "replace",
                vec![text("a/b/c"), text("/"), text("-")],
                text("a-b-c"),
            ),
            (
                "startswith() that holds",
                "startswith",
                vec![text("abc"), text("ab")],
                value(Value::Bool(true)),
            ),
            (
                "endswith() that does not hold",
                "endswith",
                vec![text("abc"), text("ab")],
                value(Value::Bool(false)),
            ),
            (
                "indexof() of a part that occurs",
                "indexof",
                vec![text("abcabc"), text("ca")],
                digits(2),
            ),
            (
                "indexof() of a part that does not occur",
                "indexof",
                vec![text("abc"), text("z")],
                digits(-1),
            ),
            (
                "floor() of a fraction",
                "floor",
                vec![value(::serde_json::json!(2.7))],
                value(::serde_json::json!(2.0)),
            ),
            (
                "ceil() of a fraction",
                "ceil",
                vec![value(::serde_json::json!(2.1))],
                value(::serde_json::json!(3.0)),
            ),
            (
                "round() of a fraction",
                "round",
                vec![value(::serde_json::json!(2.5))],
                value(::serde_json::json!(3.0)),
            ),
            ("abs() of a negative integer", "abs", vec![digits(-3)], digits(3)),
            ("sqrt() of a square", "sqrt", vec![digits(9)], digits(3)),
            ("mod() of two integers", "mod", vec![digits(7), digits(3)], digits(1)),
            (
                "power() of two integers",
                "power",
                vec![digits(2), digits(10)],
                digits(1024),
            ),
            (
                "get() of an array item",
                "get",
                vec![value(::serde_json::json!(["a", "b"])), digits(1)],
                text("b"),
            ),
            (
                "get() of an object member",
                "get",
                vec![value(::serde_json::json!({ "a": 1 })), text("a")],
                digits(1),
            ),
            (
                "get() of a member the object does not carry",
                "get",
                vec![value(::serde_json::json!({ "a": 1 })), text("b")],
                Evaluated::Absent,
            ),
            (
                "parse_time() of a date pattern",
                PARSE_TIME,
                vec![text("yyyy/MM/dd"), digits(INSTANT)],
                text("2026/01/02"),
            ),
            (
                "parse_time() of a pattern naming every field",
                PARSE_TIME,
                vec![
                    text("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'"),
                    digits(INSTANT),
                ],
                text("2026-01-02T03:04:05.678Z"),
            ),
            (
                "parse_time() of a pattern naming the month and the weekday by name",
                PARSE_TIME,
                vec![text("EEE, dd MMM yyyy"), digits(INSTANT)],
                text("Fri, 02 Jan 2026"),
            ),
            (
                "parse_time() naming the default time zone",
                PARSE_TIME,
                vec![text("yyyy"), digits(INSTANT), text("UTC")],
                text("2026"),
            ),
            (
                "parse_time() naming another time zone, which this engine does not write",
                PARSE_TIME,
                vec![text("yyyy"), digits(INSTANT), text("Europe/Berlin")],
                Evaluated::Undecided,
            ),
            (
                "parse_time() of a pattern letter this engine does not write",
                PARSE_TIME,
                vec![text("yyyy G"), digits(INSTANT)],
                Evaluated::Undecided,
            ),
            (
                "a function of the library this engine does not compute",
                "get_thing_shadow",
                vec![text("a")],
                Evaluated::Undecided,
            ),
            (
                "a call over an attribute the message does not carry",
                "upper",
                vec![Evaluated::Absent],
                Evaluated::Absent,
            ),
            (
                "a call over an argument this walk could not decide",
                "upper",
                vec![Evaluated::Undecided],
                Evaluated::Undecided,
            ),
            (
                "a text function over a collection, which is not text",
                "upper",
                vec![value(::serde_json::json!(["a"]))],
                Evaluated::Undecided,
            ),
            (
                "a number function over text that spells no number",
                "floor",
                vec![text("abc")],
                Evaluated::Undecided,
            ),
            (
                "a function called with too many arguments",
                "length",
                vec![text("a"), text("b")],
                Evaluated::Undecided,
            ),
            (
                "timestamp() called with an argument",
                TIMESTAMP,
                vec![digits(1)],
                Evaluated::Undecided,
            ),
        ];
        for (case, name, arguments, expected) in rows {
            let actual = called(name, arguments.clone());
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {expected:?} from {name}, got {actual:?}"
            );
        }
    }
}
