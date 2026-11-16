// SPDX-License-Identifier: CC0-1.0

//! The plan one openCypher query compiles to, and the explain report this engine answers
//! with for it.
//!
//! The openCypher explain endpoint reports how the engine ANSWERS the submitted query, so
//! every line of the report below is derived from that query and from this engine's own
//! evaluation of it:
//!
//! 1. the operator pipeline the query compiles to — the solutions the pipeline starts
//!    with, one operator per clause the engine evaluates, the merge of the branches of a
//!    `UNION`, and the result the last operator hands back — each printed with the
//!    arguments it was compiled with, read off the query's own syntax tree;
//! 2. in `dynamic` and `details` mode, what each of those operators DID: the solutions
//!    that reached it, the solutions it answered and how long it took. Those are
//!    MEASURED, which is why those two modes evaluate the query and `static` does not;
//! 3. in `details` mode, the query as it was submitted and the cardinalities behind the
//!    plan — how many elements the store really holds for each label and relationship
//!    type the query names.
//!
//! Nothing here is synthesised: a mode that reports no measurement does not evaluate the
//! query, and a mode that reports one evaluates it and reports what it counted.
//!
//! The operation is a READ (`@readonly` in the model), so an explain of a query that
//! WRITES is evaluated against a copy of the graph that is dropped when the report is
//! written: the stored graph a later caller reads is never replaced by an explain.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use ::opencypher::{
    Clause, Direction, Executor, Expr, LabelExpr, NodePattern, PatternPart, Properties, Query,
    RelationshipPattern, ReturnBody, SingleQuery, Visitor,
};
use ::serde_json::Value;

use super::super::property_graph::PropertyGraph;
use super::{
    aggregates, nesting_depth, validated, writes_the_graph, Budget, QueryFailure, QueryRun,
    MAX_QUERY_CHARS, MAX_QUERY_NESTING_DEPTH, MAX_QUERY_WORK_UNITS, QUERY_STACK_BYTES,
};

/// The modes `explainMode` takes.
///
/// Source: smithy_query(query="shape", shape="OpenCypherExplainMode") — the enum's values
/// are `static`, `dynamic` and `details`.
pub const OPEN_CYPHER_EXPLAIN_MODES: [&str; 3] = ["static", "dynamic", "details"];

/// The longest one argument list is printed in the plan, in characters. A longer one is
/// clipped: an argument carries text the query wrote, and the report is a bounded
/// multiple of the query it explains.
const MAX_ARGUMENTS_CHARS: usize = 240;

/// The marker a clipped argument list ends with.
const CLIPPED: &str = "...";

/// The most names the `details` report states a cardinality for.
///
/// Emulator resource guard on the report a small request may answer with: a query writes as
/// many label and relationship-type names as its text has room for, and each one is a line
/// of the report. The names past this ceiling are reported as a count rather than listed,
/// so the section's length is bounded by this rather than by the query.
const MAX_REPORTED_CARDINALITIES: usize = 32;

/// The banner the report opens with.
const REPORT_RULE: &str = "*******************************************************";

/// What an explain reports, which is what the request's `explainMode` chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplainMode {
    /// The operator pipeline alone: the query is read and planned, never evaluated.
    Static,
    /// The pipeline plus what each operator did when the engine evaluated the query.
    Dynamic,
    /// Everything `dynamic` reports, plus the query as submitted and the cardinalities
    /// the plan was built against.
    Details,
}

impl ExplainMode {
    /// The mode `explain` named, or `None` for a value the model does not declare.
    #[must_use]
    pub fn read(mode: &str) -> Option<Self> {
        match mode {
            "static" => Some(Self::Static),
            "dynamic" => Some(Self::Dynamic),
            "details" => Some(Self::Details),
            _ => None,
        }
    }

    /// How the mode is spelled on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
            Self::Details => "details",
        }
    }

    /// Whether the report carries what the engine MEASURED, which is what makes the mode
    /// evaluate the query rather than only plan it.
    #[must_use]
    fn measured(self) -> bool {
        !matches!(self, Self::Static)
    }

    /// Whether the report carries the query as submitted and the cardinalities behind the
    /// plan.
    #[must_use]
    fn detailed(self) -> bool {
        matches!(self, Self::Details)
    }
}

/// What one operator of a plan did when the engine evaluated the query: the solutions
/// that reached it, the solutions it answered, and how long it took.
#[derive(Debug, Clone, Copy, Default)]
pub struct StepRun {
    pub units_in: usize,
    pub units_out: usize,
    pub elapsed: Duration,
}

