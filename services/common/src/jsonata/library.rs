// SPDX-License-Identifier: CC0-1.0

//! The functions of the language's own library that this engine does not bind.
//!
//! [`::jsonata_rs`] describes itself as an incomplete implementation of JSONata, and the
//! part it leaves out that a caller notices is the FUNCTION LIBRARY: a few of the
//! functions the language defines have no binding in it, so an expression that calls one
//! does not compute a different value — it throws `T1006: Attempted to invoke a
//! non-function`, exactly as if the caller had written a typo. A service evaluating such
//! an expression then has no value to carry, which makes a perfectly ordinary expression
//! unserveable.
//!
//! So the missing bindings are supplied here, once, on the engine's own extension seam
//! ([`::jsonata_rs::JsonAta::register_function`]), and every caller of
//! [`super::evaluate`] gets them. Each one computes its value from the arguments it is
//! handed, as the language defines that function, and none of them opens a host surface
//! the engine does not already open: no filesystem, process, socket, environment or
//! module import, and nothing that compiles further expression text (the language's
//! `$eval` is deliberately NOT among them — an expression must not be able to build a
//! new expression).
//!
//! What is bound, with the language's own definition of each:
//!
//! * `$type(value)` — the type of a value, as one of the names the language lists;
//! * `$average(array)` — the mean of an array of numbers;
//! * `$spread(object)` — an object split into one single-pair object per key;
//! * `$sift(object, function)` — the pairs of an object its predicate keeps;
//! * `$shuffle(array)` — the members of an array in a random order;
//! * `$encodeUrlComponent`, `$encodeUrl`, `$decodeUrlComponent`, `$decodeUrl` — the
//!   percent-escaping of a URL, over the character sets each of the four names.
//!
//! NOT bound, because the language defines them by reference to the picture-string
//! notation of XPath 3.1 rather than by a computation over their arguments:
//! `$formatNumber`, `$formatInteger` and `$parseInteger`. That notation is a small
//! language of its own, and a partial reading of one would answer a picture it misread
//! with a plausible wrong string; an expression using one of the three keeps the
//! engine's own `T1006`, which says the binding is not there.
//!
//! Every function below is bounded AHEAD of the step it runs in, because a native
//! function is ONE interpreter step: the engine checks its clock between steps and
//! clamps memory on the arena, so neither reaches inside one. Each builds its result in
//! that arena (whose allocation limit refuses an over-ceiling layout) and, where it calls
//! a caller's function per member, calls it through the evaluator. And each WALK over a
//! value a caller handed in carries an explicit node budget ([`MAX_WALKED_VALUES`]) and
//! is ITERATIVE over an explicit queue — because the engine's values form a DAG rather
//! than a tree, so an expression that names one binding twice per level (`$a := [$a, $a]`)
//! costs the arena one pointer per place and DOUBLES what a walk over the value visits,
//! which neither the arena ceiling nor this thread's stack bounds.

use ::percent_encoding::{AsciiSet, NON_ALPHANUMERIC};

use ::jsonata_rs::{ArrayFlags, Error, FunctionContext, JsonAta, Result, Value};

/// Bind every function this module supplies on `parsed`, before it is evaluated.
///
/// Called by [`super::contained`] for the expressions this crate's seam evaluates, and
/// by a service that contains the engine itself, so one expression means the same thing
/// wherever this emulator evaluates it.
pub fn bind(parsed: &JsonAta<'_>) {
    parsed.register_function("type", 1, type_of);
    parsed.register_function("average", 1, average);
    parsed.register_function("spread", 1, spread);
    parsed.register_function("sift", 2, sift);
    parsed.register_function("shuffle", 1, shuffle);
    parsed.register_function("encodeUrlComponent", 1, encode_url_component);
    parsed.register_function("encodeUrl", 1, encode_url);
    parsed.register_function("decodeUrlComponent", 1, decode_url_component);
    parsed.register_function("decodeUrl", 1, decode_url);
}

