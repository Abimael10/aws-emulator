// SPDX-License-Identifier: CC0-1.0

//! The data warehouse types this emulator's engine does not carry: the
//! GEOMETRIC types and the CURRENCY type.
//!
//! A statement submitted through the Data API is evaluated by the data
//! warehouse, whose type system is PostgreSQL's — so a caller writes
//! `point(3, 4)`, `box '((2,3),(9,7))'`, `circle '<(5,6),4>'`,
//! `lseg '[(1,2),(8,9)]'`, `path '[(1,1),(4,2),(6,5)]'`,
//! `polygon '((0,0),(4,0),(4,3),(0,3))'` and `'256.78'::money`, and reads back
//! the VALUE each of those denotes rather than the text it was written as. The
//! engine behind this service implements the SQL core and its own type set, so
//! each type named here is implemented: its input syntax is READ, the value is
//! held as the pair, the list of pairs or the amount it is, and what a caller
//! reads back is that value's own printed form.
//!
//! Each type follows the one it emulates:
//!
//! * a coordinate of a point, a line segment, a rectangle, a circle or a
//!   polygon is reported as a DOUBLE, so it carries a fractional part even when
//!   it is whole — which is why the text a caller gets back for one of those is
//!   normally not the text the statement carried;
//! * a path is reported as the warehouse itself printed it, which is the plain
//!   decimal of each coordinate (see [`GeometricType::coordinate_form`]);
//! * a `box` is its two opposite corners, and a box holds the SAME pair of
//!   corners however a statement names them, so the corner with the greater
//!   coordinates is printed first;
//! * a `path` is open or closed, which the bracket its input carries decides
//!   and the bracket its output carries reports;
//! * a `circle`'s radius is a length, so a negative one is no circle;
//! * an amount of money is held to the cent, so a value carrying more decimal
//!   places than that is rounded to it, and the thousands separators and the
//!   currency symbol an amount may be written with are not part of it;
//! * input that is not a value of the type is an error worded as the warehouse
//!   words it, naming the type and the text it could not read ([`invalid_input`]).
//!
//! The functions registered here ([`install`]) are the type's own
//! CONSTRUCTORS — `point(x, y)`, `box(p1, p2)`, `lseg(p1, p2)`,
//! `circle(p, r)` — plus the one-argument form of each type's name, which is
//! what a typed literal and a cast to that type are read as (see
//! [`super::redshift_sql`]). The type a column of one of them reports is the
//! type's own name, which is where `ColumnMetadata.typeName` comes from (see
//! [`super::column_metadata`]).
//!
//! Every text one of them reads is a value the ENGINE handed it, so its length
//! is bounded by the engine's own value ceiling (`SQLITE_LIMIT_LENGTH`, set to
//! [`super::MAX_VALUE_BYTES`] in [`super::Database::open`]): each function here
//! makes ONE pass over that text and builds a printed form proportional to it,
//! and a printed form past that same ceiling is refused by the engine rather
//! than held.

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Error, Result};

/// `ColumnMetadata.typeName` for a column holding an amount of money, and the
/// name a cast or a typed literal spells that type with.
pub const MONEY_TYPE_NAME: &str = "money";

/// How many decimal places an amount of money is held to: an amount is a number
/// of cents, so a hundredth of the unit is the smallest difference between two
/// amounts.
const MONEY_DECIMAL_PLACES: i32 = 2;

/// One of the warehouse's geometric types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometricType {
    /// A single pair of coordinates: `(x,y)`.
    Point,
    /// A finite line segment, its two endpoints: `[(x1,y1),(x2,y2)]`.
    Lseg,
    /// A rectangle, its two opposite corners: `(x1,y1),(x2,y2)`.
    Box,
    /// A sequence of points, open or closed: `[(..),(..)]` / `((..),(..))`.
    Path,
    /// A closed sequence of points: `((..),(..),(..))`.
    Polygon,
    /// A centre and a radius: `<(x,y),r>`.
    Circle,
}

