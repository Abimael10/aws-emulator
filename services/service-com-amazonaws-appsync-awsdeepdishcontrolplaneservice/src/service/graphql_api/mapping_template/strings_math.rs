// SPDX-License-Identifier: CC0-1.0

//! The `$util.str` and `$util.math` helpers a mapping template may call — the string and
//! math families of the AppSync resolver mapping-template utility reference.
//!
//! Both are SCALAR families: every helper takes scalars and answers one, so neither reads
//! nor writes any state and neither walks a composite value. They are stated together
//! because they are one concern — the arithmetic and the text handling a template does on
//! the values it already holds — and are reached through the one `$util` registry
//! ([`super::utilities`]), which is what decides that a name no family carries is a fault
//! rather than an empty value.
//!
//! A helper is handed the values the call named, in the order the reference states its
//! parameters, and a value of the wrong kind is a FAULT naming it rather than a result
//! nothing computed: `$util.math.maxVal` over text that is no number has nothing to
//! compare.

use ::serde_json::Value as Json;

use super::values;

/// What one `$util.str.<helper>` call came to, or `None` when `helper` names no helper of
/// the string family — which the registry reports as the fault naming it.
///
/// The family is `toUpper` / `toLower` ("returns the string value converted to
/// upper[lower]case"), `toReplace` ("returns the string value with all occurrences of the
/// second argument replaced by the third") and `normalize` ("returns the normalized string
/// using one of the four unicode normalization forms: NFC, NFD, NFKC, or NFKD").
pub fn string_call(helper: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let text = |index: usize| text_at(args, index);
    let produced = match helper {
        "toUpper" => Json::String(text(0).to_uppercase()),
        "toLower" => Json::String(text(0).to_lowercase()),
        "toReplace" => Json::String(replaced(&text(0), &text(1), &text(2))?),
        "normalize" => Json::String(normalized(&text(0), &text(1))?),
        _ => return Ok(None),
    };
    // The backstop over what was built, beside the ceiling each helper that can amplify its
    // argument asks BEFORE it allocates ([`replaced`]): one reading of how large a value this
    // evaluation may hold.
    super::weigh(&produced, 0)?;
    Ok(Some(produced))
}

/// `subject` with every occurrence of `pattern` replaced by `replacement` — what
/// `$util.str.toReplace` answers — refused BEFORE it is built where the result would be
/// larger than this evaluation may hold.
///
/// The result's size is the one thing about this helper that is not bounded by its arguments:
/// rewriting a one-character pattern as a long replacement grows the subject by the ratio
/// between them, so the size is computed from the occurrence count first and the allocation is
/// never made for a result that would be refused ([`super::within_value_bytes`]). A pattern
/// that is EMPTY matches between every character, which is how that count is maximised, so it
/// is a fault of its own rather than a replacement.
fn replaced(subject: &str, pattern: &str, replacement: &str) -> Result<String, String> {
    if pattern.is_empty() {
        return Err(NO_PATTERN.to_string());
    }
    let occurrences = subject.matches(pattern).count();
    let projected = subject
        .len()
        .saturating_sub(occurrences.saturating_mul(pattern.len()))
        .saturating_add(occurrences.saturating_mul(replacement.len()));
    super::within_value_bytes(projected)?;
    Ok(subject.replace(pattern, replacement))
}

