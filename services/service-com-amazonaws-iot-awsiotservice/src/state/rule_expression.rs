// SPDX-License-Identifier: CC0-1.0

//! What ONE expression of a topic rule's SQL statement has as its value for one published
//! message: the walk of the AST [`super::rule_sql`] hands over.
//!
//! It is a purpose-built walk of that AST, not an engine: every arm reads the MESSAGE
//! DOCUMENT, or the topic the message was published to, and nothing else. There is no
//! user-defined-function dispatch and the ONE function of the AWS IoT SQL library it computes
//! is `topic()`, which reads that same topic ([`topic_function`]), so no expression of a
//! caller's statement can reach a socket, a subprocess, the filesystem, the
//! process environment or any other host primitive, and there is nothing for it to escape
//! into. It is bounded — every evaluation carries its depth and stops at
//! [`MAX_EXPRESSION_DEPTH`], so a deeply nested condition cannot recurse without limit
//! (`sqlparser` bounds its own recursion while PARSING) — and total: a construct it does not
//! implement answers [`Evaluated::Undecided`], never a panic and never a guess, which
//! [`super::rule_sql`] turns into "this rule performs no action for this message".

use ::serde_json::Value;
use ::sqlparser::ast::{
    BinaryOperator, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, UnaryOperator,
};

/// How deep an expression is evaluated. The statement is a caller's text, so its tree is
/// caller-controlled; the bound is counted on the TREE rather than on the brackets, because a
/// chain of `AND`s nests the tree without nesting one bracket.
const MAX_EXPRESSION_DEPTH: usize = 32;

/// The one function of the AWS IoT SQL library this walk computes: "topic(Decimal)" returns
/// "the topic to which the message was sent", whole when it is called with no argument and one
/// SEGMENT of that topic when it is called with the segment's 1-based number.
const TOPIC_FUNCTION: &str = "topic";

/// The separator between two levels of an MQTT topic name, which is what [`TOPIC_FUNCTION`]
/// numbers the segments of.
const TOPIC_LEVEL_SEPARATOR: char = '/';

/// The ONE published message an expression is evaluated over.
///
/// It carries the message's document AND the topic it was published to, because a statement's
/// value for a message is a function of both: an attribute path reads the document, while
/// [`TOPIC_FUNCTION`] reads the topic. Nothing else about the publish is reachable from an
/// expression.
#[derive(Debug, Clone, Copy)]
pub struct Message<'a> {
    /// The topic the message was published to.
    pub topic: &'a str,
    /// The message's payload as the document it is.
    pub document: &'a Value,
}

/// What one expression of a statement has as its value for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evaluated {
    /// The value it has.
    Known(Value),
    /// The message carries no such attribute, which is SQL's UNKNOWN.
    Absent,
    /// An expression this engine does not evaluate — a function of the AWS IoT SQL library
    /// other than `topic()`, an operator it does not implement, or a tree deeper than
    /// [`MAX_EXPRESSION_DEPTH`].
    Undecided,
}

