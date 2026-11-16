// SPDX-License-Identifier: CC0-1.0

//! `ExecuteOpenCypherExplainQuery` end to end over HTTP.
//!
//! The rule under test: an explain reports the PLAN this engine answers the SUBMITTED
//! query with, in the mode the request named. The plan is the operator pipeline the query
//! compiles to — the solution the pipeline starts from, one operator per clause the engine
//! evaluates, the merge of the branches of a `UNION`, and the result handed back — printed
//! with the arguments read off that query. `static` reports the pipeline alone; `dynamic`
//! and `details` evaluate the query and report what each operator DID; `details` also
//! reports the query as submitted and the cardinalities the store holds. And the operation
//! is a read: an explain leaves the stored graph and the query registry as they were.
//!
//! Each suite drives the rule over a SET of inputs: the recorded shape (explain one read
//! query) is one row of the table, and the rows differ along what the rule generalises
//! over — which clauses a query is made of, how many of them there are, their order,
//! whether it writes, whether it unions, and which mode was asked for.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted graph and serialization — the same path a client takes.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::state::{
    ComAmazonawsNeptunedataAmazonneptunedataplaneState, OPEN_CYPHER_EXPLAIN_MODES,
};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service. `neptune-db` is neptunedata's sigv4
/// signing name, and it is what routes a request to this service.
const SIGNED_FOR_NEPTUNE_DB: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/neptune-db/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The title the explain report is headed with, which is also what tells the report apart
/// from a JSON error body on the wire.
const REPORT_TITLE: &str = "Neptune openCypher Explain";

/// The operator the pipeline starts from, and the one it ends with.
const SOLUTION_INJECTION: &str = "SolutionInjection";
const SOLUTION_OUTPUT: &str = "SolutionOutput";

/// The columns a plan carries only in a mode that MEASURED the run.
const MEASURED_COLUMNS: &[&str] = &["Units In", "Units Out", "Ratio", "Time (ms)"];

/// One emulator: a router carrying only this service, over one shared engine. The engine
/// is handed back so a test can read the state the handlers write.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    Emulator { router, engine }
}

