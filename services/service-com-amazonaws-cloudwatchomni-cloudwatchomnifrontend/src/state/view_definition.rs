// SPDX-License-Identifier: CC0-1.0

//! What a view's `definition` has to be, and the refusal text a definition that is not
//! it renders.
//!
//! `ViewDefinition`'s model doc is "View definition (SQL query). Maximum 10,000
//! characters", and `CreateView`'s is "A view is a named, reusable SQL query that can be
//! referenced from telemetry queries". The model expresses only the definition's length,
//! so the rest of the rule is stated here: a definition is ONE SQL query. It is decided
//! by PARSING the definition with a real SQL parser rather than by inspecting its text,
//! so a query is admitted for being a query rather than for the shape of its source.
//!
//! What the refusal says is decided by the CLASS of the defect, and recorded behavior
//! words the three classes differently:
//!
//! * text no SQL parser can read as a statement at all is told the parser's own reading
//!   of it under one fixed prefix — what was expected, the token it stopped on and that
//!   token's line and column — because the defect is in the text itself and nothing but
//!   the text says where;
//! * text read as exactly one statement that is not a query is told the RULE it broke, as
//!   one fixed sentence: the statement is well-formed SQL, so there is nothing to point
//!   at, and what the caller has to change is the kind of statement it submitted;
//! * text read as any other number of statements is told the other rule it broke, also as
//!   one fixed sentence, for the same reason: each statement may be well-formed, and what
//!   is wrong is how many there are.
//!
//! Neither fixed sentence carries a variable part, so a client that matches or surfaces
//! one of them reads the same text whichever definition earned it.

use ::sqlparser::ast::Statement;
use ::sqlparser::dialect::GenericDialect;
use ::sqlparser::parser::Parser;

/// The fixed text a refusal of text that is not SQL opens with; the parser's own rendering
/// of what is wrong with the definition follows it verbatim.
pub const INVALID_VIEW_DEFINITION: &str = "Invalid view definition: ";

/// The whole text a refusal of one statement that is not a query carries: the rule such a
/// definition broke, stated once.
pub const VIEW_DEFINITION_MUST_BE_A_SELECT_QUERY: &str = "View definition must be a SELECT query";

/// The whole text a refusal of anything but one statement carries: the rule such a
/// definition broke, stated once.
pub const VIEW_DEFINITION_MUST_BE_ONE_STATEMENT: &str =
    "View definition must contain exactly one SQL statement";

/// Why `definition` is not a view definition, or `None` when it is one.
///
/// The parse is bounded twice over: `ViewDefinition`'s modelled `@length(1..10000)` caps
/// the text the tokenizer walks, and the parser's own recursion limit ends a deeply
/// nested expression with an error rather than a deep stack. Nothing here can panic on
/// caller text: the parser returns its failures, and they are what classes the refusal.
pub fn view_definition_rejection(definition: &str) -> Option<String> {
    let dialect = GenericDialect {};
    let statements = match Parser::parse_sql(&dialect, definition) {
        Ok(statements) => statements,
        // Text no SQL parser can read as statements at all. ITS diagnosis is the
        // refusal: the expectation, the offending token and that token's position are
        // all facts about the submitted text, which is what the caller has to fix.
        Err(error) => return Some(format!("{INVALID_VIEW_DEFINITION}{error}")),
    };
    match statements.as_slice() {
        // One query: the definition IS a view definition.
        [Statement::Query(_)] => None,
        // One statement of another kind — a mutation, a definition, a transaction
        // control. A view is the query it is defined by, so the kind is the defect.
        [_] => Some(VIEW_DEFINITION_MUST_BE_A_SELECT_QUERY.to_string()),
        // Any other count: more than one statement, or text that carries none at all.
        _ => Some(VIEW_DEFINITION_MUST_BE_ONE_STATEMENT.to_string()),
    }
}