/// The value `expression` has for `message`; `depth` is how deep this evaluation
/// already is, and past [`MAX_EXPRESSION_DEPTH`] it stops rather than recursing further.
pub fn evaluate(expression: &Expr, message: Message<'_>, depth: usize) -> Evaluated {
    if depth > MAX_EXPRESSION_DEPTH {
        return Evaluated::Undecided;
    }
    let document = message.document;
    let depth = depth + 1;
    match expression {
        Expr::Nested(inner) => evaluate(inner, message, depth),
        Expr::Identifier(identifier) => {
            attribute(document, ::std::slice::from_ref(&identifier.value))
        }
        Expr::CompoundIdentifier(identifiers) => {
            let path: Vec<String> = identifiers
                .iter()
                .map(|identifier| identifier.value.clone())
                .collect();
            attribute(document, &path)
        }
        Expr::Value(value) => literal(&value.value),
        Expr::Function(function) => topic_function(function, message, depth),
        Expr::UnaryOp { op, expr } => {
            let inner = evaluate(expr, message, depth);
            match op {
                UnaryOperator::Plus => inner,
                UnaryOperator::Minus => match inner {
                    Evaluated::Known(value) => value
                        .as_f64()
                        .and_then(|number| ::serde_json::Number::from_f64(-number))
                        .map_or(Evaluated::Undecided, |negated| {
                            Evaluated::Known(Value::Number(negated))
                        }),
                    other => other,
                },
                UnaryOperator::Not => match inner {
                    Evaluated::Known(Value::Bool(flag)) => Evaluated::Known(Value::Bool(!flag)),
                    Evaluated::Known(Value::Null) | Evaluated::Absent => Evaluated::Absent,
                    _ => Evaluated::Undecided,
                },
                _ => Evaluated::Undecided,
            }
        }
        Expr::IsNull(inner) => Evaluated::Known(Value::Bool(matches!(
            evaluate(inner, message, depth),
            Evaluated::Absent | Evaluated::Known(Value::Null)
        ))),
        Expr::IsNotNull(inner) => Evaluated::Known(Value::Bool(!matches!(
            evaluate(inner, message, depth),
            Evaluated::Absent | Evaluated::Known(Value::Null)
        ))),
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let subject = evaluate(expr, message, depth);
            let mut held = Evaluated::Known(Value::Bool(false));
            for item in list {
                let candidate = compare(
                    &subject,
                    &evaluate(item, message, depth),
                    &BinaryOperator::Eq,
                );
                held = disjunction(&held, &candidate);
            }
            negate_if(held, *negated)
        }
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => {
            let subject = evaluate(expr, message, depth);
            let above = compare(
                &subject,
                &evaluate(low, message, depth),
                &BinaryOperator::GtEq,
            );
            let below = compare(
                &subject,
                &evaluate(high, message, depth),
                &BinaryOperator::LtEq,
            );
            negate_if(conjunction(&above, &below), *negated)
        }
        Expr::BinaryOp { left, op, right } => {
            let left = evaluate(left, message, depth);
            let right = evaluate(right, message, depth);
            match op {
                BinaryOperator::And => conjunction(&left, &right),
                BinaryOperator::Or => disjunction(&left, &right),
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq => compare(&left, &right, op),
                BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Modulo => arithmetic(&left, &right, op),
                _ => Evaluated::Undecided,
            }
        }
        // Everything else — another function of the AWS IoT SQL library, a subquery, a CASE —
        // is an expression this engine does not compute. No arm above calls out of this module.
        _ => Evaluated::Undecided,
    }
}