/// What one branch of a query did: its solution injection, then one entry per clause, in
/// the order the engine evaluated them.
#[derive(Debug, Default)]
pub struct BranchRun {
    pub injection: StepRun,
    pub clauses: Vec<StepRun>,
}

/// What a whole evaluation did, operator by operator.
///
/// Filled in by the evaluation itself ([`QueryRun`]) when a run is traced, which only an
/// explain in a measuring mode asks for: a query answered through the query endpoint is
/// run untraced.
#[derive(Debug, Default)]
pub struct RunTrace {
    /// One per branch: the initial query, then each `UNION` branch in query order.
    pub branches: Vec<BranchRun>,
    /// One per branch merge, in the order the merges happened.
    pub merges: Vec<StepRun>,
    /// Handing back the result: the rows the query answered, as they were rendered.
    pub output: StepRun,
}

impl RunTrace {
    /// Start the trace of one branch with the injection of its first solution.
    pub fn begin_branch(&mut self, injection: StepRun) {
        self.branches.push(BranchRun {
            injection,
            clauses: Vec::new(),
        });
    }

    /// Record what one clause of the branch being traced did.
    pub fn clause(&mut self, run: StepRun) {
        if let Some(branch) = self.branches.last_mut() {
            branch.clauses.push(run);
        }
    }

    /// Record what one merge of two branches did.
    pub fn merge(&mut self, run: StepRun) {
        self.merges.push(run);
    }
}

/// The explain report for `source` in `mode`.
///
/// `graph` is the stored graph: a measuring mode evaluates the query against it (against
/// a COPY of it when the query writes, so the stored one is left as it was), and
/// `details` reports the cardinalities it holds. `deadline` is the instant a measured run
/// must stop at.
///
/// The query is bounded, read and checked exactly as the query endpoint bounds, reads and
/// checks it, so a text that endpoint refuses is refused here for the same reason: an
/// explain of a query that cannot be answered is not a plan.
pub fn explain_query(
    graph: &mut PropertyGraph,
    source: &str,
    parameters: BTreeMap<String, Value>,
    mode: ExplainMode,
    deadline: Instant,
) -> Result<String, QueryFailure> {
    let characters = source.chars().count();
    if characters > MAX_QUERY_CHARS {
        return Err(QueryFailure::TooLong {
            characters,
            ceiling: MAX_QUERY_CHARS,
        });
    }
    let depth = nesting_depth(source);
    if depth > MAX_QUERY_NESTING_DEPTH {
        return Err(QueryFailure::TooDeep {
            depth,
            ceiling: MAX_QUERY_NESTING_DEPTH,
        });
    }

    // The copy a measured run of a WRITING query is evaluated against. It is this
    // function's, never the store's: it is dropped when the report is written, which is
    // what makes an explain a read.
    let mut scratch = PropertyGraph::default();
    ::std::thread::scope(|scope| {
        // Parsing and evaluation both descend through the query, so they run on a thread
        // with a stack sized for the text this engine admits — the same stack the query
        // endpoint gives a run.
        let planned = ::std::thread::Builder::new()
            .stack_size(QUERY_STACK_BYTES)
            .spawn_scoped(scope, || {
                let parsed = ::opencypher::parse(source).map_err(|refused| {
                    QueryFailure::Malformed {
                        reason: super::super::open_cypher_parse_failure(
                            source,
                            &refused.message,
                            refused.span.start..refused.span.end,
                        ),
                    }
                })?;
                validated(&parsed, &parameters)?;
                let measured = if mode.measured() {
                    Some(evaluated(graph, &mut scratch, source, &parsed, &parameters, deadline)?)
                } else {
                    None
                };
                report(source, &parsed, graph, &parameters, mode, measured.as_ref())
            });
        match planned {
            Ok(handle) => handle.join().unwrap_or_else(|_| {
                Err(QueryFailure::Interrupted(
                    "the openCypher engine did not finish explaining this query".to_string(),
                ))
            }),
            Err(refused) => Err(QueryFailure::Interrupted(format!(
                "the openCypher engine could not start a run: {refused}"
            ))),
        }
    })
}

