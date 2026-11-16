// SPDX-License-Identifier: CC0-1.0

//! The formula language of IoT SiteWise, as the expression engine is given it.
//!
//! The expression is evaluated by a maintained arithmetic/boolean expression
//! engine (the `evalexpr` crate), not by reading the formula's text: a variable is
//! a bound value and a function is applied to what it is handed, so a formula this
//! service never saw before computes as readily as one it has. Two things the
//! engine has to be told, because the formula language fixes them and a
//! general-purpose engine cannot know them:
//!
//! * the language has ONE numeric type — a computed property "can only have a data
//!   type of `DOUBLE`" — so an integer literal of a formula is a double literal
//!   ([`as_double_literals`]) and `9/5` is the ratio, not an integer division;
//! * its functions are the ones [`FORMULA_FUNCTIONS`] names and NO OTHERS
//!   ([`formula_context`]), and its aggregations reduce the data points of a window
//!   rather than one number.
//!
//! The names the language PUBLISHES are a wider set ([`PUBLISHED_FUNCTIONS`]),
//! and the two answer two different questions: a WRITE asks whether the language
//! has a function by that name ([`is_formula_function`]) and refuses a formula
//! applying one it does not publish, while a READ computes only what this
//! evaluator reduces.
//!
//! Nothing here reaches anything of the host: one bounded expression is reduced to
//! one number over the values bound into a context, with no statement, no loop and
//! no binding to a file, a process, a socket, the environment or the clock. A
//! formula the engine cannot evaluate — one that does not parse, one naming a
//! function this evaluator does not implement, one whose variables are bound to
//! values it cannot combine, or one whose result is no finite number — yields NO
//! data point, rather than an invented one.

use super::*;

// The traits the engine's context carries the three bindings this evaluator makes
// on it: `set_value` for a formula's variables, `set_function` for the formula
// language's own functions, and `set_builtin_functions_disabled` for the engine's
// own, which stay off.
use ::evalexpr::{Context, ContextWithMutableFunctions, ContextWithMutableVariables};

/// `expression` compiled by the expression engine, or `None` when it does not
/// parse as one at all.
///
/// Compiled ONCE per formula property and then evaluated per data point or per
/// window, so a long series costs one parse.
///
/// # Bounds
///
/// The text handed to the engine is at most [`MAX_EXPRESSION_CHARACTERS`] — the
/// length the modelled `Expression` fixes, which the request walk has already
/// enforced and which [`as_double_literals`] holds to again — so the parse is
/// bounded by the model rather than by what a caller sends.
pub(super) fn compile(expression: &str) -> Option<::evalexpr::Node> {
    ::evalexpr::build_operator_tree(&as_double_literals(expression)).ok()
}