/// Every geometric type, which is what [`install`] registers and what a type
/// name is resolved against.
pub const GEOMETRIC_TYPES: &[GeometricType] = &[
    GeometricType::Point,
    GeometricType::Lseg,
    GeometricType::Box,
    GeometricType::Path,
    GeometricType::Polygon,
    GeometricType::Circle,
];

impl GeometricType {
    /// `ColumnMetadata.typeName` for a column of this type, which is also the
    /// name a typed literal, a cast and this type's one-argument constructor
    /// spell it with.
    pub fn type_name(self) -> &'static str {
        match self {
            Self::Point => "point",
            Self::Lseg => "lseg",
            Self::Box => "box",
            Self::Path => "path",
            Self::Polygon => "polygon",
            Self::Circle => "circle",
        }
    }

    /// The geometric type `name` spells, whatever case it is written in, or
    /// `None` for a name that is not one of them.
    pub fn named(name: &str) -> Option<Self> {
        GEOMETRIC_TYPES
            .iter()
            .copied()
            .find(|candidate| name.eq_ignore_ascii_case(candidate.type_name()))
    }

    /// How this type's coordinates are PRINTED, which is not the same for every
    /// one of them.
    ///
    /// The Data API reports a value of a type its own client type system
    /// carries — a point, a line segment, a rectangle, a circle, a polygon —
    /// through that type, which holds each coordinate as a DOUBLE and prints it
    /// as one, so a whole coordinate still carries a fractional part. A `path`
    /// is the one it carries no type for: it is reported as the warehouse
    /// itself printed it, which is the plain decimal of each coordinate.
    fn coordinate_form(self) -> CoordinateForm {
        match self {
            Self::Point | Self::Lseg | Self::Box | Self::Circle | Self::Polygon => {
                CoordinateForm::Double
            }
            Self::Path => CoordinateForm::Plain,
        }
    }

    /// Read `text` in this type's input syntax and answer the VALUE's own
    /// printed form, or the warehouse's own refusal for text that is not a
    /// value of this type.
    ///
    /// One forward pass over `text`, with no recursion and no nesting to
    /// descend — a value of these types is a flat list of coordinates, however
    /// many brackets it is written with — and refused outright past the length
    /// the engine holds a value in ([`within_value_ceiling`]), so neither the
    /// work nor the allocation it costs is the caller's to choose.
    pub fn parse(self, text: &str) -> std::result::Result<String, String> {
        if !within_value_ceiling(text) {
            return Err(too_long(self.type_name()));
        }
        let trimmed = text.trim();
        let coordinates = coordinates(trimmed).ok_or_else(|| invalid_input(self.type_name(), text))?;
        let refuse = || invalid_input(self.type_name(), text);
        let form = self.coordinate_form();
        match self {
            Self::Point => match coordinates.as_slice() {
                [x, y] => Ok(point(*x, *y, form)),
                _ => Err(refuse()),
            },
            Self::Lseg => match coordinates.as_slice() {
                [x1, y1, x2, y2] => Ok(format!(
                    "[{},{}]",
                    point(*x1, *y1, form),
                    point(*x2, *y2, form)
                )),
                _ => Err(refuse()),
            },
            // The two corners a box holds are the same pair whichever order a
            // statement names them in, so the one with the greater coordinates
            // is printed first and the pair reads back identically.
            Self::Box => match coordinates.as_slice() {
                [x1, y1, x2, y2] => Ok(format!(
                    "{},{}",
                    point(x1.max(*x2), y1.max(*y2), form),
                    point(x1.min(*x2), y1.min(*y2), form)
                )),
                _ => Err(refuse()),
            },
            // A radius is a length: a negative one describes no circle.
            Self::Circle => match coordinates.as_slice() {
                [x, y, radius] if *radius >= 0.0 => Ok(format!(
                    "<{},{}>",
                    point(*x, *y, form),
                    number(*radius, form)
                )),
                _ => Err(refuse()),
            },
            // A path carries its openness: the bracket the input opens with is
            // the bracket the value is printed with. A polygon is closed by
            // definition, so it is always printed in parentheses.
            Self::Path | Self::Polygon => {
                if coordinates.len() < 2 || coordinates.len() % 2 != 0 {
                    return Err(refuse());
                }
                let points: Vec<String> = coordinates
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| point(pair[0], pair[1], form))
                    .collect();
                let joined = points.join(",");
                if self == Self::Path && trimmed.starts_with('[') {
                    Ok(format!("[{joined}]"))
                } else {
                    Ok(format!("({joined})"))
                }
            }
        }
    }
}