/// Evaluate `parsed` and report what every operator of the pipeline did.
///
/// A query that WRITES is evaluated against `scratch`, a copy of the graph, so what it
/// writes is never stored: this operation reports a plan, it does not change the graph.
fn evaluated(
    graph: &mut PropertyGraph,
    scratch: &mut PropertyGraph,
    source: &str,
    parsed: &Query,
    parameters: &BTreeMap<String, Value>,
    deadline: Instant,
) -> Result<RunTrace, QueryFailure> {
    let writes = writes_the_graph(parsed);
    if writes {
        *scratch = graph.clone();
    }
    let target: &mut PropertyGraph = if writes { scratch } else { graph };
    let mut run = QueryRun {
        graph: target,
        source,
        parameters: parameters.clone(),
        budget: Budget::of(MAX_QUERY_WORK_UNITS, deadline),
        trace: Some(RunTrace::default()),
    };
    let result = run.execute(parsed)?;
    let rows = result.rows.len();
    // Handing the result back is work the engine does for this query too, so the step
    // that does it is timed like every other one.
    let started = Instant::now();
    let rendered = run.rendered(&result)?;
    let elapsed = started.elapsed();
    let mut measured = run.trace.take().unwrap_or_default();
    measured.output = StepRun {
        units_in: rows,
        units_out: rendered.len(),
        elapsed,
    };
    Ok(measured)
}

/// One operator of the plan: what it is, what it was compiled with, which operator its
/// solutions flow to, and what it did when the query was evaluated.
#[derive(Debug)]
struct PlanStep {
    name: &'static str,
    arguments: String,
    /// The id of the operator this one hands its solutions to, `None` for the last.
    out: Option<usize>,
    /// What this operator did, for a mode that measured the run.
    run: Option<StepRun>,
}

/// The operator the pipeline starts with: the one solution that binds nothing, which is
/// what makes a query with no reading clause answer one row.
const SOLUTION_INJECTION: &str = "SolutionInjection";

/// The operator the pipeline ends with: the solutions handed back to the caller.
const SOLUTION_OUTPUT: &str = "SolutionOutput";

/// The plan `query` compiles to, with `measured` attached operator by operator when the
/// run was traced.
fn plan_steps(
    source: &str,
    query: &Query,
    measured: Option<&RunTrace>,
) -> Result<Vec<PlanStep>, QueryFailure> {
    let branches: Vec<&SingleQuery> = ::std::iter::once(&query.initial)
        .chain(query.unions.iter().map(|union| &union.query))
        .collect();
    let mut steps: Vec<PlanStep> = Vec::new();
    // The last operator of each branch, which is what a merge takes its solutions from.
    let mut branch_ends: Vec<usize> = Vec::new();
    for (index, single) in branches.iter().enumerate() {
        let traced = measured.and_then(|trace| trace.branches.get(index));
        steps.push(PlanStep {
            name: SOLUTION_INJECTION,
            arguments: "solutions=[{}]".to_string(),
            out: None,
            run: traced.map(|branch| branch.injection),
        });
        for (ordinal, clause) in single.clauses.iter().enumerate() {
            let (name, arguments) = clause_step(source, clause)?;
            steps.push(PlanStep {
                name,
                arguments: clipped(arguments),
                out: None,
                run: traced
                    .and_then(|branch| branch.clauses.get(ordinal))
                    .copied(),
            });
        }
        branch_ends.push(steps.len().saturating_sub(1));
    }
    // One merge per `UNION`: the branches before it flow into it, and it flows into the
    // next merge, so a query with no union has none of these.
    let mut merges: Vec<usize> = Vec::new();
    for (index, union) in query.unions.iter().enumerate() {
        let name = if union.all { "UnionAll" } else { "Union" };
        steps.push(PlanStep {
            name,
            arguments: format!("all={}", union.all),
            out: None,
            run: measured.and_then(|trace| trace.merges.get(index)).copied(),
        });
        merges.push(steps.len().saturating_sub(1));
    }
    let output = steps.len();
    steps.push(PlanStep {
        name: SOLUTION_OUTPUT,
        arguments: clipped(format!("columns=[{}]", answered_columns(source, query).join(", "))),
        out: None,
        run: measured.map(|trace| trace.output),
    });

    // Where each operator's solutions go: the next operator of its own branch, then the
    // merge that branch feeds, then the next merge, and last the output.
    let mut flows: Vec<Option<usize>> = vec![None; steps.len()];
    for (index, flow) in flows.iter_mut().enumerate().take(output) {
        *flow = Some(index.saturating_add(1));
    }
    for (index, end) in branch_ends.iter().enumerate() {
        // The first two branches of a union feed its first merge; each later branch feeds
        // the merge of its own `UNION`.
        if let Some(flow) = flows.get_mut(*end) {
            *flow = Some(*merges.get(index.saturating_sub(1)).unwrap_or(&output));
        }
    }
    for (index, merge) in merges.iter().enumerate() {
        if let Some(flow) = flows.get_mut(*merge) {
            *flow = Some(*merges.get(index.saturating_add(1)).unwrap_or(&output));
        }
    }
    for (step, flow) in steps.iter_mut().zip(flows) {
        step.out = flow;
    }
    if let Some(last) = steps.last_mut() {
        last.out = None;
    }
    Ok(steps)
}