/// What the emulator answered: the status and the raw body, which on a success is the
/// report text rather than JSON.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The wire error code (`__type`) without its shape namespace, or the empty string
    /// when the body is not a modelled error.
    fn error_code(&self) -> String {
        serde_json::from_slice::<Value>(&self.body)
            .ok()
            .and_then(|body| {
                body["__type"]
                    .as_str()
                    .and_then(|code| code.rsplit('#').next())
                    .map(str::to_owned)
            })
            .unwrap_or_default()
    }

    fn detailed_message(&self) -> String {
        serde_json::from_slice::<Value>(&self.body)
            .ok()
            .and_then(|body| body["detailedMessage"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// The `message` a refusal the request walk answers carries.
    fn wire_message(&self) -> String {
        serde_json::from_slice::<Value>(&self.body)
            .ok()
            .and_then(|body| body["message"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Value) -> Answer {
    let payload = body.to_string();
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "neptune-db.us-east-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_NEPTUNE_DB)
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    Answer {
        status,
        body: response.into_body().to_vec(),
    }
}

/// `ExecuteOpenCypherExplainQuery` is `POST /opencypher/explain`, with the query in the
/// body member `query` and the mode in `explain`.
/// Source: smithy_query(query="operation", operation="ExecuteOpenCypherExplainQuery") —
/// the http binding and the `jsonName` of `openCypherQuery` and `explainMode`.
async fn explain(router: &MultiServiceRouter, query: &str, mode: &str) -> Answer {
    call(
        router,
        "POST",
        "/opencypher/explain",
        json!({ "query": query, "explain": mode }),
    )
    .await
}

async fn explain_with(
    router: &MultiServiceRouter,
    query: &str,
    mode: &str,
    parameters: &str,
) -> Answer {
    call(
        router,
        "POST",
        "/opencypher/explain",
        json!({ "query": query, "explain": mode, "parameters": parameters }),
    )
    .await
}

/// `ExecuteOpenCypherQuery` is `POST /opencypher`: how a test writes the graph an explain
/// is taken against, and how it reads back what an explain left.
async fn query(router: &MultiServiceRouter, text: &str) -> Answer {
    call(router, "POST", "/opencypher", json!({ "query": text })).await
}

/// The rows a query answered, through the query endpoint.
async fn answered(router: &MultiServiceRouter, case: &str, text: &str) -> Vec<Value> {
    let answer = query(router, text).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: {text:?} must be answered, got {} with {:?}",
        answer.status,
        answer.text()
    );
    serde_json::from_slice::<Value>(&answer.body)
        .ok()
        .and_then(|body| body["results"].as_array().cloned())
        .unwrap_or_default()
}

/// An explain that must be served, with its report.
async fn report(router: &MultiServiceRouter, case: &str, text: &str, mode: &str) -> String {
    let answer = explain(router, text, mode).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?} in {mode:?}: the explain must be served, got {} with {:?}",
        answer.status,
        answer.text()
    );
    let report = answer.text();
    assert!(
        report.contains(REPORT_TITLE),
        "case {case:?} in {mode:?}: the report must be headed {REPORT_TITLE:?}, got {report}"
    );
    report
}

/// The cells of every row of the plan table, the header row first.
fn table_rows(report: &str) -> Vec<Vec<String>> {
    report
        .lines()
        .filter(|line| line.starts_with('│'))
        .map(|line| {
            line.trim_matches('│')
                .split('│')
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

/// The column names the plan table carries.
fn columns(report: &str) -> Vec<String> {
    table_rows(report).first().cloned().unwrap_or_default()
}

/// One operator row: the cells of the plan's body rows, by column name.
fn operator_rows(report: &str) -> Vec<Vec<String>> {
    table_rows(report).into_iter().skip(1).collect()
}

/// The value of one column of one operator row.
fn cell(report: &str, row: usize, column: &str) -> String {
    let index = columns(report)
        .iter()
        .position(|held| held == column)
        .unwrap_or_else(|| panic!("the plan must carry a {column:?} column, got {report}"));
    operator_rows(report)
        .get(row)
        .and_then(|cells| cells.get(index).cloned())
        .unwrap_or_default()
}

/// The operators the plan lists, in the order the pipeline runs them.
fn operators(report: &str) -> Vec<String> {
    operator_rows(report)
        .into_iter()
        .map(|cells| cells.get(2).cloned().unwrap_or_default())
        .collect()
}

/// The arguments the plan printed one operator with.
fn arguments(report: &str) -> Vec<String> {
    operator_rows(report)
        .into_iter()
        .map(|cells| cells.get(3).cloned().unwrap_or_default())
        .collect()
}

/// One query and the pipeline it compiles to.
///
/// The rows differ along what the plan is read off: which clauses the query is made of,
/// how many there are, their order, whether the query writes, and whether it unions —
/// everything "the plan is the pipeline of THIS query" generalises over.
#[derive(Debug, Clone, Copy)]
struct Planned {
    case: &'static str,
    query: &'static str,
    /// The operators the plan must list, in order.
    operators: &'static [&'static str],
    /// Something the plan must have read off this query and printed in its arguments.
    argument: &'static str,
}

const PLANS: &[Planned] = &[
    Planned {
        case: "a read of every node",
        query: "MATCH (n) RETURN n",
        operators: &[SOLUTION_INJECTION, "PatternMatch", "Projection", SOLUTION_OUTPUT],
        argument: "patterns=[(n)]",
    },
    Planned {
        case: "a filtered, ordered and paged read",
        query: "MATCH (p:Person) WHERE p.age > 20 RETURN p.name AS name ORDER BY name DESC \
                SKIP 1 LIMIT 2",
        operators: &[SOLUTION_INJECTION, "PatternMatch", "Projection", SOLUTION_OUTPUT],
        argument: "filter=p.age > 20",
    },
    Planned {
        case: "a read that walks a typed hop",
        query: "MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN a, b",
        operators: &[SOLUTION_INJECTION, "PatternMatch", "Projection", SOLUTION_OUTPUT],
        argument: "patterns=[(a:Person)-[r:KNOWS]->(b:Person)]",
    },
    Planned {
        case: "a read that walks a variable-length hop",
        query: "MATCH (a:Person)-[:KNOWS*1..2]->(b) RETURN b",
        operators: &[SOLUTION_INJECTION, "PatternMatch", "Projection", SOLUTION_OUTPUT],
        argument: "[:KNOWS*1..2]",
    },
    Planned {
        case: "an aggregating projection",
        query: "MATCH (n) RETURN count(n) AS total",
        operators: &[SOLUTION_INJECTION, "PatternMatch", "Aggregation", SOLUTION_OUTPUT],
        argument: "vars=[total]",
    },
    Planned {
        case: "a pipeline of two reads joined by a WITH",
        query: "MATCH (a:Person) WITH a LIMIT 1 MATCH (a)-[:KNOWS]->(b) RETURN b",
        operators: &[
            SOLUTION_INJECTION,
            "PatternMatch",
            "Projection",
            "PatternMatch",
            "Projection",
            SOLUTION_OUTPUT,
        ],
        argument: "limit=1",
    },
    Planned {
        case: "an unwind of a list the query wrote",
        query: "UNWIND [1, 2, 3] AS x RETURN x",
        operators: &[SOLUTION_INJECTION, "Unwind", "Projection", SOLUTION_OUTPUT],
        argument: "as=x",
    },
    Planned {
        case: "an optional match beside a match",
        query: "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(b) RETURN a, b",
        operators: &[
            SOLUTION_INJECTION,
            "PatternMatch",
            "OptionalPatternMatch",
            "Projection",
            SOLUTION_OUTPUT,
        ],
        argument: "patterns=[(a)-[:KNOWS]->(b)]",
    },
    Planned {
        case: "two branches merged by a UNION",
        query: "MATCH (p:Person) RETURN p.name AS name UNION MATCH (c:City) RETURN c.name AS name",
        operators: &[
            SOLUTION_INJECTION,
            "PatternMatch",
            "Projection",
            SOLUTION_INJECTION,
            "PatternMatch",
            "Projection",
            "Union",
            SOLUTION_OUTPUT,
        ],
        argument: "all=false",
    },
    Planned {
        case: "two branches merged by a UNION ALL",
        query: "MATCH (p:Person) RETURN p.name AS name UNION ALL MATCH (c:City) \
                RETURN c.name AS name",
        operators: &[
            SOLUTION_INJECTION,
            "PatternMatch",
            "Projection",
            SOLUTION_INJECTION,
            "PatternMatch",
            "Projection",
            "UnionAll",
            SOLUTION_OUTPUT,
        ],
        argument: "all=true",
    },
    Planned {
        case: "a query that creates",
        query: "CREATE (n:Robot {name: 'Bender'}) RETURN n",
        operators: &[SOLUTION_INJECTION, "Create", "Projection", SOLUTION_OUTPUT],
        argument: "patterns=[(n:Robot {name})]",
    },
    Planned {
        case: "a query that merges with both actions",
        query: "MERGE (p:Person {name: 'Alice'}) ON CREATE SET p.made = 1 \
                ON MATCH SET p.seen = 1 RETURN p",
        operators: &[SOLUTION_INJECTION, "Merge", "Projection", SOLUTION_OUTPUT],
        argument: "onCreate=1",
    },
    Planned {
        case: "a query that sets and then removes a property",
        query: "MATCH (p:Person) SET p.flag = 1 REMOVE p.flag",
        operators: &[
            SOLUTION_INJECTION,
            "PatternMatch",
            "SetProperties",
            "RemoveProperties",
            SOLUTION_OUTPUT,
        ],
        argument: "items=1",
    },
    Planned {
        case: "a query that detaches and deletes",
        query: "MATCH (g:Ghost) DETACH DELETE g",
        operators: &[SOLUTION_INJECTION, "PatternMatch", "Delete", SOLUTION_OUTPUT],
        argument: "detach=true",
    },
];

/// The graph every suite that needs data to measure against is taken on: three people,
/// two cities, and one relationship between two of the people.
async fn seeded(router: &MultiServiceRouter) {
    for text in [
        "CREATE (:Person {name: 'Alice', age: 30})",
        "CREATE (:Person {name: 'Bob', age: 24})",
        "CREATE (:Person {name: 'Cleo', age: 18})",
        "CREATE (:City {name: 'Anchorage'})",
        "CREATE (:City {name: 'Boston'})",
        "MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}) CREATE (a)-[:KNOWS]->(b)",
    ] {
        answered(router, "seed", text).await;
    }
}

/// The plan an explain reports is the pipeline the SUBMITTED query compiles to, in every
/// mode: the operators and their arguments are read off the query, so the mode changes
/// what is MEASURED and never what the pipeline is.
#[tokio::test]
async fn the_plan_is_the_pipeline_of_the_submitted_query_in_every_mode() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;

    for case in PLANS {
        for mode in OPEN_CYPHER_EXPLAIN_MODES {
            let report = report(&router, case.case, case.query, mode).await;
            let found = operators(&report);
            assert_eq!(
                found,
                case.operators
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>(),
                "case {:?} in {mode:?}: expected the pipeline {:?}, got {found:?} from\n{report}",
                case.case,
                case.operators
            );
            let printed = arguments(&report);
            assert!(
                printed
                    .iter()
                    .any(|argument| argument.contains(case.argument)),
                "case {:?} in {mode:?}: expected an argument carrying {:?}, got {printed:?} \
                 from\n{report}",
                case.case,
                case.argument
            );
        }
    }
}

/// Every operator's solutions flow to the next one, and the last operator hands them to
/// the caller: an `Out #1` names a row of this plan, and only the output names none.
#[tokio::test]
async fn every_operator_hands_its_solutions_to_a_later_one() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;

    for case in PLANS {
        let report = report(&router, case.case, case.query, "static").await;
        let rows = operator_rows(&report);
        for (id, _) in rows.iter().enumerate() {
            let out = cell(&report, id, "Out #1");
            if id == rows.len() - 1 {
                assert!(
                    out.is_empty(),
                    "case {:?}: the last operator hands its solutions to the caller, \
                     so it names no output, got {out:?} from\n{report}",
                    case.case
                );
                continue;
            }
            let named: usize = out.parse().unwrap_or_else(|_| {
                panic!(
                    "case {:?}: operator {id} must name the operator it feeds, got {out:?} \
                     from\n{report}",
                    case.case
                )
            });
            assert!(
                named > id && named < rows.len(),
                "case {:?}: operator {id} feeds {named}, which is not a later row of a plan \
                 of {} rows:\n{report}",
                case.case,
                rows.len()
            );
        }
    }
}

/// `static` measures nothing, so it reports no measurement; `dynamic` and `details`
/// evaluate the query, so every measured column is there.
#[tokio::test]
async fn only_a_measuring_mode_reports_measurements() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;
    let text = "MATCH (p:Person) RETURN p.name AS name";

    let fixed = report(&router, "a read", text, "static").await;
    for column in MEASURED_COLUMNS {
        assert!(
            !columns(&fixed).iter().any(|held| held == column),
            "static measures nothing, so it must not report {column:?}, got {:?} from\n{fixed}",
            columns(&fixed)
        );
    }

    for mode in ["dynamic", "details"] {
        let measured = report(&router, "a read", text, mode).await;
        for column in MEASURED_COLUMNS {
            assert!(
                columns(&measured).iter().any(|held| held == column),
                "{mode:?} evaluates the query, so it must report {column:?}, got {:?} \
                 from\n{measured}",
                columns(&measured)
            );
        }
    }
}

/// One query, the solutions its last operator must have answered over the seeded graph.
///
/// The rows differ along how many solutions the pipeline really produces — a label that
/// matches some nodes, one that matches none, every node, a paged read and a list the
/// query itself wrote — so a report that did not COUNT what the evaluation did cannot
/// satisfy the whole table.
#[derive(Debug, Clone, Copy)]
struct Measured {
    case: &'static str,
    query: &'static str,
    solutions: usize,
}

const MEASUREMENTS: &[Measured] = &[
    Measured {
        case: "every node of one label",
        query: "MATCH (p:Person) RETURN p",
        solutions: 3,
    },
    Measured {
        case: "every node of another label",
        query: "MATCH (c:City) RETURN c",
        solutions: 2,
    },
    Measured {
        case: "a label the graph holds no node of",
        query: "MATCH (g:Ghost) RETURN g",
        solutions: 0,
    },
    Measured {
        case: "every node",
        query: "MATCH (n) RETURN n",
        solutions: 5,
    },
    Measured {
        case: "a paged read",
        query: "MATCH (p:Person) RETURN p LIMIT 2",
        solutions: 2,
    },
    Measured {
        case: "a filtered read",
        query: "MATCH (p:Person) WHERE p.age > 20 RETURN p",
        solutions: 2,
    },
    Measured {
        case: "a hop the graph holds one of",
        query: "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a, b",
        solutions: 1,
    },
    Measured {
        case: "an aggregate over every node",
        query: "MATCH (n) RETURN count(n) AS total",
        solutions: 1,
    },
    Measured {
        case: "a list the query wrote",
        query: "UNWIND [1, 2, 3, 4] AS x RETURN x",
        solutions: 4,
    },
];

/// A measuring mode reports what the evaluation COUNTED: the solutions the output handed
/// back are the rows the same query answers through the query endpoint, and the pipeline
/// starts from the one solution that binds nothing.
#[tokio::test]
async fn a_measuring_mode_reports_the_solutions_the_evaluation_counted() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;

    for case in MEASUREMENTS {
        let rows = answered(&router, case.case, case.query).await;
        assert_eq!(
            rows.len(),
            case.solutions,
            "case {:?}: the query endpoint must answer {} rows, got {rows:?}",
            case.case,
            case.solutions
        );
        for mode in ["dynamic", "details"] {
            let report = report(&router, case.case, case.query, mode).await;
            let last = operator_rows(&report).len() - 1;
            assert_eq!(
                cell(&report, last, "Units Out"),
                case.solutions.to_string(),
                "case {:?} in {mode:?}: the output must report the {} solutions the \
                 evaluation answered, got {:?} from\n{report}",
                case.case,
                case.solutions,
                cell(&report, last, "Units Out")
            );
            assert_eq!(
                cell(&report, 0, "Units Out"),
                "1",
                "case {:?} in {mode:?}: the pipeline starts from one solution, got {:?} \
                 from\n{report}",
                case.case,
                cell(&report, 0, "Units Out")
            );
        }
    }
}

