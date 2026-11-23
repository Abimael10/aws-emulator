// SPDX-License-Identifier: CC0-1.0

//! The BINARY format of the PostgreSQL protocol's parameter values, read into the text each
//! value is.
//!
//! A client that binds a parameter sends it in one of two formats, and chooses per parameter:
//! "the format code … 0 for text, 1 for binary". Every modern driver sends some types in
//! binary by default — a byte string, a timestamp, an exact decimal — so an endpoint that
//! read only the text format could not serve an ordinary parameterised statement.
//!
//! What each type's binary form is comes from the type's own send function in the PostgreSQL
//! source, as its documentation describes them: the integers and floats are big-endian, the
//! date and time types count from 2000-01-01 (`POSTGRES_EPOCH`), `numeric` is a sequence of
//! base-10000 digits with a weight and a sign, and the string types are their own bytes.
//!
//! Every value is read into the TEXT the same value has in the text format, because that is
//! what the engine binds: one statement runs with the values the client sent, whichever
//! format it sent them in.

use super::sql_engine::{EngineError, EngineErrorKind};

/// The object identifier of each type this reads, as `pg_type.oid` publishes it. A parameter
/// whose type a client declares as none (`0`) carries no type for its bytes to be read under.
const BOOL: i32 = 16;
const BYTEA: i32 = 17;
const CHAR: i32 = 18;
const NAME: i32 = 19;
const INT8: i32 = 20;
const INT2: i32 = 21;
const INT4: i32 = 23;
const TEXT: i32 = 25;
const OID: i32 = 26;
const JSON: i32 = 114;
const FLOAT4: i32 = 700;
const FLOAT8: i32 = 701;
const BPCHAR: i32 = 1042;
const VARCHAR: i32 = 1043;
const DATE: i32 = 1082;
const TIME: i32 = 1083;
const TIMESTAMP: i32 = 1114;
const TIMESTAMPTZ: i32 = 1184;
const NUMERIC: i32 = 1700;
const UUID: i32 = 2950;
const JSONB: i32 = 3802;

/// Seconds from the Unix epoch to PostgreSQL's own (2000-01-01 00:00:00 UTC), which the
/// date and time types count from.
const POSTGRES_EPOCH_SECONDS: i64 = 946_684_800;

/// Microseconds in a second, and seconds in a day: the units the binary timestamp and date
/// forms are counted in.
const MICROS_PER_SECOND: i64 = 1_000_000;
const SECONDS_PER_DAY: i64 = 86_400;

/// How many decimal digits one `numeric` digit carries: its digits are base-10000.
const NUMERIC_DIGIT_DIGITS: usize = 4;

/// The most DIGITS a `numeric` value carries, and the most it carries after the point:
/// PostgreSQL's own `NUMERIC_MAX_PRECISION` and the scale bound of the type's declaration
/// ("up to 1000 digits of precision"). The binary form's fields are a `u16` and an `i16`
/// wide, so without these the field widths would be the only bound on what one 8-byte header
/// makes this render.
const NUMERIC_MAX_DIGITS: usize = 1_000;
const NUMERIC_MAX_GROUPS: i64 = (NUMERIC_MAX_DIGITS / NUMERIC_DIGIT_DIGITS) as i64;

/// `numeric`'s sign words, as its own send function writes them.
const NUMERIC_NEGATIVE: u16 = 0x4000;
const NUMERIC_NAN: u16 = 0xC000;