/// The operator one clause compiles to, and the arguments it is printed with.
///
/// A clause this engine does not evaluate is REFUSED rather than planned: a plan for work
/// the engine cannot do would describe an evaluation that could never happen.
fn clause_step(source: &str, clause: &Clause) -> Result<(&'static str, String), QueryFailure> {
    let step = match clause {
        Clause::Match(matching) => {
            let mut arguments = format!("patterns=[{}]", printed_patterns(&matching.patterns));
            if let Some(filter) = &matching.where_clause {
                let _ = write!(arguments, ", filter={}", printed_expr(source, filter));
            }
            let name = if matching.optional {
                "OptionalPatternMatch"
            } else {
                "PatternMatch"
            };
            (name, arguments)
        }
        Clause::Unwind(unwind) => (
            "Unwind",
            format!(
                "list={}, as={}",
                printed_expr(source, &unwind.expr),
                unwind.variable.0
            ),
        ),
        Clause::With(with) => (
            projection_name(&with.body),
            projection_arguments(source, &with.body, with.distinct, with.where_clause.as_ref()),
        ),
        Clause::Return(returning) => (
            projection_name(&returning.body),
            projection_arguments(source, &returning.body, returning.distinct, None),
        ),
        Clause::Create(create) => (
            "Create",
            format!("patterns=[{}]", printed_patterns(&create.patterns)),
        ),
        Clause::Merge(merge) => {
            let mut arguments = format!("pattern={}", printed_pattern(&merge.pattern));
            for action in &merge.actions {
                let branch = match action.on {
                    ::opencypher::MergeOn::Create => "onCreate",
                    ::opencypher::MergeOn::Match => "onMatch",
                };
                let _ = write!(arguments, ", {branch}={}", action.set.len());
            }
            ("Merge", arguments)
        }
        Clause::Set(set) => ("SetProperties", format!("items={}", set.items.len())),
        Clause::Remove(remove) => ("RemoveProperties", format!("items={}", remove.items.len())),
        Clause::Delete(delete) => (
            "Delete",
            format!(
                "targets=[{}], detach={}",
                delete
                    .targets
                    .iter()
                    .map(|target| printed_expr(source, target))
                    .collect::<Vec<_>>()
                    .join(", "),
                delete.detach
            ),
        ),
        Clause::Call(_) => return Err(QueryFailure::Unsupported { clause: "CALL" }),
        Clause::Foreach(_) => return Err(QueryFailure::Unsupported { clause: "FOREACH" }),
        Clause::LoadCsv(_) => return Err(QueryFailure::Unsupported { clause: "LOAD CSV" }),
        _ => {
            return Err(QueryFailure::Unsupported {
                clause: "this clause",
            })
        }
    };
    Ok((step.0, step.1))
}

/// The operator a projection compiles to: one that GROUPS its rows when any item of it
/// aggregates, and one that projects them row by row otherwise.
fn projection_name(body: &ReturnBody) -> &'static str {
    if body.items.iter().any(|item| aggregates(&item.expr)) {
        "Aggregation"
    } else {
        "Projection"
    }
}

/// The arguments a projection is printed with: the columns it answers, and each modifier
/// the query wrote on it.
fn projection_arguments(
    source: &str,
    body: &ReturnBody,
    distinct: bool,
    filter: Option<&Expr>,
) -> String {
    let mut arguments = format!("vars=[{}]", projected_columns(source, body).join(", "));
    if distinct {
        let _ = write!(arguments, ", distinct=true");
    }
    if !body.order_by.is_empty() {
        let printed: Vec<String> = body
            .order_by
            .iter()
            .map(|sort| {
                format!(
                    "{} {}",
                    printed_expr(source, &sort.expr),
                    if sort.descending { "DESC" } else { "ASC" }
                )
            })
            .collect();
        let _ = write!(arguments, ", order=[{}]", printed.join(", "));
    }
    if let Some(skip) = &body.skip {
        let _ = write!(arguments, ", skip={}", printed_expr(source, skip));
    }
    if let Some(limit) = &body.limit {
        let _ = write!(arguments, ", limit={}", printed_expr(source, limit));
    }
    if let Some(filter) = filter {
        let _ = write!(arguments, ", filter={}", printed_expr(source, filter));
    }
    arguments
}