/// `details` reports the query as submitted and the cardinalities the STORE holds for what
/// the query names, so the numbers follow the graph rather than the request.
#[tokio::test]
async fn details_reports_the_query_and_the_stored_cardinalities() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;
    let text = "MATCH (p:Person)-[:KNOWS]->(c:Person) RETURN p, c";

    let report = report(&router, "a hop between two labelled nodes", text, "details").await;
    assert!(
        report.contains(text),
        "details must report the query as submitted, got\n{report}"
    );
    for expected in [
        "(all nodes) => 5",
        "(all relationships) => 1",
        "(:Person) => 3",
        "[:KNOWS] => 1",
    ] {
        assert!(
            report.contains(expected),
            "details must report {expected:?} off the stored graph, got\n{report}"
        );
    }

    // The cardinalities follow the store: one more node of a named label, one more in the
    // report.
    answered(&router, "another person", "CREATE (:Person {name: 'Dina'})").await;
    let again = report_of(&router, text, "details").await;
    for expected in ["(all nodes) => 6", "(:Person) => 4"] {
        assert!(
            again.contains(expected),
            "after a write the cardinalities must follow the store and report {expected:?}, \
             got\n{again}"
        );
    }

    // A mode that is not `details` reports neither.
    for mode in ["static", "dynamic"] {
        let plain = report_of(&router, text, mode).await;
        assert!(
            !plain.contains("Pattern Cardinality"),
            "{mode:?} reports no cardinalities, got\n{plain}"
        );
    }
}