/// The name `$type` answers for a value that is not there: the language's `undefined` is
/// no type of its own, so the call produces nothing at all.
///
/// `$type(value)`: "Evaluates the type of value and returns one of the following
/// strings: 'null', 'number', 'string', 'boolean', 'array', 'object', 'function'.
/// Returns (non-string) undefined when value is undefined."
fn type_of<'a>(context: FunctionContext<'a, '_>, args: &[&'a Value<'a>]) -> Result<&'a Value<'a>> {
    let value = argument(args, 0);
    if value.is_undefined() {
        return Ok(Value::undefined());
    }
    let named = if value.is_null() {
        "null"
    } else if value.is_bool() {
        "boolean"
    } else if value.is_number() {
        "number"
    } else if value.is_string() {
        "string"
    } else if value.is_array() {
        "array"
    } else if value.is_function() {
        "function"
    } else {
        "object"
    };
    Ok(Value::string(context.arena, named))
}

/// `$average(array)`: "Returns the mean value of an array of numbers. It is an error if
/// the array contains an item which is not a number."
///
/// A value that is not there produces nothing, as every function of the library does for
/// `undefined`, and so does an EMPTY array: there is no mean of no numbers, and the
/// language's own numeric type cannot carry one.
fn average<'a>(context: FunctionContext<'a, '_>, args: &[&'a Value<'a>]) -> Result<&'a Value<'a>> {
    let value = argument(args, 0);
    if value.is_undefined() {
        return Ok(Value::undefined());
    }
    let mut total = 0.0_f64;
    let mut counted = 0_usize;
    for member in sequence(value) {
        if !member.is_number() {
            return Err(Error::T0412ArgumentMustBeArrayOfType(
                context.char_index,
                FIRST_ARGUMENT,
                context.name.to_string(),
                "number".to_string(),
            ));
        }
        total += member.as_f64();
        counted += 1;
    }
    if counted == 0 {
        return Ok(Value::undefined());
    }
    Ok(Value::number(context.arena, total / counted as f64))
}

/// `$spread(object)`: "Splits an object containing key/value pairs into an array of
/// objects, each of which has a single key/value pair from the input object. If the
/// parameter is an array, then the resulting array contains an element for each member
/// of the input array."
///
/// A value that is neither — a string, a number, a function — is its own spread, which
/// is what the language answers for it. The result is a SEQUENCE, so a spread that comes
/// to one pair is that pair rather than a one-member array, exactly as every other
/// sequence-producing function of the library reads.
///
/// The walk over a nested array is ITERATIVE, over an explicit queue, and every value it
/// visits or queues is charged to one budget ([`MAX_WALKED_VALUES`]), so neither a
/// caller's nesting nor a value aliased into many places can drive it past what one
/// interpreter step may do.
fn spread<'a>(context: FunctionContext<'a, '_>, args: &[&'a Value<'a>]) -> Result<&'a Value<'a>> {
    spread_into(context.arena, argument(args, 0))
}

/// The spread itself, over the arena it builds its result in rather than over the call
/// that asked for it — which is what lets this walk be driven directly by a test.
fn spread_into<'a>(
    arena: &'a ::bumpalo::Bump,
    value: &'a Value<'a>,
) -> Result<&'a Value<'a>> {
    if !value.is_array() && !value.is_object() {
        return Ok(value);
    }
    let mut left = MAX_WALKED_VALUES;
    let spread = Value::array(arena, ArrayFlags::SEQUENCE);
    let mut pending: ::std::collections::VecDeque<&'a Value<'a>> =
        ::std::collections::VecDeque::new();
    pending.push_back(value);
    while let Some(value) = pending.pop_front() {
        walked(&mut left)?;
        if value.is_array() {
            // Pushed to the FRONT in order, so the members of a nested array are spread
            // where that array sat rather than after everything beside it. Charged as it
            // is queued as well as when it is visited, so the queue itself cannot grow
            // past the budget either.
            for member in value.members().collect::<Vec<_>>().into_iter().rev() {
                walked(&mut left)?;
                pending.push_front(member);
            }
            continue;
        }
        if value.is_object() {
            for (key, member) in value.entries() {
                walked(&mut left)?;
                let pair = Value::object(arena);
                pair.insert(key, member);
                spread.push(pair);
            }
            continue;
        }
        spread.push(value);
    }
    Ok(spread)
}