/// The columns one projection answers: each item's alias, or the text the query wrote the
/// expression with, which is what openCypher names an unaliased column by.
fn projected_columns(source: &str, body: &ReturnBody) -> Vec<String> {
    let mut columns = Vec::new();
    if body.star {
        columns.push("*".to_string());
    }
    for item in &body.items {
        columns.push(match &item.alias {
            Some(alias) => alias.0.clone(),
            None => printed_expr(source, &item.expr),
        });
    }
    columns
}

/// The columns the whole query answers: the ones its last projection names, and none when
/// the query projects nothing (a write-only query answers no rows).
fn answered_columns(source: &str, query: &Query) -> Vec<String> {
    let last = ::std::iter::once(&query.initial)
        .chain(query.unions.iter().map(|union| &union.query))
        .flat_map(|single| single.clauses.iter())
        .filter_map(|clause| match clause {
            Clause::Return(returning) => Some(&returning.body),
            _ => None,
        })
        .next_back();
    last.map(|body| projected_columns(source, body))
        .unwrap_or_default()
}

/// One expression as the query wrote it, which is the text its own span covers.
fn printed_expr(source: &str, expr: &Expr) -> String {
    let written = expr.span.slice(source).trim();
    if written.is_empty() {
        "expression".to_string()
    } else {
        written.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// A pattern list as the plan prints it.
fn printed_patterns(patterns: &[PatternPart]) -> String {
    patterns
        .iter()
        .map(printed_pattern)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One pattern part: the variable the whole path binds, then its nodes and the hops
/// between them.
fn printed_pattern(part: &PatternPart) -> String {
    let mut text = String::new();
    if let Some(variable) = &part.variable {
        let _ = write!(text, "{}=", variable.0);
    }
    text.push_str(&printed_node(&part.element.start));
    for (relationship, node) in &part.element.chain {
        text.push_str(&printed_relationship(relationship));
        text.push_str(&printed_node(node));
    }
    text
}

/// One node pattern: its variable, its labels and whether it asks for properties.
fn printed_node(node: &NodePattern) -> String {
    let mut inside = String::new();
    if let Some(variable) = &node.variable {
        inside.push_str(variable.0.as_str());
    }
    if let Some(labels) = &node.labels {
        let _ = write!(inside, ":{}", printed_labels(labels));
    }
    if let Some(properties) = &node.properties {
        let _ = write!(inside, " {}", printed_properties(properties));
    }
    format!("({inside})")
}

/// One hop: its variable, its types, the range it walks, and the direction it is written
/// in.
fn printed_relationship(relationship: &RelationshipPattern) -> String {
    let mut inside = String::new();
    if let Some(variable) = &relationship.variable {
        inside.push_str(variable.0.as_str());
    }
    if let Some(types) = &relationship.types {
        let _ = write!(inside, ":{}", printed_labels(types));
    }
    if let Some(range) = &relationship.range {
        inside.push('*');
        if let Some(minimum) = range.min {
            let _ = write!(inside, "{minimum}");
        }
        inside.push_str("..");
        if let Some(maximum) = range.max {
            let _ = write!(inside, "{maximum}");
        }
    }
    if let Some(properties) = &relationship.properties {
        let _ = write!(inside, " {}", printed_properties(properties));
    }
    let hop = if inside.is_empty() {
        String::new()
    } else {
        format!("[{inside}]")
    };
    match relationship.direction {
        Direction::Outgoing => format!("-{hop}->"),
        Direction::Incoming => format!("<-{hop}-"),
        _ => format!("-{hop}-"),
    }
}

/// A label expression as the query wrote it: a name, a wildcard, or the operators that
/// join them.
fn printed_labels(labels: &LabelExpr) -> String {
    match labels {
        LabelExpr::Leaf(name) => name.clone(),
        LabelExpr::Wildcard => "%".to_string(),
        LabelExpr::Negation(inner) => format!("!{}", printed_labels(inner)),
        LabelExpr::Conjunction(one, other) => {
            format!("{}&{}", printed_labels(one), printed_labels(other))
        }
        LabelExpr::Disjunction(one, other) => {
            format!("{}|{}", printed_labels(one), printed_labels(other))
        }
        _ => "?".to_string(),
    }
}

/// The properties a pattern asks for: the keys it names, or the parameter the whole map
/// comes from. The VALUES are the caller's own data and the plan does not print them.
fn printed_properties(properties: &Properties) -> String {
    match properties {
        Properties::Map(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, _)| key.0.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Properties::Param(name) => format!("${name}"),
    }
}

/// `arguments`, clipped to what one plan row prints.
fn clipped(arguments: String) -> String {
    if arguments.chars().count() <= MAX_ARGUMENTS_CHARS {
        return arguments;
    }
    let kept: String = arguments.chars().take(MAX_ARGUMENTS_CHARS).collect();
    format!("{kept}{CLIPPED}")
}

/// The whole report: the banner, the mode, the plan, and what `details` adds to it.
fn report(
    source: &str,
    query: &Query,
    graph: &PropertyGraph,
    parameters: &BTreeMap<String, Value>,
    mode: ExplainMode,
    measured: Option<&RunTrace>,
) -> Result<String, QueryFailure> {
    let steps = plan_steps(source, query, measured)?;
    let mut report = String::new();
    let _ = writeln!(report, "{REPORT_RULE}");
    let _ = writeln!(report, "Neptune openCypher Explain");
    let _ = writeln!(report, "{REPORT_RULE}");
    let _ = writeln!(report);
    let _ = writeln!(report, "Explain Mode");
    let _ = writeln!(report, "============");
    let _ = writeln!(report, "{}", mode.as_str());
    let _ = writeln!(report);
    if mode.detailed() {
        let _ = writeln!(report, "Query String");
        let _ = writeln!(report, "============");
        let _ = writeln!(report, "{}", source.trim());
        let _ = writeln!(report);
        let _ = writeln!(report, "Parameters");
        let _ = writeln!(report, "==========");
        let _ = writeln!(
            report,
            "[{}]",
            parameters
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = writeln!(report);
    }
    let _ = writeln!(report, "Plan");
    let _ = writeln!(report, "====");
    report.push_str(&plan_table(&steps, mode.measured()));
    if mode.detailed() {
        let _ = writeln!(report);
        let _ = writeln!(report, "Pattern Cardinality");
        let _ = writeln!(report, "===================");
        let (counted, clipped) = cardinalities(query, graph);
        for (pattern, held) in counted {
            let _ = writeln!(report, "{pattern} => {held}");
        }
        if clipped > 0 {
            // Stated rather than dropped silently: the section reports as many names as one
            // report carries, and the caller is told how many of its own it did not reach.
            let _ = writeln!(
                report,
                "({clipped} further name(s) the query names are not reported here)"
            );
        }
    }
    Ok(report)
}

/// The plan as a table: one row per operator, with the measured columns when the run was
/// traced.
fn plan_table(steps: &[PlanStep], measured: bool) -> String {
    let mut headers = vec!["ID", "Out #1", "Name", "Arguments"];
    if measured {
        headers.extend(["Units In", "Units Out", "Ratio", "Time (ms)"]);
    }
    let rows: Vec<Vec<String>> = steps
        .iter()
        .enumerate()
        .map(|(id, step)| {
            let mut row = vec![
                id.to_string(),
                step.out.map(|out| out.to_string()).unwrap_or_default(),
                step.name.to_string(),
                step.arguments.clone(),
            ];
            if measured {
                let run = step.run.unwrap_or_default();
                row.push(run.units_in.to_string());
                row.push(run.units_out.to_string());
                row.push(ratio(run.units_in, run.units_out));
                row.push(format!("{:.2}", run.elapsed.as_secs_f64() * 1_000.0));
            }
            row
        })
        .collect();
    rendered_table(&headers, &rows)
}

/// The solutions one operator answered per solution it was handed, which is what a plan
/// reports as the operator's ratio. An operator nothing reached has no ratio to report.
fn ratio(units_in: usize, units_out: usize) -> String {
    #[allow(clippy::cast_precision_loss)]
    let ratio = if units_in == 0 {
        0.0
    } else {
        units_out as f64 / units_in as f64
    };
    format!("{ratio:.2}")
}

/// `rows` under `headers`, each column as wide as its widest cell.
fn rendered_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(column, header)| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.chars().count())
                .chain(::std::iter::once(header.chars().count()))
                .max()
                .unwrap_or_default()
        })
        .collect();
    let rule = |left: char, join: char, right: char| {
        let mut line = String::new();
        line.push(left);
        for (column, width) in widths.iter().enumerate() {
            if column > 0 {
                line.push(join);
            }
            line.push_str(&"─".repeat(width.saturating_add(2)));
        }
        line.push(right);
        line.push('\n');
        line
    };
    let printed_row = |cells: &[String]| {
        let mut line = String::new();
        line.push('│');
        for (column, width) in widths.iter().enumerate() {
            let cell = cells.get(column).map(String::as_str).unwrap_or_default();
            let padding = width.saturating_sub(cell.chars().count());
            let _ = write!(line, " {cell}{} │", " ".repeat(padding));
        }
        line.push('\n');
        line
    };

    let mut table = rule('┌', '┬', '┐');
    table.push_str(&printed_row(
        &headers.iter().map(|header| (*header).to_string()).collect::<Vec<_>>(),
    ));
    table.push_str(&rule('├', '┼', '┤'));
    for row in rows {
        table.push_str(&printed_row(row));
    }
    table.push_str(&rule('└', '┴', '┘'));
    table
}

