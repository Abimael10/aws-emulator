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
//! `&'static str` columns of those tables — and never text a request carried: a value a
//! caller sets on a parameter group is stored and reported as the value it is, and reaches
//! nothing here.

use ::sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, UnaryOperator,
    Value as SqlValue,
};
use ::sqlparser::dialect::GenericDialect;

/// The formula variable this module resolves: "the number of bytes of memory available to
/// the DB process", which is the memory of the DB instance class.
const CLASS_MEMORY_VARIABLE: &str = "DBInstanceClassMemory";

/// The value `formula` comes to for an instance class with `class_memory_bytes` of memory,
/// or `None` for a published default that is not a formula at all, one this grammar does not
/// cover, or one naming a variable this module cannot resolve.
///
/// The braces are the formula's GROUPING — "{...}" encloses a formula expression — so they
/// are read as the parentheses they stand for and the whole is parsed as one expression.
pub fn evaluate(formula: &str, class_memory_bytes: u64) -> Option<i64> {
    if !formula.contains('{') {
        return None;
    }
    let expression = formula.replace('{', "(").replace('}', ")");
    let parsed = ::sqlparser::parser::Parser::new(&GenericDialect {})
        .try_with_sql(&expression)
        .ok()?
        .parse_expr()
        .ok()?;
    value_of(&parsed, i64::try_from(class_memory_bytes).ok()?)
}

/// The value one node of a parsed formula comes to, with `memory` standing for the class
/// memory variable.
///
/// Every arithmetic step is CHECKED: a formula is published text, and an overflow would
/// otherwise wrap into a value the engine would then run with.
fn value_of(expr: &Expr, memory: i64) -> Option<i64> {
    match expr {
        Expr::Nested(inner) => value_of(inner, memory),
        Expr::Identifier(ident) => variable(&ident.value, memory),
        Expr::Value(value) => match &value.value {
            // A formula's operands are whole numbers; a fractional literal is not part of
            // the published grammar, and `parse` is what refuses one.
            SqlValue::Number(digits, _) => digits.parse().ok(),
            _ => None,
        },
        Expr::UnaryOp { op, expr } => match op {
            UnaryOperator::Minus => value_of(expr, memory)?.checked_neg(),
            UnaryOperator::Plus => value_of(expr, memory),
            _ => None,
        },
        Expr::BinaryOp { left, op, right } => {
            let left = value_of(left, memory)?;
            let right = value_of(right, memory)?;
            match op {
                BinaryOperator::Plus => left.checked_add(right),
                BinaryOperator::Minus => left.checked_sub(right),
                BinaryOperator::Multiply => left.checked_mul(right),
                // "The result of division is an integer" — the quotient truncated, which is
                // what `checked_div` computes and what refuses a division by zero.
                BinaryOperator::Divide => left.checked_div(right),
                BinaryOperator::Modulo => left.checked_rem(right),
                _ => None,
            }
        }
        Expr::Function(function) => {
            let name = function_name(&function.name)?;
            let arguments = argument_values(&function.args, memory)?;
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
/// not an expression of this grammar.
fn argument_values(arguments: &FunctionArguments, memory: i64) -> Option<Vec<i64>> {
    let FunctionArguments::List(list) = arguments else {
        return None;
    };
    list.args
        .iter()
        .map(|argument| match argument {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => value_of(expr, memory),
            _ => None,
        })
        .collect()
}

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
        let cases: [(&str, &str, u64, Option<i64>); 10] = [
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
}