/// `$sift(object, function)`: "Returns an object that contains only the key/value pairs
/// from the object parameter that satisfy the predicate function passed in as the second
/// parameter. The function that is supplied as the second parameter must have the
/// following signature: `function(value [, key [, object]])`."
///
/// An object no pair of which satisfies the predicate produces NOTHING, which is what
/// the language answers rather than an empty object.
///
/// The first argument is held to being an OBJECT — the one type the function is declared
/// over — before its pairs are read, so a value of any other type is refused in the
/// language's own terms rather than read as one.
///
/// What the predicate's answers cost is charged to ONE budget across every pair, so the
/// PRODUCT of the pairs and the values each answer carries is bounded rather than each
/// factor alone ([`truthy`]).
fn sift<'a>(context: FunctionContext<'a, '_>, args: &[&'a Value<'a>]) -> Result<&'a Value<'a>> {
    let value = argument(args, 0);
    let predicate = argument(args, 1);
    if value.is_undefined() {
        return Ok(Value::undefined());
    }
    if !value.is_object() {
        return Err(Error::T0410ArgumentNotValid(
            context.char_index,
            FIRST_ARGUMENT,
            context.name.to_string(),
        ));
    }
    if !predicate.is_function() {
        return Err(Error::T0410ArgumentNotValid(
            context.char_index,
            SECOND_ARGUMENT,
            context.name.to_string(),
        ));
    }
    let mut left = MAX_WALKED_VALUES;
    let sifted = Value::object(context.arena);
    let mut kept = 0_usize;
    for (key, member) in value.entries() {
        walked(&mut left)?;
        let named = Value::string(context.arena, key);
        // As many arguments as the predicate takes, which is how the language applies
        // every function it hands a value, a key and the object itself to.
        let applied: Vec<&'a Value<'a>> = match predicate.arity() {
            0 | 1 => vec![member],
            2 => vec![member, named],
            _ => vec![member, named, value],
        };
        if truthy(context.evaluate_function(predicate, &applied)?, &mut left)? {
            sifted.insert(key, member);
            kept += 1;
        }
    }
    if kept == 0 {
        return Ok(Value::undefined());
    }
    Ok(sifted)
}

/// `$shuffle(array)`: "Returns an array containing all the values from the array
/// parameter, but shuffled into random order."
///
/// The order is drawn from the host's own randomness ([`seed`]), so two calls on one
/// array are two independent orders — which is what a caller asking for a shuffle asked
/// for. It is a shuffle rather than a cryptographic one: the draw comes from one 64-bit
/// state, so it selects among as many orders as that state has values and not among every
/// permutation of a long array.
fn shuffle<'a>(context: FunctionContext<'a, '_>, args: &[&'a Value<'a>]) -> Result<&'a Value<'a>> {
    let value = argument(args, 0);
    if value.is_undefined() {
        return Ok(Value::undefined());
    }
    let mut members: Vec<&'a Value<'a>> = sequence(value).collect();
    // One pass of Fisher-Yates: each member is moved once, so the cost is the member
    // count the arena already bounds.
    let mut state = seed();
    for position in (1..members.len()).rev() {
        state = next(state);
        let drawn = (state % (position as u64 + 1)) as usize;
        members.swap(position, drawn);
    }
    let shuffled = Value::array(context.arena, ArrayFlags::SEQUENCE);
    for member in members {
        shuffled.push(member);
    }
    Ok(shuffled)
}

/// `$encodeUrlComponent(str)`: "Encodes a Uniform Resource Locator (URL) component by
/// replacing each instance of certain characters by one, two, three, or four escape
/// sequences representing the UTF-8 encoding of the character."
fn encode_url_component<'a>(
    context: FunctionContext<'a, '_>,
    args: &[&'a Value<'a>],
) -> Result<&'a Value<'a>> {
    encoded(context, args, COMPONENT_ESCAPED)
}

/// `$encodeUrl(str)`: "Encodes a Uniform Resource Locator (URL) by replacing each
/// instance of certain characters by one, two, three, or four escape sequences
/// representing the UTF-8 encoding of the character."
///
/// A WHOLE URL rather than one component of it, so the characters that delimit a URL's
/// own parts are left as they are ([`URL_ESCAPED`]).
fn encode_url<'a>(
    context: FunctionContext<'a, '_>,
    args: &[&'a Value<'a>],
) -> Result<&'a Value<'a>> {
    encoded(context, args, URL_ESCAPED)
}