/// An explain, with its report, for a query that must be served.
async fn report_of(router: &MultiServiceRouter, text: &str, mode: &str) -> String {
    report(router, "an explain", text, mode).await
}

/// The cardinality section is bounded by the report rather than by the query: a query may
/// name as many labels as its text has room for, and the names past the ceiling are
/// reported as a count rather than listed.
#[tokio::test]
async fn the_cardinality_section_reports_at_most_the_names_one_report_carries() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;

    // Two queries either side of the ceiling: one naming fewer labels than it, one naming
    // more. None of these labels is in the store, so each is a real count of zero.
    for (named, listed, clipped) in [(8_usize, 8_usize, 0_usize), (40, 32, 8)] {
        let patterns: Vec<String> = (0..named)
            .map(|index| format!("(n{index}:Absent{index})"))
            .collect();
        let text = format!("MATCH {} RETURN n0", patterns.join(", "));
        let report = report_of(&router, &text, "details").await;
        let reported = report
            .lines()
            .filter(|line| line.starts_with("(:Absent"))
            .count();
        assert_eq!(
            reported, listed,
            "case {named} labels: expected {listed} reported cardinalities, got {reported} \
             from\n{report}"
        );
        let overflow = format!("({clipped} further name(s)");
        assert_eq!(
            report.contains(&overflow),
            clipped > 0,
            "case {named} labels: expected the report to state {clipped} unreported name(s) \
             only when there are any, got\n{report}"
        );
    }
}

