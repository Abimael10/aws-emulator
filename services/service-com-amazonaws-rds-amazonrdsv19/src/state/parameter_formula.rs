// SPDX-License-Identifier: CC0-1.0

//! The DB parameter defaults Amazon RDS publishes as a FORMULA, evaluated for the hardware
//! of one DB instance class.
//!
//! Amazon RDS publishes the default of a parameter whose value follows the instance's
//! hardware as an EXPRESSION over that hardware rather than as a number — the PostgreSQL
//! connection ceiling is `LEAST({DBInstanceClassMemory/9531392},5000)` and its shared buffer
//! pool `{DBInstanceClassMemory/32768}` — so the number the engine of an instance runs with
//! is that expression evaluated for the class that instance was created with. A REPORT of
//! the parameter publishes the formula itself, which is what AWS reports and what
//! `DescribeDBParameters` answers; only the engine turns it into a value.
//!
//! The grammar is the one the Amazon RDS User Guide's "DB parameter formulas" section
//! documents: a formula is written in braces, its operands are formula variables and
//! integers, its operators are `+`, `-`, `*` and `/` with INTEGER division, and the
//! functions `LEAST`, `GREATEST` and `SUM` take a list of them. Read with `sqlparser` — the
//! parser this crate already reads its engines' statements with — because once the braces
//! are read as the grouping they are, a formula IS an expression of that grammar: nothing
//! here splits an expression's text by hand or matches on its shape.
//!
//! The one variable resolved here is `DBInstanceClassMemory`, the memory of the instance's
//! class ([`crate::ops::describe_orderable_db_instance_options::class_memory_bytes`]). A
//! formula naming any other variable, or one that is not an expression of this grammar,
//! resolves to NO value — the caller then falls back on the engine's own compiled default
//! rather than reporting something that is not a value, exactly as it does for a class whose
//! memory this service does not publish.
//!
//! The expressions read here are the ones this service's own parameter catalogs PUBLISH —
//! `&'static str` columns of those tables — and the ones a REQUEST carries: "DB parameter
//! formulas" are a value a caller may set on a parameter group, so a modify asks whether the
//! value it was handed is a formula ([`is_formula`]) instead of holding it to the parameter's
//! published numeric range, and the engine of an instance whose group sets one runs the number
//! that expression comes to for its class. A value that is no formula is stored and reported
//! as the value it is and reaches nothing here.

use ::sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, UnaryOperator,
    Value as SqlValue,
};
use ::sqlparser::dialect::GenericDialect;

/// The formula variable this module resolves: "the number of bytes of memory available to
/// the DB process", which is the memory of the DB instance class.
const CLASS_MEMORY_VARIABLE: &str = "DBInstanceClassMemory";

/// The longest text this module reads a formula out of at all.
///
/// The expressions the published parameter catalogs carry are a few dozen characters, and a
/// parameter value a caller sets is bounded where it enters the handler, so nothing longer
/// than this is a formula any engine would run. It is checked BEFORE the text is parsed, so
/// the work one value can ask for is bounded by this length rather than by what the caller
/// sent.
const MAX_FORMULA_LEN: usize = 256;

/// How many levels deep an expression this module reads may nest.
///
/// The walks below descend the parsed expression, so the depth of that tree is what bounds
/// the work and the stack they use — and a caller-supplied value is where that depth comes
/// from: an operator chain is parsed without nesting the parser, yet yields a tree one level
/// deep per operator. The published formulas nest five levels at most, so an expression past
/// this bound is no formula of this grammar and is read as the plain value it is (and then
/// held to its parameter's own published domain) rather than descended into.
const MAX_FORMULA_DEPTH: u32 = 32;

/// The value `formula` comes to for an instance class with `class_memory_bytes` of memory,
/// or `None` for a published default that is not a formula at all, one this grammar does not
/// cover, one naming a variable this module cannot resolve, or one past the bounds above.
///
/// The braces are the formula's GROUPING — "{...}" encloses a formula expression — so they
/// are read as the parentheses they stand for and the whole is parsed as one expression.
pub fn evaluate(formula: &str, class_memory_bytes: u64) -> Option<i64> {
    value_of(
        &parse(formula)?,
        i64::try_from(class_memory_bytes).ok()?,
        MAX_FORMULA_DEPTH,
    )
}