/// `$decodeUrlComponent(str)`: "Decodes a Uniform Resource Locator (URL) component
/// previously created by encodeUrlComponent."
fn decode_url_component<'a>(
    context: FunctionContext<'a, '_>,
    args: &[&'a Value<'a>],
) -> Result<&'a Value<'a>> {
    decoded(context, args, false)
}

/// `$decodeUrl(str)`: "Decodes a Uniform Resource Locator (URL) previously created by
/// encodeUrl."
///
/// A WHOLE URL, so an escape that spells one of the characters that delimit a URL's own
/// parts STAYS an escape: decoding it would change what the URL means rather than what
/// it reads as.
fn decode_url<'a>(
    context: FunctionContext<'a, '_>,
    args: &[&'a Value<'a>],
) -> Result<&'a Value<'a>> {
    decoded(context, args, true)
}

/// The text of the first argument percent-encoded, escaping every character of
/// `escaped`.
fn encoded<'a>(
    context: FunctionContext<'a, '_>,
    args: &[&'a Value<'a>],
    escaped: &'static AsciiSet,
) -> Result<&'a Value<'a>> {
    let Some(text) = text_argument(&context, args)? else {
        return Ok(Value::undefined());
    };
    let encoded = ::percent_encoding::utf8_percent_encode(&text, escaped).to_string();
    Ok(Value::string(context.arena, &encoded))
}

/// The text of the first argument with its percent escapes resolved.
///
/// `whole_url` keeps the escapes that spell a URL's own delimiters, which is the one
/// difference between decoding a whole URL and decoding one component of it.
///
/// An escape the text does not complete, and bytes the escapes spell that are no UTF-8
/// text, are a text that was not produced by the encoding this reverses: the argument is
/// refused rather than decoded to something else.
fn decoded<'a>(
    context: FunctionContext<'a, '_>,
    args: &[&'a Value<'a>],
    whole_url: bool,
) -> Result<&'a Value<'a>> {
    let Some(text) = text_argument(&context, args)? else {
        return Ok(Value::undefined());
    };
    let refused = || {
        Error::T0410ArgumentNotValid(
            context.char_index,
            FIRST_ARGUMENT,
            context.name.to_string(),
        )
    };
    let bytes = text.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0_usize;
    while at < bytes.len() {
        if bytes[at] != b'%' {
            decoded.push(bytes[at]);
            at += 1;
            continue;
        }
        let escape = bytes.get(at + 1..at + 3).ok_or_else(refused)?;
        // Two HEX DIGITS and nothing else: the signs and underscores a numeric parse
        // would otherwise accept are not an escape the encoding ever wrote.
        if !escape.iter().all(u8::is_ascii_hexdigit) {
            return Err(refused());
        }
        let digits = ::std::str::from_utf8(escape).map_err(|_| refused())?;
        let byte = u8::from_str_radix(digits, 16).map_err(|_| refused())?;
        if whole_url && URL_DELIMITERS.contains(&byte) {
            decoded.extend_from_slice(&bytes[at..at + 3]);
        } else {
            decoded.push(byte);
        }
        at += 3;
    }
    let decoded = String::from_utf8(decoded).map_err(|_| refused())?;
    Ok(Value::string(context.arena, &decoded))
}

/// The TEXT of the first argument, or `None` for an argument that is not there.
///
/// Every function of the library that takes a string takes a string: an argument of
/// another type is one the call does not match, which is what the refusal says.
fn text_argument<'a>(
    context: &FunctionContext<'a, '_>,
    args: &[&'a Value<'a>],
) -> Result<Option<String>> {
    let value = argument(args, 0);
    if value.is_undefined() {
        return Ok(None);
    }
    if !value.is_string() {
        return Err(Error::T0410ArgumentNotValid(
            context.char_index,
            FIRST_ARGUMENT,
            context.name.to_string(),
        ));
    }
    Ok(Some(value.as_str().to_string()))
}

/// The argument at `index`, or the language's `undefined` where the call made none —
/// which is the value every function of the library answers `undefined` for.
fn argument<'a>(args: &[&'a Value<'a>], index: usize) -> &'a Value<'a> {
    args.get(index).copied().unwrap_or_else(Value::undefined)
}