/// An explain is a READ: whatever the query it explains would write, the stored graph and
/// the openCypher query registry are what they were — and the evaluation a measuring mode
/// performs is what reports the solutions the write would have produced.
#[tokio::test]
async fn an_explain_changes_neither_the_graph_nor_the_registry() {
    let Emulator { router, engine } = emulator();
    seeded(&router).await;

    let writes = [
        "CREATE (:Person {name: 'Zoe'})",
        "MATCH (p:Person {name: 'Alice'}) SET p.age = 99",
        "MATCH (p:Person {name: 'Bob'}) REMOVE p.age",
        "MATCH (p:Person {name: 'Cleo'}) DETACH DELETE p",
        "MERGE (:Person {name: 'Eve'})",
        "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b) DELETE r",
    ];
    let before = answered(&router, "the graph before", "MATCH (n) RETURN n").await;
    for text in writes {
        for mode in OPEN_CYPHER_EXPLAIN_MODES {
            let report = report(&router, text, text, mode).await;
            assert!(
                !report.is_empty(),
                "case {text:?} in {mode:?}: an explain answers a report"
            );
            let after = answered(&router, "the graph after", "MATCH (n) RETURN n").await;
            assert_eq!(
                after, before,
                "case {text:?} in {mode:?}: an explain must leave the graph as it was, \
                 it now holds {after:?}"
            );
        }
    }

    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let registered = |label: &str| -> Vec<String> {
        let held = store.lock();
        let mut strings: Vec<String> = held
            .open_cypher_queries
            .values()
            .map(|record| record.query_string.clone())
            .collect();
        strings.sort();
        assert!(
            !strings.is_empty(),
            "{label}: the queries this test ANSWERED must be in the registry"
        );
        strings
    };
    let before_explains = registered("before the explains");
    for text in writes {
        for mode in OPEN_CYPHER_EXPLAIN_MODES {
            report(&router, text, text, mode).await;
        }
    }
    let after_explains = registered("after the explains");
    assert_eq!(
        after_explains, before_explains,
        "an explain is not a query the engine was asked to answer, so it is not recorded: \
         the registry went from {before_explains:?} to {after_explains:?}"
    );
}