/// Whether `value` IS a formula of this grammar — a braced expression over formula variables
/// and whole numbers — whatever the variables it names come to.
///
/// A caller may SET a parameter to a formula, not only read one the catalog publishes: Amazon
/// RDS's "DB parameter formulas" lets the value of an integer parameter be an expression over
/// the DB instance's hardware, and the service evaluates it for the class the instance runs
/// on. Such a value is therefore not a number, and holding it to the parameter's published
/// numeric range would refuse a value the service accepts; the question "is this a formula at
/// all" is this one, asked about the text the request carried rather than about a catalog
/// column. A variable this module cannot resolve ([`variable`]) still makes the value a
/// formula — which variables an expression may name is the engine's business, and the value is
/// one of the grammar either way.
/// An expression past [`MAX_FORMULA_LEN`] or [`MAX_FORMULA_DEPTH`] is not one: the bounds are
/// what keep the work a caller-supplied value asks of the walk below finite.
pub fn is_formula(value: &str) -> bool {
    parse(value).is_some_and(|parsed| follows_the_grammar(&parsed, MAX_FORMULA_DEPTH))
}

/// `formula` parsed as ONE expression of the grammar's text, or `None` for a value carrying
/// no formula, one past [`MAX_FORMULA_LEN`], or one that is no expression at all.
///
/// The braces are the formula's GROUPING, so they are read as the parentheses they stand for;
/// a value carrying none of them is not a formula, which is what keeps a plain number out of
/// this reading. The WHOLE text has to be that one expression: a value whose text goes on
/// after the expression ends is not a formula either — nothing of the grammar follows an
/// expression — so the token after it must be the end of the text.
fn parse(formula: &str) -> Option<Expr> {
    if !formula.contains('{') || formula.len() > MAX_FORMULA_LEN {
        return None;
    }
    let expression = formula.replace('{', "(").replace('}', ")");
    let mut parser = ::sqlparser::parser::Parser::new(&GenericDialect {})
        .try_with_sql(&expression)
        .ok()?;
    let parsed = parser.parse_expr().ok()?;
    (parser.peek_token().token == ::sqlparser::tokenizer::Token::EOF).then_some(parsed)
}

/// Whether one node of a parsed formula is a node of this grammar: a variable, a whole
/// number, a signed one, one of the four arithmetic operators over them, or one of the
/// functions [`apply`] computes.
///
/// This is [`value_of`] with the VALUES left out — the shapes are the same, so the two cannot
/// disagree about what the grammar covers, `budget` included: an expression deeper than the
/// levels left is no expression of this grammar.
fn follows_the_grammar(expr: &Expr, budget: u32) -> bool {
    let Some(budget) = budget.checked_sub(1) else {
        return false;
    };
    match expr {
        Expr::Nested(inner) => follows_the_grammar(inner, budget),
        Expr::Identifier(_) => true,
        Expr::Value(value) => matches!(&value.value, SqlValue::Number(digits, _)
            if digits.parse::<i64>().is_ok()),
        Expr::UnaryOp { op, expr } => {
            matches!(op, UnaryOperator::Minus | UnaryOperator::Plus)
                && follows_the_grammar(expr, budget)
        }
        Expr::BinaryOp { left, op, right } => {
            matches!(
                op,
                BinaryOperator::Plus
                    | BinaryOperator::Minus
                    | BinaryOperator::Multiply
                    | BinaryOperator::Divide
            ) && follows_the_grammar(left, budget)
                && follows_the_grammar(right, budget)
        }
        Expr::Function(function) => {
            function_name(&function.name).is_some_and(|name| FUNCTIONS.contains(&name.as_str()))
                && arguments(&function.args).is_some_and(|arguments| {
                    !arguments.is_empty()
                        && arguments
                            .iter()
                            .all(|argument| follows_the_grammar(argument, budget))
                })
        }
        _ => false,
    }
}