/// How the warehouse states that a text is not a value of the type it was read
/// as: the type it was read as, and the text it could not read.
pub fn invalid_input(type_name: &str, text: &str) -> String {
    format!("invalid input syntax for type {type_name}: \"{text}\"")
}

/// Whether a text is one these types are read from at all: the engine holds no
/// value longer than its own value ceiling, so a longer text is no value of any
/// type and is refused BEFORE it is walked.
fn within_value_ceiling(text: &str) -> bool {
    usize::try_from(super::MAX_VALUE_BYTES).is_ok_and(|ceiling| text.len() <= ceiling)
}

/// How a text past that ceiling is refused. The refusal carries the ceiling and
/// NOT the text, which is what made it too long in the first place.
fn too_long(type_name: &str) -> String {
    format!(
        "string or blob too big: a value of type {type_name} may not exceed {} bytes",
        super::MAX_VALUE_BYTES
    )
}

/// The warehouse type `name` spells — a geometric type or the currency type —
/// whatever case it is written in, or `None` for a name that is neither.
///
/// It is the one place a type NAME is resolved: the dialect reader asks it
/// which casts and typed literals name a type the engine does not carry
/// ([`super::redshift_sql`]), and the column projection asks it which
/// constructor call produces a column of one ([`super::column_origins`]).
pub fn type_named(name: &str) -> Option<&'static str> {
    if let Some(kind) = GeometricType::named(name) {
        return Some(kind.type_name());
    }
    name.eq_ignore_ascii_case(MONEY_TYPE_NAME)
        .then_some(MONEY_TYPE_NAME)
}

/// How a coordinate is printed, which [`GeometricType::coordinate_form`]
/// decides per type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinateForm {
    /// As a DOUBLE: the shortest decimal that reads back as the same number,
    /// carrying a fractional part even when the number is whole.
    Double,
    /// As the warehouse prints a number: the shortest decimal that reads back
    /// as the same number, with no fractional part when it has none.
    Plain,
}

/// One point's printed form: `(x,y)`.
fn point(x: f64, y: f64, form: CoordinateForm) -> String {
    format!("({},{})", number(x, form), number(y, form))
}

/// One coordinate's printed form, in the form its own type prints coordinates
/// in ([`GeometricType::coordinate_form`]).
fn number(value: f64, form: CoordinateForm) -> String {
    match form {
        CoordinateForm::Double => format!("{value:?}"),
        CoordinateForm::Plain => format!("{value}"),
    }
}

/// The characters a geometric value's input syntax carries beside its numbers:
/// the delimiters each type opens, closes and separates its points with, and
/// the whitespace a caller may write between them.
const COORDINATE_DELIMITERS: &[char] = &['(', ')', '[', ']', '<', '>', ',', ' ', '\t', '\n', '\r'];

/// The deepest a value of these types nests: a LIST of points, so a point
/// inside whatever brackets the list is written with. It is an explicit limit
/// rather than a consequence of the input's length, so a text nesting deeper
/// than any value of these types can is refused rather than walked.
const MAX_NESTING: usize = 2;

/// Whether the brackets `text` carries are balanced: every one that is opened
/// is closed by its own kind, in order, and no deeper than a value of these
/// types nests ([`MAX_NESTING`]).
///
/// A bracket a value opens it also closes — a point written `(3,4` is no point
/// — so the brackets are checked here, while the COORDINATES are read on their
/// own ([`coordinates`]); between them they accept exactly the spellings these
/// types admit. One pass, no recursion, and a stack bounded by the nesting
/// limit.
fn balanced(text: &str) -> bool {
    let mut opened: Vec<char> = Vec::with_capacity(MAX_NESTING);
    for character in text.chars() {
        match character {
            '(' | '[' | '<' => {
                if opened.len() == MAX_NESTING {
                    return false;
                }
                opened.push(character);
            }
            ')' | ']' | '>' => {
                let closes = match opened.pop() {
                    Some('(') => ')',
                    Some('[') => ']',
                    Some('<') => '>',
                    _ => return false,
                };
                if closes != character {
                    return false;
                }
            }
            _ => {}
        }
    }
    opened.is_empty()
}