/// The value a CALL in a statement has, which is [`TOPIC_FUNCTION`]'s and no other's.
///
/// "topic(Decimal)" is "the topic to which the message was sent": called with no argument it is
/// the whole topic name, and called with a number it is that 1-based SEGMENT of it, so
/// `topic(2)` of `sensors/room-1/temp` is `room-1`. A number outside the topic's segments names
/// no segment, which is SQL's unknown ([`Evaluated::Absent`]) rather than an invented value.
///
/// Every other call — every other function of that library, and anything spelled like a call —
/// is [`Evaluated::Undecided`]: this walk computes no function it does not implement, so a rule
/// whose statement uses one performs no action rather than acting on a guess.
fn topic_function(function: &Function, message: Message<'_>, depth: usize) -> Evaluated {
    if !function.name.to_string().eq_ignore_ascii_case(TOPIC_FUNCTION) {
        return Evaluated::Undecided;
    }
    let arguments: &[FunctionArg] = match &function.args {
        FunctionArguments::None => &[],
        FunctionArguments::List(list) if list.clauses.is_empty() => list.args.as_slice(),
        _ => return Evaluated::Undecided,
    };
    match arguments {
        [] => Evaluated::Known(Value::String(message.topic.to_string())),
        [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] => {
            let Evaluated::Known(value) = evaluate(argument, message, depth) else {
                return Evaluated::Undecided;
            };
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

/// The value the attribute `path` names in `document`: its members walked in order, so
/// `state.reported.temp` reads `temp` of `reported` of `state`. [`Evaluated::Absent`] when the
/// document carries no such attribute, which a path through a non-object answers too.
fn attribute(document: &Value, path: &[String]) -> Evaluated {
    let mut here = document;
    for segment in path {
        match here.get(segment) {
            Some(next) => here = next,
            None => return Evaluated::Absent,
        }
    }
    Evaluated::Known(here.clone())
}

/// The JSON value one parsed SQL literal stands for.
fn literal(value: &::sqlparser::ast::Value) -> Evaluated {
    use ::sqlparser::ast::Value as SqlValue;
    match value {
        SqlValue::Number(digits, _) => digits
            .parse::<::serde_json::Number>()
            .map_or(Evaluated::Undecided, |number| {
                Evaluated::Known(Value::Number(number))
            }),
        SqlValue::SingleQuotedString(text)
        | SqlValue::DoubleQuotedString(text)
        | SqlValue::EscapedStringLiteral(text)
        | SqlValue::UnicodeStringLiteral(text) => Evaluated::Known(Value::String(text.clone())),
        SqlValue::Boolean(flag) => Evaluated::Known(Value::Bool(*flag)),
        SqlValue::Null => Evaluated::Known(Value::Null),
        _ => Evaluated::Undecided,
    }
}

/// `left AND right` in SQL's three-valued logic: false wins over unknown, unknown over true.
fn conjunction(left: &Evaluated, right: &Evaluated) -> Evaluated {
    match (truth(left), truth(right)) {
        (Some(Some(false)), _) | (_, Some(Some(false))) => Evaluated::Known(Value::Bool(false)),
        (None, _) | (_, None) => Evaluated::Undecided,
        (Some(Some(true)), Some(Some(true))) => Evaluated::Known(Value::Bool(true)),
        _ => Evaluated::Absent,
    }
}

/// `left OR right` in SQL's three-valued logic: true wins over unknown, unknown over false.
fn disjunction(left: &Evaluated, right: &Evaluated) -> Evaluated {
    match (truth(left), truth(right)) {
        (Some(Some(true)), _) | (_, Some(Some(true))) => Evaluated::Known(Value::Bool(true)),
        (None, _) | (_, None) => Evaluated::Undecided,
        (Some(Some(false)), Some(Some(false))) => Evaluated::Known(Value::Bool(false)),
        _ => Evaluated::Absent,
    }
}

/// `NOT evaluated` when `negated`, and `evaluated` itself otherwise — how a negated `IN` and a
/// negated `BETWEEN` are answered from the un-negated reading.
fn negate_if(evaluated: Evaluated, negated: bool) -> Evaluated {
    if !negated {
        return evaluated;
    }
    match truth(&evaluated) {
        Some(Some(flag)) => Evaluated::Known(Value::Bool(!flag)),
        Some(None) => Evaluated::Absent,
        None => Evaluated::Undecided,
    }
}

/// The truth value `evaluated` carries: `None` for one this engine cannot decide, `Some(None)`
/// for SQL's unknown, `Some(Some(flag))` for a boolean.
fn truth(evaluated: &Evaluated) -> Option<Option<bool>> {
    match evaluated {
        Evaluated::Known(Value::Bool(flag)) => Some(Some(*flag)),
        Evaluated::Known(Value::Null) | Evaluated::Absent => Some(None),
        _ => None,
    }
}

/// `left <op> right` for one of SQL's comparison operators.
///
/// A comparison in which either side is unknown — an attribute the message does not carry, or
/// a NULL — is itself unknown ([`Evaluated::Absent`]), SQL's own reading. Two values of the
/// same JSON type compare by that type; a number and a string compare numerically when the
/// string spells a number, the implicit conversion the AWS IoT SQL reference gives between
/// them; anything else is unknown.
fn compare(left: &Evaluated, right: &Evaluated, op: &BinaryOperator) -> Evaluated {
    let (Evaluated::Known(left), Evaluated::Known(right)) = (left, right) else {
        return undecided_or_unknown(left, right);
    };
    if left.is_null() || right.is_null() {
        return Evaluated::Absent;
    }
    let Some(ordering) = ordering(left, right) else {
        return Evaluated::Absent;
    };
    let holds = match op {
        BinaryOperator::Eq => ordering == ::std::cmp::Ordering::Equal,
        BinaryOperator::NotEq => ordering != ::std::cmp::Ordering::Equal,
        BinaryOperator::Lt => ordering == ::std::cmp::Ordering::Less,
        BinaryOperator::LtEq => ordering != ::std::cmp::Ordering::Greater,
        BinaryOperator::Gt => ordering == ::std::cmp::Ordering::Greater,
        BinaryOperator::GtEq => ordering != ::std::cmp::Ordering::Less,
        _ => return Evaluated::Undecided,
    };
    Evaluated::Known(Value::Bool(holds))
}

/// What an operator answers when a side is not a value: undecided where a side is, and SQL's
/// unknown otherwise.
fn undecided_or_unknown(left: &Evaluated, right: &Evaluated) -> Evaluated {
    match (left, right) {
        (Evaluated::Undecided, _) | (_, Evaluated::Undecided) => Evaluated::Undecided,
        _ => Evaluated::Absent,
    }
}

/// How `left` orders against `right`, or `None` when the two do not compare.
///
/// Two JSON numbers that are both integers are compared as integers, so two ids beyond the
/// range `f64` represents exactly do not compare equal to each other; everything else
/// compares as `f64`.
fn ordering(left: &Value, right: &Value) -> Option<::std::cmp::Ordering> {
    match (left, right) {
        (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
        (Value::Bool(left), Value::Bool(right)) => Some(left.cmp(right)),
        _ => match (as_integer(left), as_integer(right)) {
            (Some(left), Some(right)) => Some(left.cmp(&right)),
            _ => as_number(left)?.partial_cmp(&as_number(right)?),
        },
    }
}

/// `value` as the integer it is, or `None` when it is not a JSON integer.
fn as_integer(value: &Value) -> Option<i128> {
    let number = value.as_number()?;
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

/// `value` as the number it is, or the number the string it is spells — the implicit conversion
/// between a string and a number the AWS IoT SQL reference gives.
fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(_) => value.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// `left <op> right` for one of SQL's arithmetic operators, over the numbers both sides are.
/// Unknown where either side is ([`compare`] documents that reading), and undecided where a
/// side is not a number at all or the result is not one a JSON document can carry (a division
/// by zero) — so no arithmetic on a caller's statement can trap.
///
/// An expression over INTEGERS whose result is itself a whole number is reported as that
/// integer, because that is the value the expression has: a JSON document carries no
/// distinction between `68` and `68.0`, and a projection that wrote the second where the
/// message's own members are integers would read back as a different document from the one the
/// statement computed. Everything else is reported as the number it is.
fn arithmetic(left: &Evaluated, right: &Evaluated, op: &BinaryOperator) -> Evaluated {
    let (Evaluated::Known(left), Evaluated::Known(right)) = (left, right) else {
        return undecided_or_unknown(left, right);
    };
    if left.is_null() || right.is_null() {
        return Evaluated::Absent;
    }
    let whole = as_integer(left).is_some() && as_integer(right).is_some();
    let (Some(left), Some(right)) = (as_number(left), as_number(right)) else {
        return Evaluated::Undecided;
    };
    let computed = match op {
        BinaryOperator::Plus => left + right,
        BinaryOperator::Minus => left - right,
        BinaryOperator::Multiply => left * right,
        BinaryOperator::Divide => left / right,
        BinaryOperator::Modulo => left % right,
        _ => return Evaluated::Undecided,
    };
    if whole && computed.fract() == 0.0 && computed.abs() < INTEGER_RESULT_CEILING {
        #[allow(clippy::cast_possible_truncation)]
        return Evaluated::Known(Value::Number(::serde_json::Number::from(computed as i64)));
    }
    ::serde_json::Number::from_f64(computed).map_or(Evaluated::Undecided, |number| {
        Evaluated::Known(Value::Number(number))
    })
}

/// The magnitude up to which a whole result of integer arithmetic is reported as an integer:
/// `2^53`, the largest magnitude `f64` — which is what the arithmetic above is computed in —
/// represents every integer below exactly. Past it the computed value is no longer the integer
/// the expression names, so it is reported as the number it is.
const INTEGER_RESULT_CEILING: f64 = 9_007_199_254_740_992.0;