/// The value one node of a parsed formula comes to, with `memory` standing for the class
/// memory variable.
///
/// Every arithmetic step is CHECKED: a formula is text, and an overflow would otherwise wrap
/// into a value the engine would then run with. `budget` is the levels of the expression left
/// to descend ([`MAX_FORMULA_DEPTH`]): an expression deeper than that comes to no value, so
/// the walk this text asks for is bounded whatever the text is.
fn value_of(expr: &Expr, memory: i64, budget: u32) -> Option<i64> {
    let budget = budget.checked_sub(1)?;
    match expr {
        Expr::Nested(inner) => value_of(inner, memory, budget),
        Expr::Identifier(ident) => variable(&ident.value, memory),
        Expr::Value(value) => match &value.value {
            // A formula's operands are whole numbers; a fractional literal is not part of
            // the published grammar, and `parse` is what refuses one.
            SqlValue::Number(digits, _) => digits.parse().ok(),
            _ => None,
        },
        Expr::UnaryOp { op, expr } => match op {
            UnaryOperator::Minus => value_of(expr, memory, budget)?.checked_neg(),
            UnaryOperator::Plus => value_of(expr, memory, budget),
            _ => None,
        },
        Expr::BinaryOp { left, op, right } => {
            let left = value_of(left, memory, budget)?;
            let right = value_of(right, memory, budget)?;
            match op {
                BinaryOperator::Plus => left.checked_add(right),
                BinaryOperator::Minus => left.checked_sub(right),
                BinaryOperator::Multiply => left.checked_mul(right),
                // "The result of division is an integer" — the quotient truncated, which is
                // what `checked_div` computes and what refuses a division by zero.
                BinaryOperator::Divide => left.checked_div(right),
                _ => None,
            }
        }
        Expr::Function(function) => {
            let name = function_name(&function.name)?;
            let arguments = argument_values(&function.args, memory, budget)?;
            apply(&name, &arguments)
        }
        _ => None,
    }
}

/// The value of the formula variable `named`, or `None` for a variable this module does not
/// resolve. Variable names are matched case-insensitively, as the published formulas write
/// them in mixed case.
fn variable(named: &str, memory: i64) -> Option<i64> {
    named
        .eq_ignore_ascii_case(CLASS_MEMORY_VARIABLE)
        .then_some(memory)
}

/// The name of a called function, folded to upper case — the case the published formulas
/// write their functions in — or `None` for a qualified name, which no formula carries.
fn function_name(name: &::sqlparser::ast::ObjectName) -> Option<String> {
    match name.0.as_slice() {
        [part] => part
            .as_ident()
            .map(|ident| ident.value.to_ascii_uppercase()),
        _ => None,
    }
}

/// The values of a call's arguments, in the order written, or `None` where any of them is
/// not an expression of this grammar or is deeper than the levels left to descend.
fn argument_values(
    arguments: &FunctionArguments,
    memory: i64,
    budget: u32,
) -> Option<Vec<i64>> {
    self::arguments(arguments)?
        .into_iter()
        .map(|argument| value_of(argument, memory, budget))
        .collect()
}

/// The expressions a call's argument list carries, in the order written, or `None` for an
/// argument form no formula uses — read once here so [`argument_values`] and
/// [`follows_the_grammar`] walk the same list.
fn arguments(arguments: &FunctionArguments) -> Option<Vec<&Expr>> {
    let FunctionArguments::List(list) = arguments else {
        return None;
    };
    list.args
        .iter()
        .map(|argument| match argument {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
            _ => None,
        })
        .collect()
}

/// The functions of the formula grammar, which is both what [`apply`] computes and what
/// [`follows_the_grammar`] recognises.
const FUNCTIONS: [&str; 3] = ["LEAST", "GREATEST", "SUM"];

/// One function of the formula grammar over its evaluated arguments: `LEAST` is the smallest
/// of them, `GREATEST` the largest and `SUM` their total — the three the published formulas
/// use. A call with no argument, and any other function, resolves to no value.
fn apply(function: &str, arguments: &[i64]) -> Option<i64> {
    match function {
        "LEAST" => arguments.iter().copied().min(),
        "GREATEST" => arguments.iter().copied().max(),
        "SUM" => arguments
            .iter()
            .copied()
            .try_fold(0i64, |total, value| total.checked_add(value)),
        _ => None,
    }
}

/// The published formulas this service's parameter catalogs carry, each evaluated for a
/// class whose memory is known, plus the grammar's own edges: these pin the reading of the
/// expressions the catalogs publish, which is what the engine then runs with.
#[cfg(test)]
mod tests {
    use super::evaluate;

