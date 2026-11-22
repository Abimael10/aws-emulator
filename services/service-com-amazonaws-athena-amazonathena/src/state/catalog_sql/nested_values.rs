// SPDX-License-Identifier: CC0-1.0

//! Reading INTO a column whose value is a STRUCTURE rather than one scalar.
//!
//! A Data Catalog column may be declared with a collection or record type —
//! `array<string>`, `map<string,string>`, `struct<city:string,zip:int>` — and Athena's SQL
//! reads into such a value three ways: a record's field by name (`addr.city`), an array's
//! element by position (`tags[1]`, counted from ONE) and a map's entry by key
//! (`attrs['colour']`). The engine under [`super::query_engine`] holds such a value as the
//! JSON document the object carried it as, and its own dialect has no production for any of
//! those three readings — so a statement that read into one failed as an unresolvable name.
//!
//! What this module does is name the SAME value in a production the engine does have: its own
//! JSON reading (`json_extract`), handed the document the column holds and the path the
//! statement addressed inside it. One value either way, read out of the same document, so
//! what the engine computes over it is what Athena computes over it.
//!
//! The rewrite is on the PARSE, and only over the columns the CATALOG declares as structures
//! ([`super::hive_types::is_nested`]): a qualified name whose first part is a relation, and
//! an index into anything else, are left exactly as the statement wrote them.

use ::std::collections::HashSet;
use ::std::ops::ControlFlow;

use ::service_com_amazonaws_glue_awsglue::state as glue_state;
use ::sqlparser::ast::{
    AccessExpr, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, ObjectNamePart, Statement as SqlStatement, Subscript,
    Value as SqlValue,
};

use super::hive_types;
use super::statement::DeclaredColumn;

/// The engine function that reads one value out of a JSON document at a path.
const JSON_EXTRACT: &str = "json_extract";

/// What a JSON path begins with: the document itself.
const PATH_ROOT: &str = "$";

/// One STEP of a path into a document, naming the member `member`.
///
/// The member is QUOTED, with the escape character and then the quote inside it escaped, so
/// a key carrying a dot, a space, a bracket, a quote or a backslash addresses the member it
/// names rather than a path of its own: a map's key is DATA the caller chose, and the catalog
/// admits any text as one.
fn member_step(member: &str) -> String {
    let escaped = member.replace('\\', "\\\\").replace('"', "\\\"");
    format!(".\"{escaped}\"")
}

/// Where an ARRAY's first element is, as Athena's SQL counts: "the first element of an array
/// is at index 1", so an index the statement wrote is one further along than the same index
/// into the JSON document.
const FIRST_ELEMENT: i64 = 1;

/// The columns of `columns` whose values are STRUCTURES, as the catalog folds their names.
#[must_use]
pub fn structures<'c>(columns: impl IntoIterator<Item = &'c DeclaredColumn>) -> HashSet<String> {
    columns
        .into_iter()
        .filter(|column| hive_types::is_nested(&column.data_type))
        .map(|column| glue_state::fold_name(&column.name))
        .collect()
}

/// Resolve every reading INTO one of the `structures` in `statement` to the engine's own
/// reading of the document that column holds.
pub fn resolve(statement: &mut SqlStatement, structures: &HashSet<String>) {
    if structures.is_empty() {
        return;
    }
    let _ = ::sqlparser::ast::visit_expressions_mut(statement, |expression: &mut Expr| {
        if let Some(read) = reading(expression, structures) {
            *expression = read;
        }
        ControlFlow::<()>::Continue(())
    });
}

/// The engine's reading of `expression`, when it reads into a column the catalog declares as
/// a structure.
fn reading(expression: &Expr, structures: &HashSet<String>) -> Option<Expr> {
    match expression {
        // `<column>.<field>[.<field>…]`, and the same under a relation qualifier: the FIRST
        // part that names a structure column is the document, and every part after it is a
        // step into it.
        Expr::CompoundIdentifier(parts) => {
            let at = parts.iter().position(|part| {
                structures.contains(&glue_state::fold_name(&part.value))
            })?;
            let steps: Vec<String> = parts
                .get(at + 1..)?
                .iter()
                .map(|part| member_step(&part.value))
                .collect();
            if steps.is_empty() {
                return None;
            }
            Some(extract(&parts[at], &steps.concat()))
        }
        // `<column>[<index>]`, `<column>['<key>']` and `<column>.<field>`, and any chain of
        // those: the root is the document and the chain is the path into it.
        Expr::CompoundFieldAccess { root, access_chain } => {
            let column = column_of(root)?;
            if !structures.contains(&glue_state::fold_name(&column.value)) {
                return None;
            }
            let mut path = String::new();
            for access in access_chain {
                path.push_str(&step(access)?);
            }
            (!path.is_empty()).then(|| extract(&column, &path))
        }
        _ => None,
    }
}

/// The column one access chain's root reads, or `None` when its root is not a plain column
/// reference.
fn column_of(root: &Expr) -> Option<Ident> {
    match root {
        Expr::Identifier(ident) => Some(ident.clone()),
        // A qualified root's LAST part is the column; the ones in front name the relation.
        Expr::CompoundIdentifier(parts) => parts.last().cloned(),
        _ => None,
    }
}

