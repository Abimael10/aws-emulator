// SPDX-License-Identifier: CC0-1.0

//! The dashboard-body schema every operation that takes a dashboard definition
//! validates `body` against, and the refusal text a violated schema renders.
//!
//! `body`'s model doc is "The dashboard definition, as a JSON document", and the model
//! can express only its length — the document's own schema is not modelled, so it is
//! stated here: the definition is a JSON OBJECT, CLOSED at the top level, whose single
//! member `panels` is required and is a list of panel objects, and every panel object
//! carries the members a panel is laid out by — its `type` and its `layout`.
//!
//! A body is validated against that schema as a whole rather than up to its first
//! problem: EVERY constraint it violates is collected, and the refusal names each one as
//! its own clause, so one round trip tells a caller everything it has to fix. A clause
//! is `<location>: does not satisfy the '<constraint>' constraint of the dashboard body
//! schema`, and the clauses are joined with `; `. A location is the document root
//! ([`DOCUMENT_ROOT`]) for a constraint the whole document violates, the member's name
//! for one a named member violates, and `panels[<index>].<member>` for one a member of a
//! panel violates. A constraint is named as the JSON-schema keyword it is: `type`,
//! `required`, `items`, `additionalProperties`.
//!
//! Text that is not JSON at all is refused one step earlier, and reported as what it is:
//! there is no document to hold to the schema, so the refusal carries the position the
//! parse failed at rather than a schema clause.
//!
//! The refusal a body costs is BOUNDED: a body may hold as many panels as its length
//! admits, so the panel walk stops at [`PANEL_CLAUSE_BUDGET`] clauses.

/// The member of a dashboard body that carries the dashboard's panels.
pub const DASHBOARD_BODY_PANELS: &str = "panels";

/// The members the schema requires of EVERY panel object, in schema order: what the
/// panel renders (`type`) and where it sits on the dashboard (`layout`). A panel missing
/// either is not a panel the dashboard can be laid out from, so each absence is its own
/// `required` violation at that panel's position.
pub const PANEL_REQUIRED_MEMBERS: &[&str] = &["type", "layout"];

/// How many panel clauses one refusal names.
///
/// The clause list is a response the REQUEST's own body sizes: a body may hold as many
/// panels as its length admits, and each panel missing both required members would
/// otherwise add two clauses of its own to the message. So the walk stops once this many
/// panel clauses stand, which bounds both the violations collected and the message
/// rendered from them, whatever the body holds. It is far above any dashboard a caller
/// writes by hand, and a caller fixes a document from the first panels it names: the
/// refusal is the same answer either way, and a body still carrying a malformed panel is
/// refused again when it comes back.
pub const PANEL_CLAUSE_BUDGET: usize = 20;

/// The location a constraint on the whole document is reported at.
pub const DOCUMENT_ROOT: &str = "$";

/// The keyword for "this value is of the wrong JSON type" — the document root when it is
/// not an object (text that is not JSON at all does not parse into one either), or a
/// member whose value is not of the type the schema gives it.
const CONSTRAINT_TYPE: &str = "type";

/// The keyword for "a member the schema requires is absent".
const CONSTRAINT_REQUIRED: &str = "required";

/// The keyword for "a list member holds an entry the schema does not admit".
const CONSTRAINT_ITEMS: &str = "items";

/// The keyword for "this object carries a member the schema does not declare" — the
/// schema is closed, so any top-level member beyond `panels` violates it.
const CONSTRAINT_ADDITIONAL_PROPERTIES: &str = "additionalProperties";

/// What joins one violation's clause to the next in the refusal text.
const CLAUSE_SEPARATOR: &str = "; ";

/// One violated constraint of the dashboard-body schema: WHERE in the document it was
/// violated, and WHICH constraint was violated there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardBodyViolation {
    /// The document root, or the name of the member that violated the constraint.
    pub location: String,
    /// The JSON-schema keyword the document violates at [`Self::location`].
    pub constraint: &'static str,
}