/// The value `bytes` carries for the type `oid` in the BINARY format, as the TEXT that value
/// has in the text format.
///
/// `Err` for a type this server does not read in that format, which is a limit of this
/// endpoint rather than a refusal of the statement: it says which type it could not read, so
/// a client can send that one as text.
pub fn parameter_text(oid: i32, bytes: &[u8]) -> Result<String, EngineError> {
    match oid {
        BOOL => Ok(
            // The text form of a boolean is `t` or `f`, which is what a text-format
            // parameter of the same value carries.
            if bytes.first().copied().unwrap_or_default() == 0 {
                "f"
            } else {
                "t"
            }
            .to_string(),
        ),
        INT2 => Ok(read_i16(bytes)?.to_string()),
        INT4 | OID => Ok(read_i32(bytes)?.to_string()),
        INT8 => Ok(read_i64(bytes)?.to_string()),
        FLOAT4 => Ok(f32::from_bits(read_u32(bytes)?).to_string()),
        FLOAT8 => Ok(f64::from_bits(read_u64(bytes)?).to_string()),
        NUMERIC => numeric_text(bytes),
        TEXT | VARCHAR | CHAR | NAME | BPCHAR | JSON => {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
        // `jsonb`'s binary form is a version byte and then the document's own text.
        JSONB => Ok(String::from_utf8_lossy(bytes.get(1..).unwrap_or_default()).into_owned()),
        // A byte string's text form is the hexadecimal format PostgreSQL outputs it in.
        BYTEA => Ok(format!("\\x{}", ::hex::encode(bytes))),
        DATE => {
            // The day count is the client's own four bytes, so the instant it names is
            // carried with checked arithmetic: a count whose microseconds no instant can
            // hold is refused rather than wrapped into another date.
            let days = i64::from(read_i32(bytes)?);
            let at = days
                .checked_mul(SECONDS_PER_DAY)
                .and_then(|seconds| seconds.checked_mul(MICROS_PER_SECOND))
                .ok_or_else(out_of_range)?;
            Ok(rendered(at)
                .split(' ')
                .next()
                .unwrap_or_default()
                .to_string())
        }
        TIME => Ok(clock_text(read_i64(bytes)?)),
        TIMESTAMP => Ok(rendered(read_i64(bytes)?)),
        // The two timestamp types differ in what the value MEANS, not in its binary form:
        // one carries an instant and the other a local time, and both are sent as the
        // microseconds from PostgreSQL's epoch. The text form of an instant carries the
        // offset the server renders it at.
        TIMESTAMPTZ => Ok(format!("{}+00", rendered(read_i64(bytes)?))),
        UUID => uuid_text(bytes),
        _ => Err(EngineError::new(
            EngineErrorKind::FeatureNotSupported,
            format!(
                "this endpoint reads a parameter of type {oid} in the text format only"
            ),
        )),
    }
}

/// The microseconds `at` from PostgreSQL's epoch, in the text form a timestamp has.
fn rendered(at: i64) -> String {
    let seconds = POSTGRES_EPOCH_SECONDS + at.div_euclid(MICROS_PER_SECOND);
    let micros = at.rem_euclid(MICROS_PER_SECOND);
    let rendered = super::pg_server::timestamptz_text(
        f64::from(i32::try_from(micros).unwrap_or_default()) / 1_000_000.0
            + seconds_as_f64(seconds),
    );
    rendered.trim_end_matches("+00").to_string()
}

/// `seconds` as the floating-point instant the renderer takes.
fn seconds_as_f64(seconds: i64) -> f64 {
    ::aws_smithy_types::DateTime::from_secs(seconds).as_secs_f64()
}

/// The microseconds `at` since midnight, in the text form `time` has.
fn clock_text(at: i64) -> String {
    let seconds = at.div_euclid(MICROS_PER_SECOND);
    let micros = at.rem_euclid(MICROS_PER_SECOND);
    format!(
        "{:02}:{:02}:{:02}.{micros:06}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

/// A `uuid`'s sixteen bytes, in the canonical form PostgreSQL prints one in.
fn uuid_text(bytes: &[u8]) -> Result<String, EngineError> {
    let bytes = bytes.get(..16).ok_or_else(truncated)?;
    Ok(crate::state::uuid_text(bytes))
}

/// A `numeric` value, read from the digits, weight, sign and display scale its binary form
/// carries.
fn numeric_text(bytes: &[u8]) -> Result<String, EngineError> {
    let count = usize::from(read_u16(bytes)?);
    let weight = i64::from(read_i16(bytes.get(2..).unwrap_or_default())?);
    let sign = read_u16(bytes.get(4..).unwrap_or_default())?;
    let scale = usize::from(read_u16(bytes.get(6..).unwrap_or_default())?);
    if sign == NUMERIC_NAN {
        return Ok("NaN".to_string());
    }
    // The header's own three numbers decide how much text this renders, and all three come
    // out of the client's bytes: a value past what a `numeric` holds is refused before any of
    // it is built, so an 8-byte header cannot name a 200-KiB number.
    if count > NUMERIC_MAX_DIGITS
        || scale > NUMERIC_MAX_DIGITS
        || !(-NUMERIC_MAX_GROUPS..NUMERIC_MAX_GROUPS).contains(&weight)
    {
        return Err(out_of_range());
    }
    let mut digits = Vec::with_capacity(count);
    for index in 0..count {
        let at = 8 + index * 2;
        digits.push(read_u16(bytes.get(at..).unwrap_or_default())?);
    }
    // The digits are base-10000 and the weight says which of them is the one before the
    // decimal point, so the value is written out digit group by digit group and then the
    // point is placed.
    let mut whole = String::new();
    for position in 0..=weight.max(-1) {
        let digit = digits
            .get(usize::try_from(position).unwrap_or_default())
            .copied()
            .unwrap_or_default();
        if position == 0 {
            whole.push_str(&digit.to_string());
        } else {
            whole.push_str(&format!("{digit:0>width$}", width = NUMERIC_DIGIT_DIGITS));
        }
    }
    if whole.is_empty() {
        whole.push('0');
    }
    let mut fraction = String::new();
    let mut position = weight + 1;
    while fraction.len() < scale {
        let digit = if position < 0 {
            0
        } else {
            digits
                .get(usize::try_from(position).unwrap_or_default())
                .copied()
                .unwrap_or_default()
        };
        fraction.push_str(&format!("{digit:0>width$}", width = NUMERIC_DIGIT_DIGITS));
        position += 1;
    }
    fraction.truncate(scale);
    let sign = if sign == NUMERIC_NEGATIVE { "-" } else { "" };
    if scale == 0 {
        return Ok(format!("{sign}{whole}"));
    }
    Ok(format!("{sign}{whole}.{fraction}"))
}

/// A value whose bytes name an instant no timestamp can hold.
fn out_of_range() -> EngineError {
    EngineError::new(
        EngineErrorKind::NumericValueOutOfRange,
        "date/time field value out of range".to_string(),
    )
}

/// A value whose bytes ended before the type's own form does.
fn truncated() -> EngineError {
    EngineError::new(
        EngineErrorKind::Syntax,
        "the parameter's value is shorter than its type's binary form".to_string(),
    )
}

/// The BINARY form of `value`, for a column declared `declared` — the bytes a
/// `COPY … TO STDOUT WITH (FORMAT binary)` stream carries for the field, and `None` for SQL
/// NULL, which that stream writes as a length of -1.
///
/// The form is the TYPE's own, which is why the column's declaration decides it: the integers
/// and floats are big-endian, a boolean is one byte, `numeric` is the sequence of base-10000
/// digits with a weight, a sign and a display scale that [`numeric_text`] reads, and the
/// string types are their own bytes. A column the statement COMPUTED carries no declaration,
/// so its value's own class decides the form, exactly as the row description reports such a
/// column under the type of the value it carries.
///
/// `Err` for a declared type this endpoint does not WRITE in that format, which is a limit of
/// this endpoint rather than a refusal of the statement: it names the type, so a client can
/// ask for the text format instead.
pub fn copy_field(
    declared: Option<&str>,
    value: &super::sql_engine::Cell,
) -> Result<Option<Vec<u8>>, EngineError> {
    use super::sql_engine::Cell;
    if matches!(value, Cell::Null) {
        return Ok(None);
    }
    let text = || match value {
        Cell::Integer(number) => number.to_string(),
        Cell::Real(number) => number.to_string(),
        Cell::Text(text) => text.clone(),
        Cell::Blob(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Cell::Null => String::new(),
    };
    let number = || match value {
        Cell::Integer(number) => *number as f64,
        Cell::Real(number) => *number,
        other => text_of(other).trim().parse::<f64>().unwrap_or_default(),
    };
    let whole = || match value {
        Cell::Integer(number) => *number,
        Cell::Real(number) => *number as i64,
        other => text_of(other).trim().parse::<i64>().unwrap_or_default(),
    };
    // The base of the declared type, without the precision or the length it carries.
    let base = declared.map(|declared| {
        declared
            .trim()
            .to_ascii_lowercase()
            .split('(')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    let bytes = match base.as_deref() {
        Some("smallint" | "int2") => i16::try_from(whole())
            .unwrap_or_default()
            .to_be_bytes()
            .to_vec(),
        Some("int" | "integer" | "int4" | "serial" | "oid") => i32::try_from(whole())
            .unwrap_or_default()
            .to_be_bytes()
            .to_vec(),
        Some("bigint" | "int8" | "bigserial") => whole().to_be_bytes().to_vec(),
        Some("real" | "float4") => (number() as f32).to_bits().to_be_bytes().to_vec(),
        Some("double precision" | "double" | "float8" | "float") => {
            number().to_bits().to_be_bytes().to_vec()
        }
        Some("bool" | "boolean") => vec![u8::from(boolean(value))],
        Some("numeric" | "decimal" | "dec" | "fixed") => numeric_bytes(&text())?,
        Some(
            "text" | "varchar" | "character varying" | "char" | "character" | "bpchar" | "name"
            | "json" | "citext",
        ) => text().into_bytes(),
        Some("bytea" | "blob") => match value {
            Cell::Blob(bytes) => bytes.clone(),
            other => text_of(other).into_bytes(),
        },
        // A column the statement computed: the value's own class says what it is.
        None => match value {
            Cell::Integer(number) => number.to_be_bytes().to_vec(),
            Cell::Real(number) => number.to_bits().to_be_bytes().to_vec(),
            Cell::Text(text) => text.clone().into_bytes(),
            Cell::Blob(bytes) => bytes.clone(),
            Cell::Null => Vec::new(),
        },
        Some(named) => {
            return Err(EngineError::new(
                EngineErrorKind::FeatureNotSupported,
                format!("this endpoint writes a column of type {named} in the text format only"),
            )
            .with_sqlstate("0A000"))
        }
    };
    Ok(Some(bytes))
}

/// One value's own text, for the readings above that need it from a borrowed cell.
fn text_of(value: &super::sql_engine::Cell) -> String {
    use super::sql_engine::Cell;
    match value {
        Cell::Integer(number) => number.to_string(),
        Cell::Real(number) => number.to_string(),
        Cell::Text(text) => text.clone(),
        Cell::Blob(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Cell::Null => String::new(),
    }
}

/// Whether `value` is TRUE, in each of the forms a boolean column's value is held in: the
/// engine's own 0 and 1, and the text forms both engines read as true.
fn boolean(value: &super::sql_engine::Cell) -> bool {
    use super::sql_engine::Cell;
    match value {
        Cell::Integer(number) => *number != 0,
        Cell::Real(number) => *number != 0.0,
        other => matches!(
            text_of(other).trim().to_ascii_lowercase().as_str(),
            "t" | "true" | "y" | "yes" | "on" | "1"
        ),
    }
}

/// The BINARY form of the `numeric` value `text` names: the base-10000 digits it carries, the
/// weight of the first of them, its sign and the display scale its text has — the four fields
/// [`numeric_text`] reads a value back out of.
fn numeric_bytes(text: &str) -> Result<Vec<u8>, EngineError> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("nan") {
        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0i16.to_be_bytes());
        out.extend_from_slice(&NUMERIC_NAN.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        return Ok(out);
    }
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || whole.len() + fraction.len() > NUMERIC_MAX_DIGITS
    {
        return Err(EngineError::new(
            EngineErrorKind::DataError,
            format!("invalid input syntax for type numeric: \"{text}\""),
        ));
    }
    // The digits are grouped in fours FROM the decimal point in both directions, which is
    // what the weight then counts in.
    let scale = fraction.len();
    let leading = (NUMERIC_DIGIT_DIGITS - whole.len() % NUMERIC_DIGIT_DIGITS) % NUMERIC_DIGIT_DIGITS;
    let trailing =
        (NUMERIC_DIGIT_DIGITS - fraction.len() % NUMERIC_DIGIT_DIGITS) % NUMERIC_DIGIT_DIGITS;
    let padded = format!("{}{whole}{fraction}{}", "0".repeat(leading), "0".repeat(trailing));
    let mut groups: Vec<u16> = padded
        .as_bytes()
        .chunks(NUMERIC_DIGIT_DIGITS)
        .map(|chunk| {
            String::from_utf8_lossy(chunk)
                .parse::<u16>()
                .unwrap_or_default()
        })
        .collect();
    let whole_groups = (whole.len() + leading) / NUMERIC_DIGIT_DIGITS;
    let mut weight = i64::try_from(whole_groups).unwrap_or_default() - 1;
    // The leading and trailing zero groups carry nothing: PostgreSQL's own form holds only
    // the digits between the first and the last that is not zero, with the weight saying
    // where they sit.
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        // Zero carries no digits at all, and sits at weight zero.
        weight = 0;
    }
    let mut out = Vec::with_capacity(8 + groups.len() * 2);
    out.extend_from_slice(&u16::try_from(groups.len()).unwrap_or_default().to_be_bytes());
    out.extend_from_slice(&i16::try_from(weight).unwrap_or_default().to_be_bytes());
    out.extend_from_slice(&if negative { NUMERIC_NEGATIVE } else { 0u16 }.to_be_bytes());
    out.extend_from_slice(&u16::try_from(scale).unwrap_or_default().to_be_bytes());
    for group in groups {
        out.extend_from_slice(&group.to_be_bytes());
    }
    Ok(out)
}

fn read_i16(bytes: &[u8]) -> Result<i16, EngineError> {
    let bytes = bytes.get(..2).ok_or_else(truncated)?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u16(bytes: &[u8]) -> Result<u16, EngineError> {
    let bytes = bytes.get(..2).ok_or_else(truncated)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(bytes: &[u8]) -> Result<u32, EngineError> {
    let bytes = bytes.get(..4).ok_or_else(truncated)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(bytes: &[u8]) -> Result<u64, EngineError> {
    let bytes = bytes.get(..8).ok_or_else(truncated)?;
    Ok(u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

fn read_i32(bytes: &[u8]) -> Result<i32, EngineError> {
    let bytes = bytes.get(..4).ok_or_else(truncated)?;
    Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_i64(bytes: &[u8]) -> Result<i64, EngineError> {
    let bytes = bytes.get(..8).ok_or_else(truncated)?;
    Ok(i64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a parameter sent in the binary format carries the same value as the same
    /// parameter sent as text, whatever its type.
    ///
    /// The rows span the type classes the format differs by: the integers, the floating
    /// point, the exact decimal, the strings, the byte strings, the date and time types and
    /// the identifier types.
    #[test]
    fn a_binary_parameter_reads_as_the_value_its_text_form_carries() {
        let timestamp = 793_152_000_000_000i64; // 2025-02-18 00:00:00 from the 2000 epoch.
        let rows: &[(&str, i32, Vec<u8>, &str)] = &[
            ("a false boolean", BOOL, vec![0], "f"),
            ("a true boolean", BOOL, vec![1], "t"),
            ("a small integer", INT2, 1234i16.to_be_bytes().to_vec(), "1234"),
            ("an integer", INT4, (-7i32).to_be_bytes().to_vec(), "-7"),
            (
                "a big integer",
                INT8,
                9_007_199_254_740_993i64.to_be_bytes().to_vec(),
                "9007199254740993",
            ),
            (
                "double precision",
                FLOAT8,
                2.5f64.to_bits().to_be_bytes().to_vec(),
                "2.5",
            ),
            ("text", TEXT, b"hello".to_vec(), "hello"),
            ("a byte string", BYTEA, vec![0x00, 0xff, 0x10], "\\x00ff10"),
            (
                "a date",
                DATE,
                9_180i32.to_be_bytes().to_vec(),
                "2025-02-18",
            ),
            (
                "a timestamp",
                TIMESTAMP,
                timestamp.to_be_bytes().to_vec(),
                "2025-02-18 00:00:00.000000",
            ),
            (
                "an instant",
                TIMESTAMPTZ,
                timestamp.to_be_bytes().to_vec(),
                "2025-02-18 00:00:00.000000+00",
            ),
            (
                "a time of day",
                TIME,
                ((13 * 3600 + 5 * 60 + 9) * MICROS_PER_SECOND)
                    .to_be_bytes()
                    .to_vec(),
                "13:05:09.000000",
            ),
            (
                "a uuid",
                UUID,
                (0..16u8).collect::<Vec<u8>>(),
                "00010203-0405-0607-0809-0a0b0c0d0e0f",
            ),
            (
                // 12.34: one digit before the point (12) and one after (3400), scale 2.
                "an exact decimal",
                NUMERIC,
                [
                    2i16.to_be_bytes(),
                    0i16.to_be_bytes(),
                    0i16.to_be_bytes(),
                    2i16.to_be_bytes(),
                    12i16.to_be_bytes(),
                    3_400i16.to_be_bytes(),
                ]
                .concat(),
                "12.34",
            ),
            (
                "a negative exact decimal",
                NUMERIC,
                [
                    1i16.to_be_bytes(),
                    0i16.to_be_bytes(),
                    0x4000i16.to_be_bytes(),
                    0i16.to_be_bytes(),
                    5i16.to_be_bytes(),
                ]
                .concat(),
                "-5",
            ),
        ];
        for (case, oid, bytes, expected) in rows {
            let read = parameter_text(*oid, bytes)
                .unwrap_or_else(|error| panic!("case {case:?}: must read, got {error:?}"));
            assert_eq!(
                &read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: a value whose bytes name an instant no timestamp can hold, or a number past what
    /// a `numeric` holds, is refused rather than rendered.
    #[test]
    fn a_date_outside_the_range_a_timestamp_holds_is_refused() {
        for (case, bytes) in [
            ("the largest day count the wire can carry", i32::MAX),
            ("the smallest", i32::MIN),
        ] {
            let error = parameter_text(DATE, &bytes.to_be_bytes())
                .expect_err(&format!("case {case:?}: must be refused"));
            assert_eq!(
                error.kind,
                EngineErrorKind::NumericValueOutOfRange,
                "case {case:?}: expected the out-of-range class, got {error:?}"
            );
        }
        // The `numeric` header's own three numbers, each past what the type holds: eight
        // bytes must not name a number this renders.
        let rows: &[(&str, [i16; 4])] = &[
            ("a weight past the type's precision", [0, 32_767, 0, 0]),
            ("a weight below it", [0, -32_768, 0, 0]),
            ("a display scale past it", [0, 0, 0, 32_767]),
            ("more digits than it holds", [32_767, 0, 0, 0]),
        ];
        for (case, header) in rows {
            let bytes: Vec<u8> = header
                .iter()
                .flat_map(|field| field.to_be_bytes())
                .collect();
            let error = parameter_text(NUMERIC, &bytes)
                .expect_err(&format!("case {case:?}: must be refused"));
            assert_eq!(
                error.kind,
                EngineErrorKind::NumericValueOutOfRange,
                "case {case:?}: expected the out-of-range class, got {error:?}"
            );
        }
    }

    /// RULE: a type this endpoint cannot read in the binary format says so, rather than
    /// being read as something else.
    #[test]
    fn a_type_this_endpoint_does_not_read_in_binary_says_so() {
        let unknown = 3_220; // `pg_lsn`, which no parameter of this server's statements is.
        let error = parameter_text(unknown, &[0u8; 8])
            .expect_err("a type this endpoint does not read must be refused");
        assert_eq!(
            error.kind,
            EngineErrorKind::FeatureNotSupported,
            "a format this endpoint does not read is a limit of the endpoint, got {:?}",
            error.kind
        );
    }
}