/// How many elements the store HOLDS for each thing the query names, plus its totals, and
/// how many further names the section did not report.
///
/// Every number is counted off the stored graph, so a `details` report states the
/// cardinalities the plan was built against rather than an estimate of them. The counting
/// takes ONE pass over the store per kind of element and then LOOKS each name up, so the
/// work is bounded by what the store holds ([`PropertyGraph::label_counts`]) and not by how
/// many names one query writes; the names reported are bounded in their own right
/// ([`MAX_REPORTED_CARDINALITIES`]), which is what bounds this section of the report.
fn cardinalities(query: &Query, graph: &PropertyGraph) -> (Vec<(String, usize)>, usize) {
    /// The labels and relationship types the query's patterns name.
    #[derive(Default)]
    struct Named {
        labels: BTreeSet<String>,
        types: BTreeSet<String>,
    }
    impl Named {
        fn leaves(labels: &LabelExpr, into: &mut BTreeSet<String>) {
            match labels {
                LabelExpr::Leaf(name) => {
                    into.insert(name.clone());
                }
                LabelExpr::Negation(inner) => Self::leaves(inner, into),
                LabelExpr::Conjunction(one, other) | LabelExpr::Disjunction(one, other) => {
                    Self::leaves(one, into);
                    Self::leaves(other, into);
                }
                _ => {}
            }
        }
    }
    impl Visitor for Named {
        fn visit_node_pattern(&mut self, node: &NodePattern) {
            if let Some(labels) = &node.labels {
                Self::leaves(labels, &mut self.labels);
            }
            ::opencypher::walk_node_pattern(self, node);
        }

        fn visit_relationship_pattern(&mut self, relationship: &RelationshipPattern) {
            if let Some(types) = &relationship.types {
                Self::leaves(types, &mut self.types);
            }
            ::opencypher::walk_relationship_pattern(self, relationship);
        }
    }
    let mut named = Named::default();
    named.visit_query(query);

    let mut counted = vec![
        ("(all nodes)".to_string(), graph.node_count()),
        (
            "(all relationships)".to_string(),
            graph.relationship_count(),
        ),
    ];
    // A name the store holds no element for is absent from the counts, which is the count
    // zero: the query named it, so the section reports it as zero rather than omitting it.
    let labels = graph.label_counts();
    let types = graph.relationship_type_counts();
    let reported = named.labels.len().saturating_add(named.types.len());
    for label in named.labels.iter().take(MAX_REPORTED_CARDINALITIES) {
        counted.push((
            format!("(:{label})"),
            labels.get(label).copied().unwrap_or_default(),
        ));
    }
    let left = MAX_REPORTED_CARDINALITIES.saturating_sub(named.labels.len());
    for rel_type in named.types.iter().take(left) {
        counted.push((
            format!("[:{rel_type}]"),
            types.get(rel_type).copied().unwrap_or_default(),
        ));
    }
    (
        counted,
        reported.saturating_sub(MAX_REPORTED_CARDINALITIES),
    )
}