impl DashboardBodyViolation {
    /// This violation as the one clause it contributes to the refusal text.
    pub fn clause(&self) -> String {
        format!(
            "{}: does not satisfy the '{}' constraint of the dashboard body schema",
            self.location, self.constraint
        )
    }

    /// A constraint the whole document violates.
    fn at_root(constraint: &'static str) -> Self {
        Self {
            location: DOCUMENT_ROOT.to_string(),
            constraint,
        }
    }

    /// A constraint the named member violates.
    fn at_member(member: &str, constraint: &'static str) -> Self {
        Self {
            location: member.to_string(),
            constraint,
        }
    }

    /// A constraint the member of the panel at `index` violates.
    fn at_panel_member(index: usize, member: &str, constraint: &'static str) -> Self {
        Self {
            location: format!("{DASHBOARD_BODY_PANELS}[{index}].{member}"),
            constraint,
        }
    }
}

/// Every constraint of the dashboard-body schema `body` violates, in document order:
/// the root's own type first, then the `panels` member and its entries (up to
/// [`PANEL_CLAUSE_BUDGET`] clauses), then the closedness of the document. An empty result
/// means `body` IS a dashboard definition.
///
/// Collecting rather than returning at the first problem is what lets a caller fix a
/// body in one round trip: a document that both omits `panels` and carries a member the
/// schema does not declare violates two constraints, and both are reported.
pub fn dashboard_body_violations(body: &str) -> Vec<DashboardBodyViolation> {
    match ::serde_json::from_str::<::serde_json::Value>(body) {
        Ok(document) => document_violations(&document),
        // Text that is not JSON at all is no document of the schema's type either, so it
        // violates the root's `type` constraint and nothing below the root is reachable
        // to be checked. The REFUSAL for it is [`dashboard_body_rejection`]'s parse
        // diagnostic rather than this clause.
        Err(_) => vec![DashboardBodyViolation::at_root(CONSTRAINT_TYPE)],
    }
}

/// Every constraint of the schema the PARSED `document` violates, in document order.
fn document_violations(document: &::serde_json::Value) -> Vec<DashboardBodyViolation> {
    let Some(members) = document.as_object() else {
        return vec![DashboardBodyViolation::at_root(CONSTRAINT_TYPE)];
    };

    let mut violations = Vec::new();
    match members.get(DASHBOARD_BODY_PANELS) {
        None => violations.push(DashboardBodyViolation::at_member(
            DASHBOARD_BODY_PANELS,
            CONSTRAINT_REQUIRED,
        )),
        Some(panels) => match panels.as_array() {
            None => violations.push(DashboardBodyViolation::at_member(
                DASHBOARD_BODY_PANELS,
                CONSTRAINT_TYPE,
            )),
            Some(panels) => violations.extend(panel_violations(panels)),
        },
    }
    if members.keys().any(|name| name != DASHBOARD_BODY_PANELS) {
        violations.push(DashboardBodyViolation::at_root(
            CONSTRAINT_ADDITIONAL_PROPERTIES,
        ));
    }
    violations
}

/// Every constraint the entries of a `panels` list violate, walked in index order, up to
/// [`PANEL_CLAUSE_BUDGET`] clauses.
///
/// An entry that is not an object at all is not admitted as an entry of the list, which
/// is the list's own `items` constraint and is reported once however many entries break
/// it; an entry that IS an object is held to the panel's required members, each absence
/// reported at that panel's own position.
fn panel_violations(panels: &[::serde_json::Value]) -> Vec<DashboardBodyViolation> {
    let mut violations = Vec::new();
    let mut reported_items = false;
    for (index, panel) in panels.iter().enumerate() {
        // The bound is checked before each entry is walked, so neither the collection nor
        // the message rendered from it grows with the number of panels a body holds.
        if violations.len() >= PANEL_CLAUSE_BUDGET {
            break;
        }
        match panel.as_object() {
            None => {
                if !reported_items {
                    reported_items = true;
                    violations.push(DashboardBodyViolation::at_member(
                        DASHBOARD_BODY_PANELS,
                        CONSTRAINT_ITEMS,
                    ));
                }
            }
            Some(panel) => {
                for required in PANEL_REQUIRED_MEMBERS {
                    if !panel.contains_key(*required) {
                        violations.push(DashboardBodyViolation::at_panel_member(
                            index,
                            required,
                            CONSTRAINT_REQUIRED,
                        ));
                    }
                }
            }
        }
    }
    violations
}