/// The numbers `text` carries, in the order it carries them, or `None` when it
/// carries anything that is not a number or one of the delimiters a geometric
/// value is written with.
///
/// Reading the COORDINATES rather than the brackets is what accepts every
/// spelling one of these types admits — a point as `(3,4)` or `3,4`, a box as
/// `((2,3),(9,7))`, `(2,3),(9,7)` or `2,3,9,7` — while the count of them is
/// what each type checks ([`GeometricType::parse`]).
fn coordinates(text: &str) -> Option<Vec<f64>> {
    if !balanced(text) {
        return None;
    }
    let characters: Vec<char> = text.chars().collect();
    let mut numbers = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if COORDINATE_DELIMITERS.contains(&character) {
            index += 1;
            continue;
        }
        if !(character.is_ascii_digit() || character == '-' || character == '+' || character == '.')
        {
            return None;
        }
        let start = index;
        index += 1;
        while index < characters.len() {
            let next = characters[index];
            let exponent_sign = (next == '-' || next == '+')
                && matches!(characters[index - 1], 'e' | 'E');
            if next.is_ascii_digit() || next == '.' || next == 'e' || next == 'E' || exponent_sign {
                index += 1;
            } else {
                break;
            }
        }
        let token: String = characters[start..index].iter().collect();
        let value: f64 = token.parse().ok()?;
        // A coordinate is a finite number: neither an infinity nor a NaN is a
        // position in the plane.
        if !value.is_finite() {
            return None;
        }
        numbers.push(value);
    }
    (!numbers.is_empty()).then_some(numbers)
}

/// Read `text` as an AMOUNT, in the input syntax the currency type accepts: an
/// optional currency symbol, thousands separators, and parentheses or a leading
/// sign for a negative amount. Answers the amount held to the cent.
pub fn parse_money(text: &str) -> std::result::Result<f64, String> {
    if !within_value_ceiling(text) {
        return Err(too_long(MONEY_TYPE_NAME));
    }
    let trimmed = text.trim();
    let (body, parenthesised) = match trimmed.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
        Some(inner) => (inner.trim(), true),
        None => (trimmed, false),
    };
    // The sign, then the currency symbol: both are written before the amount,
    // and either order is accepted because both spellings are written.
    let (body, negative) = match body.strip_prefix('-') {
        Some(rest) => (rest.trim_start(), true),
        None => (body.strip_prefix('+').unwrap_or(body).trim_start(), false),
    };
    let body = body.strip_prefix(CURRENCY_SYMBOL).unwrap_or(body).trim_start();
    let (body, negative) = match body.strip_prefix('-') {
        Some(rest) => (rest.trim_start(), true),
        None => (body, negative),
    };
    // A thousands separator groups the digits of one amount and is not part of
    // its value.
    let digits: String = body.chars().filter(|character| *character != ',').collect();
    let amount: f64 = digits
        .parse()
        .ok()
        .filter(|value: &f64| value.is_finite())
        .ok_or_else(|| invalid_input(MONEY_TYPE_NAME, text))?;
    let signed = if negative || parenthesised { -amount } else { amount };
    Ok(to_cents(signed))
}

/// The currency symbol an amount may be written with.
const CURRENCY_SYMBOL: char = '$';

/// `amount` held to the cent, which is the resolution an amount of money has.
fn to_cents(amount: f64) -> f64 {
    let scale = 10_f64.powi(MONEY_DECIMAL_PLACES);
    (amount * scale).round() / scale
}