/// One step of a path into a document, as the access the statement wrote names it, or `None`
/// for an access this reading does not name a step for.
fn step(access: &AccessExpr) -> Option<String> {
    match access {
        AccessExpr::Dot(Expr::Identifier(field)) => Some(member_step(&field.value)),
        AccessExpr::Subscript(Subscript::Index {
            index: Expr::Value(index),
        }) => match &index.value {
            // An ARRAY is addressed by position, counted from one.
            SqlValue::Number(digits, ..) => {
                let at = digits.parse::<i64>().ok()?;
                Some(format!("[{}]", at.checked_sub(FIRST_ELEMENT)?))
            }
            // A MAP is addressed by key, which its document holds as a member name.
            SqlValue::SingleQuotedString(key)
            | SqlValue::DoubleQuotedString(key)
            | SqlValue::EscapedStringLiteral(key) => Some(member_step(key)),
            _ => None,
        },
        _ => None,
    }
}

/// The call that reads the value at `path` out of the document the column `column` holds.
fn extract(column: &Ident, path: &str) -> Expr {
    let unnamed = |expression: Expr| FunctionArg::Unnamed(FunctionArgExpr::Expr(expression));
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(JSON_EXTRACT))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                unnamed(Expr::Identifier(column.clone())),
                unnamed(Expr::Value(
                    SqlValue::SingleQuotedString(format!("{PATH_ROOT}{path}")).with_empty_span(),
                )),
            ],
            clauses: Vec::new(),
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::statement::parse_statements;

    /// The columns the cases below read, as a catalog table declares them.
    fn declared() -> Vec<DeclaredColumn> {
        [
            ("id", "int"),
            ("label", "string"),
            ("tags", "array<string>"),
            ("attrs", "map<string,string>"),
            ("addr", "struct<city:string,zip:int>"),
        ]
        .iter()
        .map(|(name, data_type)| DeclaredColumn {
            name: (*name).to_string(),
            data_type: (*data_type).to_string(),
            comment: None,
        })
        .collect()
    }

    /// RULE: a reading into a column the catalog declares as a structure becomes the
    /// engine's own reading of the document that column holds, at the path the statement
    /// addressed; every other expression is left as written.
    ///
    /// The rows span what the rule generalises over: the kind of reading (a field, an
    /// element, an entry), how deep it goes, where it sits in the statement, the column's
    /// declared type, and the expressions that read into no structure at all.
    #[test]
    fn a_reading_into_a_structure_becomes_the_engines_own_reading_of_its_document() {
        // (case, the statement, the statement after the resolution)
        let cases: Vec<(&str, &str, &str)> = vec![
            (
                "a record's field by name",
                "SELECT addr.city FROM \"t\"",
                "SELECT json_extract(addr, '$.\"city\"') FROM \"t\"",
            ),
            (
                "the same field under a relation qualifier",
                "SELECT t.addr.city FROM \"t\"",
                "SELECT json_extract(addr, '$.\"city\"') FROM \"t\"",
            ),
            (
                "an array's element, counted from one",
                "SELECT tags[1] FROM \"t\"",
                "SELECT json_extract(tags, '$[0]') FROM \"t\"",
            ),
            (
                "a later element",
                "SELECT tags[3] FROM \"t\"",
                "SELECT json_extract(tags, '$[2]') FROM \"t\"",
            ),
            (
                "a map's entry by key",
                "SELECT attrs['colour'] FROM \"t\"",
                "SELECT json_extract(attrs, '$.\"colour\"') FROM \"t\"",
            ),
            (
                "a key carrying the characters a path is written with",
                "SELECT attrs['shade.dark one'] FROM \"t\"",
                "SELECT json_extract(attrs, '$.\"shade.dark one\"') FROM \"t\"",
            ),
            (
                "a key carrying the characters the quoting is written with",
                "SELECT attrs['back\\slash'] FROM \"t\"",
                "SELECT json_extract(attrs, '$.\"back\\\\slash\"') FROM \"t\"",
            ),
            (
                "a reading in a predicate rather than in the select list",
                "SELECT id FROM \"t\" WHERE addr.city = 'Ely'",
                "SELECT id FROM \"t\" WHERE json_extract(addr, '$.\"city\"') = 'Ely'",
            ),
            (
                "a column the catalog declares as one scalar, qualified by its relation",
                "SELECT t.label FROM \"t\"",
                "SELECT t.label FROM \"t\"",
            ),
            (
                "a column read whole",
                "SELECT tags FROM \"t\"",
                "SELECT tags FROM \"t\"",
            ),
            (
                "an index into a column that is no structure",
                "SELECT label[1] FROM \"t\"",
                "SELECT label[1] FROM \"t\"",
            ),
        ];

        for (case, sql, expected) in &cases {
            let columns = declared();
            let mut statement = parse_statements(sql)
                .and_then(|mut statements| {
                    (statements.len() == 1).then(|| statements.remove(0))
                })
                .unwrap_or_else(|| panic!("case {case:?}: the case parses as one statement"));
            resolve(&mut statement, &structures(columns.iter()));
            let actual = statement.to_string();
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