/// The fixed text of the refusal for a body whose text is not JSON.
pub const NOT_VALID_JSON_PREFIX: &str = "dashboard body is not valid JSON";

/// The refusal text for a body that could not be PARSED, naming the position the parse
/// failed at.
///
/// A body that does not parse has no document to hold to the schema, so the refusal says
/// that instead of naming a schema constraint: the fixed [`NOT_VALID_JSON_PREFIX`], the
/// line the parse stopped on and the position in that line, both properties of the
/// submitted text. The position is counted from the start of the line, where the line's
/// FIRST character is position 0; the parser counts its own columns from one, so the
/// column it reports is converted here rather than reported raw.
fn not_valid_json_message(error: &::serde_json::Error) -> String {
    format!(
        "{NOT_VALID_JSON_PREFIX} at line {} column {}",
        error.line(),
        error.column().saturating_sub(1)
    )
}

/// The refusal text for a `body` that is not a dashboard definition, or `None` when it is
/// one: the parse diagnostic when the text is not JSON at all, and otherwise one clause
/// per violated constraint, joined with `; `.
///
/// This is the message an operation carries into its modelled `ValidationException`.
pub fn dashboard_body_rejection(body: &str) -> Option<String> {
    let document = match ::serde_json::from_str::<::serde_json::Value>(body) {
        Ok(document) => document,
        Err(error) => return Some(not_valid_json_message(&error)),
    };
    let violations = document_violations(&document);
    (!violations.is_empty()).then(|| {
        violations
            .iter()
            .map(DashboardBodyViolation::clause)
            .collect::<Vec<String>>()
            .join(CLAUSE_SEPARATOR)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_violating_two_constraints_reports_both_in_document_order() {
        // Rows: the body, and the (location, constraint) pairs the schema says it
        // violates — in the order the clauses are rendered.
        for (body, expected) in [
            (r#"{"panels":[]}"#, vec![]),
            (
                r#"{"panels":[{"type":"logs","layout":{"x":0}}]}"#,
                vec![],
            ),
            (
                r#"{"widgets":[]}"#,
                vec![("panels", "required"), ("$", "additionalProperties")],
            ),
            (r#"{}"#, vec![("panels", "required")]),
            (
                r#"{"panels":"none","widgets":[]}"#,
                vec![("panels", "type"), ("$", "additionalProperties")],
            ),
            (r#"{"panels":[1]}"#, vec![("panels", "items")]),
            // A panel is held to its own required members, each absence at that panel's
            // position, and the panels are walked in index order.
            (
                r#"{"panels":[{}]}"#,
                vec![
                    ("panels[0].type", "required"),
                    ("panels[0].layout", "required"),
                ],
            ),
            (
                r#"{"panels":[{"type":"logs"}]}"#,
                vec![("panels[0].layout", "required")],
            ),
            (
                r#"{"panels":[{"type":"logs","layout":{}},{"layout":{}}]}"#,
                vec![("panels[1].type", "required")],
            ),
            (
                r#"{"panels":[{"title":"p99"},1],"widgets":[]}"#,
                vec![
                    ("panels[0].type", "required"),
                    ("panels[0].layout", "required"),
                    ("panels", "items"),
                    ("$", "additionalProperties"),
                ],
            ),
            (r#"{"panels":[],"widgets":[]}"#, vec![("$", "additionalProperties")]),
            (r#"[{"type":"logs"}]"#, vec![("$", "type")]),
            ("panels", vec![("$", "type")]),
        ] {
            let actual: Vec<(String, &str)> = dashboard_body_violations(body)
                .into_iter()
                .map(|violation| (violation.location, violation.constraint))
                .collect();
            let actual: Vec<(&str, &str)> = actual
                .iter()
                .map(|(location, constraint)| (location.as_str(), *constraint))
                .collect();
            assert_eq!(
                actual, expected,
                "case {body:?}: expected violations {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn the_refusal_joins_one_clause_per_violation() {
        assert_eq!(
            dashboard_body_rejection(r#"{"widgets":[]}"#).as_deref(),
            Some(
                "panels: does not satisfy the 'required' constraint of the dashboard body \
                 schema; $: does not satisfy the 'additionalProperties' constraint of the \
                 dashboard body schema"
            ),
            "a document that omits panels and carries another member reports both clauses"
        );
        assert_eq!(
            dashboard_body_rejection(r#"{"panels":[{}]}"#).as_deref(),
            Some(
                "panels[0].type: does not satisfy the 'required' constraint of the dashboard \
                 body schema; panels[0].layout: does not satisfy the 'required' constraint of \
                 the dashboard body schema"
            ),
            "a panel missing both required members reports one clause for each"
        );
        assert_eq!(
            dashboard_body_rejection(r#"{"panels":[]}"#),
            None,
            "a conforming document is not refused"
        );
    }

    #[test]
    fn a_body_that_is_not_json_is_refused_at_the_position_the_parse_failed() {
        // Rows: text that is not JSON at all, text that starts as a document and stops,
        // and text whose syntax fails further in — the position is where each one
        // actually fails, so it is read from the parser rather than fixed.
        for (body, expected) in [
            (
                "panels",
                "dashboard body is not valid JSON at line 1 column 0",
            ),
            (
                r#"{"panels":[{"type":"logs"}"#,
                "dashboard body is not valid JSON at line 1 column 25",
            ),
            (
                r#"{"panels":[],}"#,
                "dashboard body is not valid JSON at line 1 column 13",
            ),
        ] {
            let actual = dashboard_body_rejection(body);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {body:?}: expected {expected:?}, got {actual:?}"
            );
        }
        // A document that PARSES is held to the schema instead, so its refusal is the
        // clause list rather than a parse diagnostic.
        let parsed = dashboard_body_rejection("7").unwrap_or_default();
        assert!(
            !parsed.starts_with(NOT_VALID_JSON_PREFIX),
            "a JSON document of the wrong type must be refused by the schema, got {parsed:?}"
        );
    }

    #[test]
    fn a_refusal_names_at_most_the_panel_clause_budget() {
        // Rows: a panels list that fills the budget exactly, one far past it, and one of
        // entries that are not panels at all. Each clause is about 90 characters, so the
        // bound on the clause count is what bounds the message a body can cost.
        for (case, panels, entry) in [
            ("a list that fills the budget", PANEL_CLAUSE_BUDGET / 2, "{}"),
            ("a list far past the budget", 500, "{}"),
            ("entries that are not panels", 500, "1"),
        ] {
            let body = format!(r#"{{"panels":[{}]}}"#, vec![entry; panels].join(","));
            let violations = dashboard_body_violations(&body);
            assert!(
                violations.len() <= PANEL_CLAUSE_BUDGET,
                "case {case:?}: expected at most {PANEL_CLAUSE_BUDGET} clauses for {panels} \
                 entries, got {}",
                violations.len()
            );
            let message = dashboard_body_rejection(&body).unwrap_or_default();
            assert!(
                message.len() <= PANEL_CLAUSE_BUDGET * 128,
                "case {case:?}: expected a bounded refusal for {panels} entries, got {} \
                 characters",
                message.len()
            );
        }

        // The clauses named are the FIRST panels of the list: a body one panel past the
        // budget names every panel up to it and none beyond.
        let past = PANEL_CLAUSE_BUDGET / 2 + 1;
        let body = format!(r#"{{"panels":[{}]}}"#, vec!["{}"; past].join(","));
        let message = dashboard_body_rejection(&body).unwrap_or_default();
        let named = format!("panels[{}].type", past - 2);
        let beyond = format!("panels[{}].type", past - 1);
        assert!(
            message.contains(&named) && !message.contains(&beyond),
            "expected the refusal to name {named} and not {beyond}, got {message:?}"
        );
    }
}