/// Register every type this module implements on `connection`.
///
/// Called from [`super::Database::open`], so a statement naming one of these
/// types finds an implementation of it rather than an unknown function.
pub fn install(connection: &Connection) -> Result<()> {
    // Deterministic: a value's printed form depends on nothing but the value.
    let pure = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;

    for kind in GEOMETRIC_TYPES.iter().copied() {
        // The one-argument form of the type's own name: a typed literal
        // (`box '((2,3),(9,7))'`) and a cast to the type (`'(3,4)'::point`) are
        // both read as this call, and so is a caller writing it directly.
        connection.create_scalar_function(kind.type_name(), 1, pure, move |ctx| {
            let Some(text) = text_argument(ctx, 0) else {
                return Ok(None);
            };
            kind.parse(&text).map(Some).map_err(failed)
        })?;
    }

    // The constructors that take the type's PARTS rather than its text.
    connection.create_scalar_function("point", 2, pure, |ctx| {
        let (Some(x), Some(y)) = (double_argument(ctx, 0)?, double_argument(ctx, 1)?) else {
            return Ok(None);
        };
        Ok(Some(point(x, y, GeometricType::Point.coordinate_form())))
    })?;
    connection.create_scalar_function("lseg", 2, pure, |ctx| {
        two_points(ctx, GeometricType::Lseg)
    })?;
    connection.create_scalar_function("box", 2, pure, |ctx| {
        two_points(ctx, GeometricType::Box)
    })?;
    connection.create_scalar_function("circle", 2, pure, |ctx| {
        let (Some(centre), Some(radius)) = (text_argument(ctx, 0), double_argument(ctx, 1)?) else {
            return Ok(None);
        };
        let form = GeometricType::Circle.coordinate_form();
        let centre = GeometricType::Point.parse(&centre).map_err(failed)?;
        GeometricType::Circle
            .parse(&format!("<{centre},{}>", number(radius, form)))
            .map(Some)
            .map_err(failed)
    })?;

    // The currency type: `'256.78'::money` and `money('256.78')` are the same
    // amount, held as the number of cents it is.
    connection.create_scalar_function(MONEY_TYPE_NAME, 1, pure, |ctx| match ctx.get_raw(0) {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(integer) => Ok(Some(to_cents(integer as f64))),
        ValueRef::Real(real) => Ok(Some(to_cents(real))),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            parse_money(&String::from_utf8_lossy(bytes)).map(Some).map_err(failed)
        }
    })?;

    Ok(())
}

/// A two-point constructor: each argument is a point, in any spelling a point
/// is written in, and the value is read from the pair.
fn two_points(ctx: &Context<'_>, kind: GeometricType) -> Result<Option<String>> {
    let (Some(first), Some(second)) = (text_argument(ctx, 0), text_argument(ctx, 1)) else {
        return Ok(None);
    };
    let first = GeometricType::Point.parse(&first).map_err(failed)?;
    let second = GeometricType::Point.parse(&second).map_err(failed)?;
    kind.parse(&format!("[{first},{second}]"))
        .map(Some)
        .map_err(failed)
}

/// One argument as the text its type is read from. `None` for `NULL`, which
/// makes every function here answer `NULL`, as the whole family is strict.
fn text_argument(ctx: &Context<'_>, index: usize) -> Option<String> {
    match ctx.get_raw(index) {
        ValueRef::Null => None,
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        ValueRef::Integer(integer) => Some(integer.to_string()),
        ValueRef::Real(real) => Some(real.to_string()),
    }
}

/// One argument as a coordinate: a number, or the number a character string
/// spells, which is the engine's own implicit conversion.
fn double_argument(ctx: &Context<'_>, index: usize) -> Result<Option<f64>> {
    match ctx.get_raw(index) {
        ValueRef::Null => Ok(None),
        #[allow(clippy::cast_precision_loss)]
        ValueRef::Integer(integer) => Ok(Some(integer as f64)),
        ValueRef::Real(real) => Ok(Some(real)),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            text.trim()
                .parse::<f64>()
                .ok()
                .filter(|value: &f64| value.is_finite())
                .map(Some)
                .ok_or_else(|| failed(invalid_input("double precision", &text)))
        }
    }
}

/// The engine error one of these functions reports: the text it was given is
/// not a value of the type it was read as.
fn failed(message: String) -> Error {
    Error::UserFunctionError(message.into())
}