/// The same query explains the same way twice: a plan follows from the query and the store
/// it is taken against, so nothing in a `static` report is per-call.
#[tokio::test]
async fn the_same_query_explains_the_same_way_twice() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;

    for case in PLANS {
        let once = report(&router, case.case, case.query, "static").await;
        let again = report(&router, case.case, case.query, "static").await;
        assert_eq!(
            once, again,
            "case {:?}: two explains of one query must answer the same report",
            case.case
        );
    }
}

/// One request an explain cannot be answered for, and the modelled error that states why.
#[derive(Debug, Clone, Copy)]
struct Refused {
    case: &'static str,
    query: &'static str,
    mode: &'static str,
    parameters: Option<&'static str>,
    status: u16,
    code: &'static str,
    /// Something the message has to carry for the caller to fix the request from it.
    message: &'static str,
}

const REFUSALS: &[Refused] = &[
    Refused {
        case: "a query that is not openCypher",
        query: "this is not a query",
        mode: "static",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
        message: "Invalid input",
    },
    Refused {
        case: "a blank query",
        query: "   ",
        mode: "static",
        parameters: None,
        status: 400,
        code: "MissingParameterException",
        message: "query",
    },
    Refused {
        case: "a mode the model does not declare",
        query: "MATCH (n) RETURN n",
        mode: "verbose",
        parameters: None,
        status: 400,
        // `explainMode` is a modelled enum, so a value outside it is refused by the
        // request walk before the handler runs, and the refusal names the member and the
        // values it takes.
        code: "ValidationException",
        message: "static, dynamic, details",
    },
    Refused {
        case: "a clause this engine does not run",
        query: "CALL db.labels()",
        mode: "static",
        parameters: None,
        status: 400,
        code: "UnsupportedOperationException",
        message: "CALL",
    },
    Refused {
        case: "parameters that are not a JSON object",
        query: "MATCH (n) RETURN n",
        mode: "static",
        parameters: Some("[1, 2]"),
        status: 400,
        code: "InvalidParameterException",
        message: "JSON object",
    },
    Refused {
        case: "a parameter the query references and the request did not carry",
        query: "MATCH (p:Person {name: $name}) RETURN p",
        mode: "static",
        parameters: Some("{\"other\": 1}"),
        status: 400,
        code: "InvalidParameterException",
        message: "name",
    },
    Refused {
        case: "a variable the query never binds",
        query: "MATCH (p:Person) RETURN q",
        mode: "dynamic",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
        message: "not defined",
    },
];