/// The members of `value` as a sequence: the members themselves for an array, and the
/// value alone for anything else — the reading the language gives a function whose
/// argument is declared an array and handed a single value.
fn sequence<'a>(value: &'a Value<'a>) -> Box<dyn Iterator<Item = &'a Value<'a>> + 'a> {
    match value.is_array() {
        true => Box::new(value.members()),
        false => Box::new(::std::iter::once(value)),
    }
}

/// Whether `value` is TRUE as the language reads truth (`$boolean`): a boolean itself, a
/// number other than zero, a non-empty string, an array one of whose members is true, an
/// object that holds a pair — and false for everything else, `null`, a function and a
/// value that is not there included.
///
/// An array's members are read the same way, so the reading walks them — ITERATIVELY,
/// over an explicit queue, charging each value to `left` — rather than recursing: a
/// caller's value may nest as deeply as the arena admits and may alias one member into
/// many places, neither of which this thread's stack bounds.
fn truthy<'a>(value: &'a Value<'a>, left: &mut usize) -> Result<bool> {
    let mut pending: ::std::collections::VecDeque<&'a Value<'a>> =
        ::std::collections::VecDeque::new();
    pending.push_back(value);
    while let Some(value) = pending.pop_front() {
        walked(left)?;
        if value.is_bool() {
            if value.as_bool() {
                return Ok(true);
            }
        } else if value.is_number() {
            if value.as_f64() != 0.0 {
                return Ok(true);
            }
        } else if value.is_string() {
            if !value.as_str().is_empty() {
                return Ok(true);
            }
        } else if value.is_object() {
            if value.entries().next().is_some() {
                return Ok(true);
            }
        } else if value.is_array() {
            // An array is true when one of its MEMBERS is, each read as this same
            // reading reads it, so the members join the walk.
            for member in value.members() {
                walked(left)?;
                pending.push_back(member);
            }
        }
    }
    Ok(false)
}

/// Charge ONE value to a walk's budget, or report the walk as asking for more than one
/// of these functions visits.
///
/// The refusal is the engine's own structural-bound failure, the one it raises for an
/// evaluation that asked for more structure than it will follow, so a caller reads one
/// answer for a value too large to walk however the evaluation reached that size.
fn walked(left: &mut usize) -> Result<()> {
    match left.checked_sub(1) {
        Some(remaining) => {
            *left = remaining;
            Ok(())
        }
        None => Err(Error::U1001StackOverflow),
    }
}

/// The most values ONE walk below visits.
///
/// EMULATOR bound, and the reason every walk here carries one: the engine's values form
/// a DAG, so a value's size in the arena is no bound on the number of values a walk over
/// it visits — an expression that aliases one binding twice per level costs two pointers
/// per level and doubles that number — and a native function is one interpreter step,
/// which the engine's clock cannot interrupt and whose memory the arena clamp does not
/// govern when the walk allocates nothing.
///
/// Stated as the BYTES of the largest document this seam evaluates over
/// ([`super::MAX_INPUT_BYTES`]): a document of that many bytes spells fewer values than
/// that, so every value a caller's own document holds is inside this budget, while a
/// value whose aliasing multiplies the walk past it is refused instead of walked.
const MAX_WALKED_VALUES: usize = super::MAX_INPUT_BYTES;

/// Which argument a refusal names, counted as the language counts them: from one.
const FIRST_ARGUMENT: usize = 1;

/// The second, for the function a higher-order call is handed.
const SECOND_ARGUMENT: usize = 2;

/// The characters an encoded URL COMPONENT escapes: everything but the unreserved set —
/// the letters and digits, and the marks `- _ . ! ~ * ' ( )` — since a component carries
/// no delimiter of its own.
const COMPONENT_ESCAPED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// The characters an encoded WHOLE URL escapes: the component set, less the characters
/// that delimit a URL's own parts ([`URL_DELIMITERS`]), which a whole URL is written
/// with rather than escaping.
const URL_ESCAPED: &AsciiSet = &COMPONENT_ESCAPED
    .remove(b';')
    .remove(b'/')
    .remove(b'?')
    .remove(b':')
    .remove(b'@')
    .remove(b'&')
    .remove(b'=')
    .remove(b'+')
    .remove(b'$')
    .remove(b',')
    .remove(b'#');

