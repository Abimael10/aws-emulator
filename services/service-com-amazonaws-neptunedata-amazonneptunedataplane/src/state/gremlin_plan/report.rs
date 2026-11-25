// SPDX-License-Identifier: CC0-1.0

//! The Gremlin explain REPORT: the text an explain answers with.
//!
//! The report is a banner followed by one section per thing an explain states, each headed
//! by its name underlined with `=`, in this order:
//!
//! 1. `Query String` — the traversal as it was submitted;
//! 2. `Original Traversal` — the steps that traversal compiles to, the way Apache TinkerPop
//!    prints a compiled traversal;
//! 3. the two header lines of the run itself, `Explain Time` (when the explain ran) and
//!    `Engine Version` (the engine version of the database serving it);
//! 4. `Converted Traversal` — the plan the engine converts the traversal into, with the
//!    steps conversion left to the step iterators named in a `+ not converted into Neptune
//!    steps:` trailer;
//! 5. `Optimized Traversal` — the same plan after optimization, which folds a filter into
//!    the pattern it constrains and answers a terminal count inside the graph query;
//! 6. `Predicates` — how many distinct predicates the graph being queried holds.
//!
//! Both plan sections print the SAME plan ([`ExplainPlan`], built by this module's parent)
//! and differ only in how much of it is optimized, which is why the plan is carried in its
//! positions rather than as text: the converted plan states a pinned label as a filter
//! beside the pattern that matches the label statement, and the optimized plan folds that
//! label into the pattern's object position — so the projection no longer has to be made
//! distinct — and reports a terminal count as a count the graph query answers.

use std::fmt::Write as _;
use std::time::SystemTime;

use super::*;

/// The title the report is headed with, centred between two rules.
const REPORT_TITLE: &str = "Neptune Gremlin Explain";

/// The indentation one entry of a Neptune step list is written at. An entry is listed by
/// its indentation and separated from the next by a comma — not bulleted.
const ENTRY_INDENT: &str = "    ";

/// The indentation of the join group inside one step entry, and of the patterns inside that
/// group.
const GROUP_INDENT: &str = "        ";
const PATTERN_INDENT: &str = "            ";

/// The step a graph query hands its matches to the rest of the traversal through.
const TRAVERSER_CONVERTER_STEP: &str = "NeptuneTraverserConverterStep";

/// The step an optimized plan answers a terminal count with: the count is computed over
/// the matches of the patterns instead of over traversers a step iterator counted.
const COUNT_STEP: &str = "NeptuneCountGlobalStep";

/// The pattern kind a statement about an element's LABEL is matched by, which the plan
/// prefixes that pattern with.
const LABEL_PATTERN_PREFIX: &str = "VL";

/// The projection of a pattern that binds nothing: it asks whether the statement is there
/// rather than projecting a variable out of it.
const ASK_PROJECTION: &str = "ask";

/// What the DATABASE serving an explain contributes to the report: the two header lines and
/// the predicate count are facts of the engine and of its graph, not of the traversal, so
/// they are handed to the renderer rather than derived from the query.
#[derive(Debug, Clone, Copy)]
pub struct ExplainContext<'version> {
    /// The instant the explain ran.
    pub explained_at: SystemTime,
    /// The engine version of the database serving the explain — the same version its
    /// engine status reports.
    pub engine_version: &'version str,
    /// How many distinct predicates the graph the plan would be evaluated against holds.
    pub predicate_count: usize,
}