/// A request an explain cannot be answered for is refused through the modelled error whose
/// own documentation states why — never answered with a report for a query the engine
/// could not read, and never with a 501.
#[tokio::test]
async fn a_request_that_cannot_be_explained_is_refused_through_its_modelled_error() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;

    for case in REFUSALS {
        let answer = match case.parameters {
            Some(parameters) => explain_with(&router, case.query, case.mode, parameters).await,
            None => explain(&router, case.query, case.mode).await,
        };
        assert_eq!(
            answer.status,
            case.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            case.case,
            case.status,
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.error_code(),
            case.code,
            "case {:?}: expected {}, got {:?} with {:?}",
            case.case,
            case.code,
            answer.error_code(),
            answer.text()
        );
        let message = answer.detailed_message();
        let message = if message.is_empty() {
            // A refusal the request walk answers carries its reason in `message`; a
            // modelled Neptune error carries it in `detailedMessage`.
            answer.wire_message()
        } else {
            message
        };
        assert!(
            message.contains(case.message),
            "case {:?}: the message must carry {:?} for the caller to fix the request, \
             got {message:?}",
            case.case,
            case.message
        );
    }
}

/// A query with parameters is explained for the parameters the request carried: the plan
/// names the parameter the pattern takes its properties from, and `details` reports the
/// names that were bound.
#[tokio::test]
async fn a_parameterised_query_is_explained_for_the_parameters_the_request_carried() {
    let Emulator { router, .. } = emulator();
    seeded(&router).await;

    let answer = explain_with(
        &router,
        "MATCH (p:Person) WHERE p.name = $name RETURN p.age AS age",
        "details",
        "{\"name\": \"Alice\"}",
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "a parameterised query must be explained, got {} with {:?}",
        answer.status,
        answer.text()
    );
    let report = answer.text();
    assert!(
        arguments(&report)
            .iter()
            .any(|argument| argument.contains("filter=p.name = $name")),
        "the plan must name the parameter the filter reads, got {:?} from\n{report}",
        arguments(&report)
    );
    assert!(
        report.contains("[name]"),
        "details must report the parameter names the request bound, got\n{report}"
    );
    let last = operator_rows(&report).len() - 1;
    assert_eq!(
        cell(&report, last, "Units Out"),
        "1",
        "the bound parameter must match the one person it names, got {:?} from\n{report}",
        cell(&report, last, "Units Out")
    );
}