/// What `formula` evaluates to under `context`, as the one finite number a
/// computed data point can carry — `None` when it evaluates to no number at all,
/// or to one no `doubleValue` can carry.
pub(super) fn evaluate(
    formula: &::evalexpr::Node,
    context: &::evalexpr::HashMapContext,
) -> Option<f64> {
    let number = match formula.eval_with_context(context).ok()? {
        ::evalexpr::Value::Float(number) => number,
        ::evalexpr::Value::Int(number) => number as f64,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// `expression` with every integer literal spelled as a double literal, because
/// the formula language has one numeric type (see the module docs).
///
/// Reads the expression the way the formula language lays it out, exactly as
/// [`formula_identifiers`] does: a quoted literal is text, an identifier runs
/// letters, digits and `_` (so the `1` of `t1` is part of the name), and a number
/// runs its digits, its fraction and its exponent. TOTAL over every string a caller
/// can send, in a single pass with no recursion, over at most
/// [`MAX_EXPRESSION_CHARACTERS`] characters.
fn as_double_literals(expression: &str) -> String {
    let mut spelled = String::with_capacity(expression.len());
    let characters: Vec<char> = expression.chars().take(MAX_EXPRESSION_CHARACTERS).collect();
    let mut at = 0;
    while at < characters.len() {
        let character = characters[at];
        if character == '\'' || character == '"' {
            // A quoted literal: its contents are text, so nothing inside it is a
            // number. An unterminated quote runs to the end of the expression.
            spelled.push(character);
            at += 1;
            while at < characters.len() && characters[at] != character {
                spelled.push(characters[at]);
                at += 1;
            }
            if at < characters.len() {
                spelled.push(characters[at]);
                at += 1;
            }
            continue;
        }
        if character.is_alphabetic() || character == '_' {
            // An identifier, digits and all: a name is not a number.
            while at < characters.len()
                && (characters[at].is_alphanumeric() || characters[at] == '_')
            {
                spelled.push(characters[at]);
                at += 1;
            }
            continue;
        }
        if character.is_ascii_digit() {
            let start = at;
            let mut whole = true;
            while at < characters.len()
                && (characters[at].is_ascii_alphanumeric() || characters[at] == '.')
            {
                // A fraction or an exponent already spells a double; only a run of
                // digits alone is the integer literal that needs one.
                if !characters[at].is_ascii_digit() {
                    whole = false;
                }
                spelled.push(characters[at]);
                at += 1;
            }
            // A sign belongs to the exponent that introduces it, so `1e-3` stays
            // one literal.
            if !whole
                && matches!(characters.get(at), Some('+' | '-'))
                && matches!(
                    characters.get(start..at).and_then(<[char]>::last),
                    Some('e' | 'E')
                )
            {
                spelled.push(characters[at]);
                at += 1;
            }
            if whole {
                spelled.push_str(".0");
            }
            continue;
        }
        spelled.push(character);
        at += 1;
    }
    spelled
}

/// Bind `name` to the one number `number` in `context`, answering whether the
/// binding took.
pub(super) fn bind_number(
    context: &mut ::evalexpr::HashMapContext,
    name: &str,
    number: f64,
) -> bool {
    context
        .set_value(name.to_string(), ::evalexpr::Value::Float(number))
        .is_ok()
}

/// Bind `name` to `numbers` — the data points of one window, which the formula's
/// aggregation functions reduce.
pub(super) fn bind_numbers(
    context: &mut ::evalexpr::HashMapContext,
    name: &str,
    numbers: &[f64],
) -> bool {
    let window = ::evalexpr::Value::Tuple(
        numbers
            .iter()
            .copied()
            .map(::evalexpr::Value::Float)
            .collect(),
    );
    context.set_value(name.to_string(), window).is_ok()
}

/// Bind `name` to NO value: the formula reads a property that carries no number,
/// so nothing it is part of evaluates to a number either.
pub(super) fn bind_nothing(context: &mut ::evalexpr::HashMapContext, name: &str) -> bool {
    context
        .set_value(name.to_string(), ::evalexpr::Value::Empty)
        .is_ok()
}

/// Whether `name` is a function of the formula language: one of the names it
/// PUBLISHES ([`PUBLISHED_FUNCTIONS`]), whether or not this evaluator reduces it.
///
/// The question a WRITE asks of a formula is the language's — does the language
/// have a function by this name — and not this evaluator's: a published function
/// this evaluator does not reduce computes no data point (see the module docs),
/// while a name the language does not publish is no function at all and the
/// definition naming it is refused.
pub(in crate::state) fn is_formula_function(name: &str) -> bool {
    PUBLISHED_FUNCTIONS.contains(&name)
}

/// The function names the formula language publishes.
///
/// Source: the IoT SiteWise User Guide's formula-expression reference ("Use
/// formula expressions"), group by group as that page sections them — marked
/// below, so each entry can be re-read against the section it came from:
/// aggregation, temporal, comparison and logical, conditional, the elementary
/// arithmetic a transform maps a data point with, text, and conversion. The
/// Smithy model carries no list of them, so this is REFERENCE DATA transcribed
/// from that page rather than a shape of the model.
///
/// The set is deliberately WIDE, because it is load-bearing in the refusing
/// direction: an applied name it does not hold is refused, so a name the guide
/// publishes and this transcription missed would refuse a definition the service
/// stores. Every group is therefore taken whole, including the entries this
/// evaluator does not reduce, and the set has to be re-read against that page as
/// AWS adds to the language.
///
/// It is a SUPERSET of [`FORMULA_FUNCTIONS`], the names this evaluator reduces
/// (pinned by the test below): what a write accepts is what the language has, and
/// what a read computes is what the evaluator implements — two different
/// questions, and conflating them would either refuse formulas the service
/// computes or store formulas naming nothing.
const PUBLISHED_FUNCTIONS: &[&str] = &[
    // Aggregation: the data points of one window reduced to one number.
    "avg",
    "count",
    "max",
    "min",
    "stdev",
    "sum",
    // Temporal: what a window or a stream held, and for how long.
    "first",
    "last",
    "latest",
    "pretrigger",
    "statetime",
    "timeweightedavg",
    "timeweightedavg_forward_fill",
    "timeweightedavg_linear_interpolation",
    // Comparison and logical, the function spellings of the operators.
    "and",
    "eq",
    "gt",
    "gte",
    "lt",
    "lte",
    "ne",
    "not",
    "or",
    // Conditional, and the tests a condition is written over.
    "coalesce",
    "if",
    "isnan",
    // Arithmetic over one number, or two.
    "abs",
    "acos",
    "asin",
    "atan",
    "atan2",
    "cbrt",
    "ceil",
    "cos",
    "cosh",
    "exp",
    "floor",
    "hypot",
    "ln",
    "log",
    "log10",
    "log2",
    "mod",
    "pow",
    "round",
    "signum",
    "sin",
    "sinh",
    "sqrt",
    "tan",
    "tanh",
    "trunc",
    // Text, and the JSON path reading over a text value.
    "concat",
    "contains",
    "endswith",
    "index",
    "jp",
    "length",
    "lower",
    "ltrim",
    "rtrim",
    "startswith",
    "substring",
    "trim",
    "upper",
    // Conversion between a number and its text.
    "num2str",
    "str2num",
];

/// The functions a formula may apply AND THIS EVALUATOR REDUCES, each with how it
/// reduces the numbers it is handed.
///
/// Source: the formula-expression functions the IoT SiteWise User Guide publishes
/// for transforms and metrics — the aggregation functions a metric reduces a
/// window's data points with (`avg`, `count`, `max`, `min`, `stdev`, `sum`,
/// `first`, `last`) and the arithmetic functions a transform maps a data point with
/// (`abs`, `ceil`, `exp`, `floor`, `ln`, `log10`, `pow`, `round`, `sqrt`). The
/// Smithy model carries no list of them. This catalogue bounds what this
/// evaluator COMPUTES; what a write accepts is the wider published surface
/// ([`PUBLISHED_FUNCTIONS`]), so a published function this evaluator does not
/// reduce yields no data point rather than a refused write.
///
/// Each aggregation reduces EVERY number it is handed, so one implementation serves
/// both halves of the language: `min(a, b)` of a transform's two numbers and
/// `min(t)` of the data points a metric's window holds are the same question. The
/// arithmetic functions take the one number — `pow` the two — their own arithmetic
/// is defined over.
const FORMULA_FUNCTIONS: [(&str, Reduce); 17] = [
    ("avg", Reduce::Average),
    ("count", Reduce::Count),
    ("max", Reduce::Maximum),
    ("min", Reduce::Minimum),
    ("stdev", Reduce::StandardDeviation),
    ("sum", Reduce::Sum),
    ("first", Reduce::First),
    ("last", Reduce::Last),
    ("abs", Reduce::Absolute),
    ("ceil", Reduce::Ceiling),
    ("exp", Reduce::Exponential),
    ("floor", Reduce::Floor),
    ("ln", Reduce::NaturalLogarithm),
    ("log10", Reduce::Logarithm10),
    ("pow", Reduce::Power),
    ("round", Reduce::Round),
    ("sqrt", Reduce::SquareRoot),
];

/// What one formula function reduces the numbers it is handed to.
#[derive(Debug, Clone, Copy)]
enum Reduce {
    Average,
    Count,
    Maximum,
    Minimum,
    StandardDeviation,
    Sum,
    First,
    Last,
    Absolute,
    Ceiling,
    Exponential,
    Floor,
    NaturalLogarithm,
    Logarithm10,
    Power,
    Round,
    SquareRoot,
}

impl Reduce {
    /// `numbers` reduced, or `None` when they are not what this function reduces —
    /// an empty window for an aggregation, or a count of numbers a function of one
    /// or of two cannot take.
    fn of(self, numbers: &[f64]) -> Option<f64> {
        let count = numbers.len() as f64;
        let sum: f64 = numbers.iter().sum();
        let one = || match numbers {
            [only] => Some(*only),
            _ => None,
        };
        match self {
            // `count` is the one aggregation an empty window answers: a window
            // holding no data point holds none of them.
            Self::Count => Some(count),
            Self::Sum => (!numbers.is_empty()).then_some(sum),
            Self::Average => (!numbers.is_empty()).then(|| sum / count),
            Self::Maximum => numbers.iter().copied().reduce(f64::max),
            Self::Minimum => numbers.iter().copied().reduce(f64::min),
            // The POPULATION standard deviation, as this service already reduces a
            // window's data points to a `STANDARD_DEVIATION` aggregate: a window's
            // data points are all of that window's data, so nothing is being
            // sampled from a wider set.
            Self::StandardDeviation => (!numbers.is_empty()).then(|| {
                let mean = sum / count;
                (numbers
                    .iter()
                    .map(|number| (number - mean) * (number - mean))
                    .sum::<f64>()
                    / count)
                    .sqrt()
            }),
            Self::First => numbers.first().copied(),
            Self::Last => numbers.last().copied(),
            Self::Absolute => one().map(f64::abs),
            Self::Ceiling => one().map(f64::ceil),
            Self::Exponential => one().map(f64::exp),
            Self::Floor => one().map(f64::floor),
            Self::NaturalLogarithm => one().map(f64::ln),
            Self::Logarithm10 => one().map(f64::log10),
            // The one function of TWO numbers: the base and the exponent it is
            // raised to, in that order.
            Self::Power => match numbers {
                [base, exponent] => Some(base.powf(*exponent)),
                _ => None,
            },
            Self::Round => one().map(f64::round),
            Self::SquareRoot => one().map(f64::sqrt),
        }
    }
}

/// A context carrying every function of [`FORMULA_FUNCTIONS`] AND NOTHING ELSE,
/// ready for the caller's formula variables to be bound into.
///
/// The engine's own builtin function table is turned OFF, so the only functions a
/// formula can reach are the ones this crate binds: what the catalogue does not
/// name is not callable, rather than falling through to whatever a general-purpose
/// engine happens to offer under that name. That is what makes
/// [`FORMULA_FUNCTIONS`] the whole reachable surface, and what makes "a formula
/// naming a function this evaluator does not implement computes nothing" true of
/// every name alike.
pub(super) fn formula_context() -> ::evalexpr::HashMapContext {
    let mut context = ::evalexpr::HashMapContext::new();
    // A context that cannot turn them off would leave that surface reachable, so
    // the formula is evaluated only in one that did.
    let disabled = context.set_builtin_functions_disabled(true).is_ok();
    debug_assert!(
        disabled,
        "the formula context must bind the only functions a formula can reach"
    );
    for (name, reduce) in FORMULA_FUNCTIONS {
        // Nothing to report on a name the context already holds: the catalogue
        // names each function once.
        let _ = context.set_function(
            name.to_string(),
            ::evalexpr::Function::new(move |argument| {
                let mut numbers = Vec::new();
                collect_numbers(argument, &mut numbers);
                Ok(match reduce.of(&numbers) {
                    Some(number) => ::evalexpr::Value::Float(number),
                    // No number this function can answer with — an aggregation
                    // over an empty window. The data point it would have fed is
                    // then not computed at all.
                    None => ::evalexpr::Value::Empty,
                })
            }),
        );
    }
    context
}

/// Every number `value` carries, in order: the numbers of a tuple — what a window's
/// data points are bound as, and what a call of several arguments hands over — or
/// the one number a scalar is.
///
/// Iterative, so the nesting of a caller's argument tuples is never a depth of this
/// walk.
fn collect_numbers(value: &::evalexpr::Value, into: &mut Vec<f64>) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            ::evalexpr::Value::Float(number) => into.push(*number),
            ::evalexpr::Value::Int(number) => into.push(*number as f64),
            ::evalexpr::Value::Tuple(items) => {
                // Pushed in reverse so the tuple is read left to right, which is
                // what `first` and `last` report over a window.
                pending.extend(items.iter().rev());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every function this evaluator reduces is a function the language
    /// publishes, so nothing the evaluator implements is refused at the write
    /// boundary and no accepted formula names a function a read cannot reach.
    #[test]
    fn every_reduced_function_is_a_published_one() {
        for (name, _) in FORMULA_FUNCTIONS {
            assert!(
                is_formula_function(name),
                "case {name:?}: a function this evaluator reduces must be published"
            );
        }
    }

    /// RULE: a name the language does not publish is no function of it. Driven
    /// over names outside the catalogue, a near-miss spelling of one in it
    /// included. Case is NOT among the rows: nothing in the model or the
    /// recordings states how the language reads the case of a function name.
    #[test]
    fn a_name_the_language_does_not_publish_is_no_function() {
        for name in ["average", "mystery", "avg2", "", "sumx"] {
            assert!(
                !is_formula_function(name),
                "case {name:?}: must not be a function of the language"
            );
        }
    }
}