/// The explain report for `query`, which `traversal` is the reading of, as the database
/// `context` describes answers it.
///
/// The plan and every line about the query are derived from the traversal alone, so the
/// same query always explains the same way against the same database; the explain instant
/// is the one value of the run itself.
pub fn gremlin_explain_report(
    query: &str,
    traversal: &Traversal,
    context: &ExplainContext<'_>,
) -> String {
    let printed = printed_steps(traversal);
    let plan = build_plan(traversal, &printed);

    let mut report = String::new();
    let _ = writeln!(report, "{REPORT_RULE}");
    let _ = writeln!(report, "{}{REPORT_TITLE}", " ".repeat(title_indent()));
    let _ = writeln!(report, "{REPORT_RULE}");
    let _ = writeln!(report);

    heading(&mut report, "Query String");
    let _ = writeln!(report, "{}", query.trim());
    let _ = writeln!(report);

    heading(&mut report, "Original Traversal");
    let _ = writeln!(
        report,
        "[{}]",
        printed
            .iter()
            .map(|step| step.text.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(report);

    let _ = writeln!(
        report,
        "Explain Time: {}",
        explain_timestamp(context.explained_at)
    );
    let _ = writeln!(report, "Engine Version: {}", context.engine_version);
    let _ = writeln!(report);

    heading(&mut report, "Converted Traversal");
    write_plan(&mut report, &plan, false);
    let _ = writeln!(report);

    heading(&mut report, "Optimized Traversal");
    write_plan(&mut report, &plan, true);
    let _ = writeln!(report);

    heading(&mut report, "Predicates");
    let _ = writeln!(report, "# of predicates: {}", context.predicate_count);
    report
}

/// How far the title is indented: centred under the rule it is banners between.
fn title_indent() -> usize {
    REPORT_RULE
        .chars()
        .count()
        .saturating_sub(REPORT_TITLE.chars().count())
        / 2
}

/// One section heading: its name, underlined with as many `=` as the name is long.
fn heading(report: &mut String, name: &str) {
    let _ = writeln!(report, "{name}");
    let _ = writeln!(report, "{}", "=".repeat(name.chars().count()));
}

/// One plan section's body: the Neptune steps the plan is made of, then the steps that are
/// not Neptune steps at all and are left to the step iterators.
fn write_plan(report: &mut String, plan: &ExplainPlan, optimized: bool) {
    let _ = writeln!(report, "Neptune steps:");
    let _ = writeln!(report, "[");
    let entries = plan_entries(plan, optimized);
    if !entries.is_empty() {
        let _ = writeln!(report, "{}", entries.join(",\n"));
    }
    let _ = writeln!(report, "]");

    let left_behind = not_converted(plan, optimized);
    if !left_behind.is_empty() {
        // Each step is named and terminated, so a list of them reads the same however many
        // there are.
        let _ = writeln!(
            report,
            "+ not converted into Neptune steps: {}",
            left_behind
                .iter()
                .map(|step| format!("{step},"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}

/// The Neptune steps one plan section lists, each already indented as a list entry.
///
/// A plan that pushed nothing down has no Neptune step at all: the whole traversal is left
/// to the step iterators. One that did is a graph query over its patterns followed by the
/// converter that hands its matches on — unless the optimized plan answers a terminal count
/// itself, which leaves it nothing to hand on.
fn plan_entries(plan: &ExplainPlan, optimized: bool) -> Vec<String> {
    if plan.patterns.is_empty() {
        return Vec::new();
    }
    if optimized && plan.counts {
        return vec![graph_query_entry(COUNT_STEP, plan, optimized)];
    }
    vec![
        graph_query_entry(
            &format!("NeptuneGraphQueryStep({})", plan.answers),
            plan,
            optimized,
        ),
        format!("{ENTRY_INDENT}{TRAVERSER_CONVERTER_STEP}"),
    ]
}

/// The steps one plan section leaves to the step iterators: the steps that did not go down,
/// minus the terminal count an optimized plan answers inside its graph query.
fn not_converted(plan: &ExplainPlan, optimized: bool) -> &[String] {
    if optimized && plan.counts {
        &[]
    } else {
        &plan.remaining
    }
}

/// One graph-query entry: the step, the join group of the patterns it matches, and the
/// annotation naming what the query binds and the first variable id it does not use.
fn graph_query_entry(step: &str, plan: &ExplainPlan, optimized: bool) -> String {
    let mut entry = String::new();
    let _ = writeln!(entry, "{ENTRY_INDENT}{step} {{");
    let _ = writeln!(entry, "{GROUP_INDENT}JoinGroupNode {{");
    for pattern in &plan.patterns {
        let _ = writeln!(
            entry,
            "{PATTERN_INDENT}{}",
            pattern_node(pattern, optimized)
        );
    }
    let _ = writeln!(
        entry,
        "{GROUP_INDENT}}}, {{path=[{}], maxVarId={}}}",
        plan.path.join(", "),
        plan.max_var_id
    );
    let _ = write!(entry, "{ENTRY_INDENT}}}");
    entry
}

/// One pattern of the plan, as the section printing it states it.
///
/// A pinned term is a FILTER beside the pattern in the converted plan and is folded into
/// the object position in the optimized one; the projection drops the matched label, so it
/// is made `distinct` wherever the label is not pinned to a single term — with the term
/// folded in, each element matches the pattern once and the projection needs no dedup.
fn pattern_node(pattern: &PlanPattern, optimized: bool) -> String {
    let prefix = if pattern.label {
        LABEL_PATTERN_PREFIX
    } else {
        ""
    };
    let object = pattern.object.render();
    let mut matched = object.clone();
    let mut filter = String::new();
    let mut distinct = "";
    match (&pattern.bound, optimized) {
        (Some(term), true) => matched = format!("{object}={term}"),
        (Some(term), false) => {
            filter = format!(" ContainsFilter({object} in ({term})) .");
            distinct = "distinct ";
        }
        (None, _) => {
            if pattern.label && drops_its_object(pattern) {
                distinct = "distinct ";
            }
        }
    }

    let projection = if pattern.projected.is_empty() {
        // Nothing is projected, so there is nothing to make distinct either.
        distinct = "";
        ASK_PROJECTION.to_string()
    } else {
        pattern
            .projected
            .iter()
            .map(|id| format!("?{id}"))
            .collect::<Vec<_>>()
            .join(",")
    };

    format!(
        "PatternNode[{prefix}({}, {}, {matched}, {}) . project {distinct}{projection} .{filter}]",
        pattern.subject.render(),
        pattern.predicate.render(),
        pattern.graph.render(),
    )
}

/// Whether the pattern matches a variable in its object position that it does not project
/// out — the case where one element can match the pattern more than once.
fn drops_its_object(pattern: &PlanPattern) -> bool {
    pattern
        .object
        .variable()
        .is_some_and(|id| !pattern.projected.contains(&id))
}

/// The instant an explain ran, as the report states it: the RFC 3339 UTC form, to the
/// millisecond.
///
/// The calendar is a library's, not this crate's. An instant the formatter cannot render —
/// one outside the years it can print — reports as the empty string rather than failing an
/// explain.
fn explain_timestamp(instant: SystemTime) -> String {
    let rendered = ::time::OffsetDateTime::from(instant).format(
        ::time::macros::format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        ),
    );
    debug_assert!(
        rendered.is_ok(),
        "an instant this emulator reads from the clock must be printable: {rendered:?}"
    );
    rendered.unwrap_or_default()
}