/// The characters that delimit a URL's own parts, which a whole URL carries unescaped
/// and keeps escaped where they were escaped.
const URL_DELIMITERS: &[u8] = b";/?:@&=+$,#";

/// A seed for one shuffle, drawn from the host's own randomness: the hasher
/// [`::std::collections::hash_map::RandomState`] builds is keyed per process, and the
/// counter below moves the drawn value on every call, so two shuffles in one process
/// draw two orders.
fn seed() -> u64 {
    use ::std::hash::BuildHasher;

    static DRAWN: ::std::sync::atomic::AtomicU64 = ::std::sync::atomic::AtomicU64::new(0);
    let counted = DRAWN.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
    let state = ::std::collections::hash_map::RandomState::new();
    next(state.hash_one(counted))
}

/// The next value of the shuffle's own generator: one xorshift64* step, which is a
/// permutation of the 64-bit words, so a drawn sequence never repeats a state.
fn next(state: u64) -> u64 {
    let mut state = state | 1;
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

#[cfg(test)]
mod tests {
    use ::serde_json::{json, Value};

    use super::super::{evaluate, Expression, Refused};

    /// The ENGINE's own value type, named apart from the JSON one the rows are written
    /// with: the walk tests below build a value in an arena directly.
    use ::jsonata_rs::Value as EngineValue;

    /// One expression, evaluated over `input` with the ceilings this crate's callers
    /// hand in — through the SEAM, so what is asserted is what a service's own
    /// expression computes.
    fn over(text: &str, input: &Value) -> Result<Option<Value>, Refused> {
        evaluate(&Expression {
            text,
            input,
            variables: &[],
            allowed_ms: 250,
            allowance: 4096,
        })
        .map(|produced| produced.map(|(value, _)| value))
    }

    /// RULE: a function the language's library defines produces that function's value,
    /// whether or not this engine binds it itself.
    ///
    /// The rows span the functions this module supplies and the shapes each of them is
    /// defined over: the value's type, the members of an array, the pairs of an object,
    /// the predicate a higher-order call applies, and the character sets a URL's
    /// escaping is read over. Every expected value is the one the language's own
    /// documentation of that function gives.
    #[test]
    fn a_function_of_the_library_produces_its_value() {
        // (case, the expression, the value it produces — `None` for the language's
        //  `undefined`)
        let rows: [(&str, &str, Option<Value>); 32] = [
            // $type: one row per name it answers.
            ("$type of a string", "$type('a')", Some(json!("string"))),
            ("$type of a number", "$type(42)", Some(json!("number"))),
            ("$type of a boolean", "$type(true)", Some(json!("boolean"))),
            ("$type of null", "$type(null)", Some(json!("null"))),
            ("$type of an array", "$type([1, 2])", Some(json!("array"))),
            (
                "$type of an object",
                "$type({'a': 1})",
                Some(json!("object")),
            ),
            (
                "$type of a function",
                "$type($uppercase)",
                Some(json!("function")),
            ),
            ("$type of nothing", "$type(x.y)", None),
            (
                "$type beside the value, as a transformer composes it",
                "$type(Data) & ':' & Data",
                Some(json!("string:eyJhIjoxfQ==")),
            ),
            // $average over the member counts and the one refusal it is defined with.
            ("$average of several numbers", "$average([1, 2, 3])", Some(json!(2))),
            ("$average of two", "$average([1, 2])", Some(json!(1.5))),
            ("$average of one value", "$average(3)", Some(json!(3))),
            ("$average of negative numbers", "$average([-2, 2])", Some(json!(0))),
            ("$average of nothing", "$average(x.y)", None),
            ("$average of no members", "$average([])", None),
            // $spread over each shape it is defined for.
            (
                "$spread of an object of two pairs",
                "$sort($spread({'a': 1, 'b': 2}).$string($))",
                Some(json!([r#"{"a":1}"#, r#"{"b":2}"#])),
            ),
            (
                "$spread of an object of one pair, a sequence of one",
                "$spread({'a': 1})",
                Some(json!({ "a": 1 })),
            ),
            (
                "$spread of an array of objects",
                "$sort($spread([{'a': 1}, {'b': 2, 'c': 3}]).$string($))",
                Some(json!([r#"{"a":1}"#, r#"{"b":2}"#, r#"{"c":3}"#])),
            ),
            ("$spread of a string", "$spread('abc')", Some(json!("abc"))),
            ("$spread of a number", "$spread(7)", Some(json!(7))),
            // $sift over the arities its predicate may take.
            (
                "$sift on the value",
                "$sift({'a': 1, 'b': 2}, function($v) { $v > 1 })",
                Some(json!({ "b": 2 })),
            ),
            (
                "$sift on the key",
                "$sift({'a': 1, 'b': 2}, function($v, $k) { $k = 'a' })",
                Some(json!({ "a": 1 })),
            ),
            (
                "$sift on the object itself",
                "$sift({'a': 1, 'b': 2}, function($v, $k, $o) { $count($keys($o)) = 2 })",
                Some(json!({ "a": 1, "b": 2 })),
            ),
            (
                "$sift whose predicate keeps nothing",
                "$sift({'a': 1}, function($v) { $v > 5 })",
                None,
            ),
            // The four URL functions, over the sets each of them is defined by. The
            // encoded forms are the examples the language's own documentation gives.
            (
                "$encodeUrlComponent escapes a component's delimiters",
                "$encodeUrlComponent('?x=test')",
                Some(json!("%3Fx%3Dtest")),
            ),
            (
                "$encodeUrlComponent keeps the unreserved marks",
                "$encodeUrlComponent(\"-_.!~*'()\")",
                Some(json!("-_.!~*'()")),
            ),
            (
                "$encodeUrl keeps a URL's own delimiters",
                "$encodeUrl('https://mozilla.org/?x=шеллы')",
                Some(json!("https://mozilla.org/?x=%D1%88%D0%B5%D0%BB%D0%BB%D1%8B")),
            ),
            (
                "$decodeUrlComponent resolves every escape",
                "$decodeUrlComponent('%3Fx%3Dtest')",
                Some(json!("?x=test")),
            ),
            (
                "$decodeUrl resolves the escapes that are not a URL's delimiters",
                "$decodeUrl('https://mozilla.org/?x=%D1%88%D0%B5%D0%BB%D0%BB%D1%8B')",
                Some(json!("https://mozilla.org/?x=шеллы")),
            ),
            (
                "$decodeUrl keeps an escaped delimiter escaped",
                "$decodeUrl('https://a.example/%2Fb%3Fc')",
                Some(json!("https://a.example/%2Fb%3Fc")),
            ),
            (
                "$decodeUrlComponent resolves an escaped delimiter",
                "$decodeUrlComponent('%2Fb%3Fc')",
                Some(json!("/b?c")),
            ),
            ("$encodeUrlComponent of nothing", "$encodeUrlComponent(x.y)", None),
        ];

        let input = json!({ "Data": "eyJhIjoxfQ==" });
        for (case, text, expected) in rows {
            let produced = over(text, &input);
            assert_eq!(
                produced.as_ref().ok(),
                Some(&expected),
                "case {case:?}: expected {expected:?} from {text:?}, got {produced:?}"
            );
        }
    }

    /// RULE: a shuffle produces the SAME members in some order, which is what the
    /// language defines it as — asserted over the members rather than over one order,
    /// since the order is drawn.
    #[test]
    fn a_shuffle_produces_the_same_members_in_some_order() {
        let input = json!({});
        for case in ["[1, 2, 3, 4, 5]", "['a', 'b']", "[1]", "[]"] {
            let produced = over(&format!("$shuffle({case})"), &input);
            let mut members: Vec<Value> = match produced.as_ref().map(Option::as_ref) {
                Ok(Some(Value::Array(members))) => members.clone(),
                // A sequence of one member reads as that member, and an empty one as
                // nothing at all.
                Ok(Some(other)) => vec![other.clone()],
                Ok(None) => Vec::new(),
                Err(why) => panic!("case {case:?}: a shuffle must produce a value, got {why:?}"),
            };
            let mut expected: Vec<Value> = match over(case, &input) {
                Ok(Some(Value::Array(members))) => members,
                Ok(Some(other)) => vec![other],
                _ => Vec::new(),
            };
            members.sort_by_key(ToString::to_string);
            expected.sort_by_key(ToString::to_string);
            assert_eq!(
                members, expected,
                "case {case:?}: expected the members {expected:?} in some order, got {members:?}"
            );
        }
    }

    /// RULE: an argument of a type the function is not defined over is REFUSED — the
    /// expression throws, as it does for every other argument the language does not
    /// admit — rather than producing a value computed from something else.
    #[test]
    fn an_argument_the_function_is_not_defined_over_is_refused() {
        // (case, the expression)
        let rows: [(&str, &str); 9] = [
            ("$average over a member that is no number", "$average([1, 'a'])"),
            ("$sift whose second argument is no function", "$sift({'a': 1}, 'a')"),
            // `$sift` is declared over an OBJECT, so a first argument of any other
            // type is one the call does not match.
            (
                "$sift over a string",
                "$sift('abc', function($v) { true })",
            ),
            ("$sift over a number", "$sift(7, function($v) { true })"),
            (
                "$sift over an array",
                "$sift([1, 2], function($v) { true })",
            ),
            (
                "$decodeUrlComponent over an escape the text does not complete",
                "$decodeUrlComponent('%E0%A4%A')",
            ),
            (
                "$decodeUrlComponent over escapes that spell no text",
                "$decodeUrlComponent('%FF%FE')",
            ),
            (
                "$decodeUrlComponent over an escape that is no pair of hex digits",
                "$decodeUrlComponent('%+5')",
            ),
            (
                "$decodeUrl over an escape the text does not complete",
                "$decodeUrl('https://a.example/%E')",
            ),
        ];
        let input = json!({});
        for (case, text) in rows {
            let produced = over(text, &input);
            assert!(
                matches!(produced, Err(Refused::Threw(_))),
                "case {case:?}: expected {text:?} to throw, got {produced:?}"
            );
        }
    }

    /// RULE: each walk over a value a caller handed in is bounded AHEAD of the step it
    /// runs in, so a value whose number of nodes no ceiling of the evaluation bounds —
    /// one ALIASED into many places, which costs the arena a pointer per place and
    /// doubles what a walk visits per level — is REFUSED rather than walked.
    ///
    /// The value is built directly in an arena, aliasing one child into both positions of
    /// each level, because that is the shape no ceiling of the evaluation can see: thirty
    /// levels are sixty pointers in the arena and 2^30 nodes to a walk. The rows differ
    /// along the walk driven: the one that spreads a value's members, and the one that
    /// reads the truth of a value ([`truthy`], which `$sift` applies to every answer its
    /// predicate gives).
    #[test]
    fn an_aliased_value_too_large_to_walk_is_refused() {
        /// A value of `2^levels` nodes built from `levels` arrays, each holding the SAME
        /// child twice. Every leaf is false, so a walk that reads truth visits all of
        /// them.
        fn aliased<'a>(arena: &'a ::bumpalo::Bump, levels: usize) -> &'a EngineValue<'a> {
            let mut value: &'a EngineValue<'a> = EngineValue::number(arena, 0.0);
            for _ in 0..levels {
                let level = EngineValue::array(arena, ::jsonata_rs::ArrayFlags::empty());
                level.push(value);
                level.push(value);
                value = level;
            }
            value
        }

        let arena = ::bumpalo::Bump::new();
        let value = aliased(&arena, 30);
        let spread = super::spread_into(&arena, value);
        assert!(
            spread.is_err(),
            "case \"the walk that spreads a value's members\": expected the walk to be \
             refused, got a value"
        );
        let mut left = super::MAX_WALKED_VALUES;
        let read = super::truthy(value, &mut left);
        assert!(
            read.is_err(),
            "case \"the walk that reads the truth of a value\": expected the walk to be \
             refused, got {read:?}"
        );

        // The counter-cases, so the refusals above are about the SIZE of the walk and not
        // about an aliased value as such: the same construction inside the budget is
        // walked, and answers. Ten levels is 1,024 leaves.
        let small = aliased(&arena, 10);
        let spread = super::spread_into(&arena, small).expect("a spread inside the budget");
        assert_eq!(
            spread.members().count(),
            1024,
            "a spread inside the budget must produce every leaf, got {}",
            spread.serialize(false).len()
        );
        let mut left = super::MAX_WALKED_VALUES;
        assert_eq!(
            super::truthy(small, &mut left),
            Ok(false),
            "a reading inside the budget must answer: every leaf of this value is false"
        );
    }
}