/// What one `$util.math.<helper>` call came to, or `None` when `helper` names no helper of
/// the math family.
///
/// The family is `roundNum` ("takes a double and rounds it to the nearest integer"),
/// `minVal` / `maxVal` ("returns the minimum[maximum] value of the two arguments"),
/// `randomDouble` ("returns a random double between 0 and 1") and `randomWithinRange`
/// ("returns a random integer value within the specified range").
pub fn math_call(helper: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let number = |index: usize| number_at(args, index);
    let produced = match helper {
        "roundNum" => values::number(number(0)?.round()),
        "minVal" => values::number(number(0)?.min(number(1)?)),
        "maxVal" => values::number(number(0)?.max(number(1)?)),
        "randomDouble" => values::number(::rand::random::<f64>()),
        // The range is read the way Java's own `Random.nextInt(bound)` is: an INTEGER is drawn,
        // the start is drawn and the end is not, so a range naming the same value twice draws
        // nothing. Both bounds are raised to the next whole number first, so the answer is
        // always within the range the reference states even where the caller stated a
        // fractional one. Both are finite ([`number_at`]) and the WIDTH between them is checked
        // too, because two finite bounds of opposite sign can span more than a number can hold.
        "randomWithinRange" => {
            let (start, end) = (number(0)?.ceil(), number(1)?.ceil());
            if end <= start || !(end - start).is_finite() {
                return Err(format!("{NOT_A_RANGE} {start} to {end}."));
            }
            let drawn = ::rand::Rng::random_range(&mut ::rand::rng(), start..end);
            values::number(drawn.floor())
        }
        _ => return Ok(None),
    };
    Ok(Some(produced))
}

/// The TEXT the argument at `index` carries: a string is its own content, an absent value
/// and a null are the empty string, and every other value is the text this language renders
/// it as — the reading every other helper of this language gives a text argument.
fn text_at(args: &[Json], index: usize) -> String {
    match args.get(index) {
        Some(Json::String(held)) => held.clone(),
        Some(Json::Null) | None => String::new(),
        Some(other) => values::rendered(other),
    }
}

/// The NUMBER the argument at `index` carries. Text that reads as a number is taken as that
/// number — the form an argument built out of template text arrives in — and anything else
/// is nothing a math helper can compute with.
///
/// A reading that is not FINITE is one of those: `NaN` and the infinities compare to nothing
/// (so no range over them is ordered) and JSON holds neither, so a value of this language is
/// never one and a helper is never handed one.
fn number_at(args: &[Json], index: usize) -> Result<f64, String> {
    let finite = |held: f64, stated: String| match held.is_finite() {
        true => Ok(held),
        false => Err(format!("{NOT_A_NUMBER} {stated}")),
    };
    match args.get(index) {
        Some(Json::Number(held)) => match held.as_f64() {
            Some(reading) => finite(reading, held.to_string()),
            None => Err(format!("{NOT_A_NUMBER} {held}")),
        },
        Some(Json::String(held)) => match held.trim().parse::<f64>() {
            Ok(reading) => finite(reading, held.clone()),
            Err(_) => Err(format!("{NOT_A_NUMBER} {held}")),
        },
        Some(other) => Err(format!("{NOT_A_NUMBER} {}", values::rendered(other))),
        None => Err(NO_NUMBER.to_string()),
    }
}

/// `subject` in the normalization `form` names, which is one of the four Unicode forms the
/// helper publishes, read without regard to case. A form this does not name is a fault: the
/// template asked for a normalization that does not exist.
fn normalized(subject: &str, form: &str) -> Result<String, String> {
    use ::unicode_normalization::UnicodeNormalization as _;
    match form.trim().to_ascii_lowercase().as_str() {
        "nfc" => Ok(subject.nfc().collect()),
        "nfd" => Ok(subject.nfd().collect()),
        "nfkc" => Ok(subject.nfkc().collect()),
        "nfkd" => Ok(subject.nfkd().collect()),
        other => Err(format!("{NO_SUCH_FORM} {other}")),
    }
}

/// The sentences a fault of either family states.
const NOT_A_NUMBER: &str = "This value is not a number:";
const NO_NUMBER: &str = "This helper compares two numbers, and one of them was not given.";
const NO_PATTERN: &str =
    "A replacement names the text it replaces, and this one names nothing to replace.";
const NOT_A_RANGE: &str =
    "A random value is drawn within a range whose end is above its start by a width a number \
     can hold, and this one runs";
const NO_SUCH_FORM: &str = "No such Unicode normalization form:";