    /// One gibibyte, so the rows below read as the published class memories do.
    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn a_published_formula_is_the_value_its_expression_comes_to() {
        // The rows span what the grammar generalises over: a division, a division with a
        // ceiling, a multiplication by a fraction, each at two class sizes, and the forms
        // that resolve to nothing.
        let cases: [(&str, &str, u64, Option<i64>); 12] = [
            (
                "the PostgreSQL connection ceiling on a 2 GiB class",
                "LEAST({DBInstanceClassMemory/9531392},5000)",
                2 * GIB,
                Some(225),
            ),
            (
                "the same formula where the ceiling binds",
                "LEAST({DBInstanceClassMemory/9531392},5000)",
                128 * GIB,
                Some(5000),
            ),
            (
                "the MySQL connection formula on a 1 GiB class",
                "{DBInstanceClassMemory/12582880}",
                GIB,
                Some(85),
            ),
            (
                "the shared buffer pool in 8 kB pages",
                "{DBInstanceClassMemory/32768}",
                2 * GIB,
                Some(65536),
            ),
            (
                "a formula with a product and a quotient",
                "{DBInstanceClassMemory*3/4}",
                GIB,
                Some(805306368),
            ),
            (
                "the floor form of the same grammar",
                "GREATEST({DBInstanceClassMemory/9531392},1024)",
                GIB,
                Some(1024),
            ),
            (
                "a sum of two formula terms",
                "SUM({DBInstanceClassMemory/9531392},{DBInstanceClassMemory/9531392})",
                2 * GIB,
                Some(450),
            ),
            (
                "a published default that is a plain value",
                "100",
                2 * GIB,
                None,
            ),
            (
                "a formula naming a variable this service does not resolve",
                "{DBInstanceVCPU*2}",
                2 * GIB,
                None,
            ),
            (
                "a formula that is no expression at all",
                "{DBInstanceClassMemory/}",
                2 * GIB,
                None,
            ),
            (
                "a value whose text goes on after the expression ends",
                "{DBInstanceClassMemory} and then some",
                2 * GIB,
                None,
            ),
            (
                "an operator outside the four the grammar publishes",
                "{DBInstanceClassMemory%4}",
                2 * GIB,
                None,
            ),
        ];

        for (case, formula, memory, expected) in cases {
            let evaluated = evaluate(formula, memory);
            assert_eq!(
                evaluated, expected,
                "case {case:?}: expected {expected:?} for {formula:?} at {memory} bytes, got \
                 {evaluated:?}"
            );
        }
    }

    /// A value a CALLER may set is read under the same bounds: text past the length this
    /// module reads a formula out of, and an expression deeper than the levels it descends,
    /// are no formulas of this grammar and come to no value — so the work one value can ask
    /// of either walk is finite however long or deeply chained that value is, and such a
    /// value is then held to its parameter's own published domain as the plain text it is.
    #[test]
    fn a_value_past_the_length_or_depth_bound_is_no_formula() {
        /// One operator chain of `terms` additions, braced as a formula is: an expression
        /// the parser builds without nesting ITSELF, whose tree is one level deep per term.
        fn chain(terms: usize) -> String {
            let mut expression = String::from("{1");
            for _ in 1..terms {
                expression.push_str("+1");
            }
            expression.push('}');
            expression
        }

        // (case, the value, whether this module reads a formula out of it)
        let cases: [(&str, String, bool); 5] = [
            (
                "a chain within both bounds",
                chain(super::MAX_FORMULA_DEPTH as usize / 2),
                true,
            ),
            (
                "a chain deeper than the levels this module descends",
                chain(super::MAX_FORMULA_DEPTH as usize + 4),
                false,
            ),
            (
                "a chain far longer than the text this module reads",
                chain(super::MAX_FORMULA_LEN * 4),
                false,
            ),
            (
                "a formula at the published length",
                "SUM({DBInstanceClassMemory/9531392},{DBInstanceClassMemory/9531392})"
                    .to_string(),
                true,
            ),
            (
                "the same expression padded past the length bound",
                format!(
                    "{}{}",
                    " ".repeat(super::MAX_FORMULA_LEN),
                    "{DBInstanceClassMemory/32768}"
                ),
                false,
            ),
        ];

        for (case, value, is_a_formula) in cases {
            let read = super::is_formula(&value);
            assert_eq!(
                read, is_a_formula,
                "case {case:?}: expected is_formula to answer {is_a_formula} for a value of \
                 {} characters, got {read}",
                value.len()
            );
            let evaluated = evaluate(&value, 2 * GIB);
            assert_eq!(
                evaluated.is_some(),
                is_a_formula,
                "case {case:?}: expected a value only for an expression of this grammar, got \
                 {evaluated:?}"
            );
        }
    }
}
