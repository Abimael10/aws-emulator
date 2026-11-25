// SPDX-License-Identifier: CC0-1.0

//! `ExecuteGremlinExplainQuery` end to end over HTTP.
//!
//! The rule under test: an explain answers with the report for the traversal that was
//! SUBMITTED, as the database serving it describes answering it. The report is a banner
//! headed by the centred title, then one section per thing an explain states — the query
//! string, the compiled traversal, the explain instant and the engine version, the
//! converted plan, the optimized plan and the predicate count — and every line of it
//! follows from the query and from this engine's own state: the quad patterns the plan
//! matches, the steps the planner leaves to the step iterators (named in the section's
//! `+ not converted into Neptune steps:` trailer), the pushdown the optimized plan adds,
//! the engine version the status reports and the predicates the stored graph holds. So a
//! query that is not a traversal is refused instead of explained, the same query explains
//! the same way twice, and the query the engine read and planned is recorded in the
//! registry the Gremlin status family reports from.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::state::{
    ComAmazonawsNeptunedataAmazonneptunedataplaneState, QueryState, MAX_ARGUMENT_DEPTH,
    MAX_QUERY_LENGTH,
};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service. `neptune-db` is neptunedata's sigv4
/// signing name, and it is what routes a request to this service.
const SIGNED_FOR_NEPTUNE_DB: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/neptune-db/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The title the explain report is headed with, which is also what tells the report
/// apart from a JSON error body on the wire.
const REPORT_TITLE: &str = "Neptune Gremlin Explain";

/// One emulator: a router carrying only this service, over one shared engine. The engine
/// is handed back so a test can read the query registry the handler writes.
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

/// What the emulator answered: the status, the media type it announced and the raw body,
/// which on a success is the report text rather than JSON.
struct Answer {
    status: u16,
    content_type: String,
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
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "neptune-db.us-east-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_NEPTUNE_DB);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Answer {
        status,
        content_type,
        body: response.into_body().to_vec(),
    }
}

/// `ExecuteGremlinExplainQuery` is `POST /gremlin/explain`, with the query in the body
/// member `gremlin`.
/// Source: smithy_query(query="operation", operation="ExecuteGremlinExplainQuery") — the
/// http binding and `gremlinQuery`'s `jsonName`.
async fn explain(router: &MultiServiceRouter, query: &str) -> Answer {
    call(
        router,
        "POST",
        "/gremlin/explain",
        Some(json!({ "gremlin": query })),
    )
    .await
}

/// The lines of one section of the report: the lines after the section's name and its
/// underline, up to the next blank line.
fn section(report: &str, name: &str) -> Vec<String> {
    let mut lines = report.lines();
    assert!(
        lines.by_ref().any(|line| line == name),
        "the report has no {name:?} section:\n{report}"
    );
    let underline = lines.next().unwrap_or_default();
    assert!(
        !underline.is_empty() && underline.chars().all(|character| character == '='),
        "the {name:?} section is not underlined, found {underline:?}:\n{report}"
    );
    lines
        .take_while(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// One query to explain, and what the report for it must say. The rows differ along
/// everything the rule generalises over: how the start is found, which steps the plan
/// pushes down and which it leaves behind, whether the optimized plan folds a pinned label
/// or a terminal count into the graph query, how arguments are written, and which steps and
/// modulators the query is made of.
#[derive(Debug, Clone, Copy)]
struct Case {
    case: &'static str,
    query: &'static str,
    /// The whole "Original Traversal" line: the steps the query compiles to.
    traversal: &'static str,
    /// Lines the CONVERTED plan must carry, each compared with a whole line of the section.
    converted: &'static [&'static str],
    /// Lines the OPTIMIZED plan must carry.
    optimized: &'static [&'static str],
    /// The whole `+ not converted into Neptune steps:` trailer of each section, or `None`
    /// for a section that leaves nothing behind.
    not_converted: (Option<&'static str>, Option<&'static str>),
    /// Text no line of either plan section may carry.
    absent: &'static [&'static str],
}

/// The entries of a Neptune step list are indented and comma-separated — never bulleted —
/// and the annotation of a graph query is a brace group of its own.
const CASES: &[Case] = &[
    Case {
        case: "a bare vertex scan is found by label",
        query: "g.V()",
        traversal: "[GraphStep(vertex,[])]",
        converted: &[
            "NeptuneGraphQueryStep(Vertex) {",
            "JoinGroupNode {",
            "PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]",
            "}, {path=[Vertex(?1):GraphStep], maxVarId=3}",
            "NeptuneTraverserConverterStep",
        ],
        // Nothing to fold: no label is pinned and no count is asked for.
        optimized: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        not_converted: (None, None),
        absent: &["+- ", "annotations="],
    },
    Case {
        case: "a property filter is what finds the start",
        query: "g.V().has('code','ANC')",
        traversal: "[GraphStep(vertex,[]), HasStep([code.eq(ANC)])]",
        converted: &[
            "PatternNode[(?1, <code>, \"ANC\", <~>) . project ?1 .]",
            "}, {path=[Vertex(?1):GraphStep], maxVarId=2}",
        ],
        optimized: &["PatternNode[(?1, <code>, \"ANC\", <~>) . project ?1 .]"],
        not_converted: (None, None),
        // The filter binds the start, so no label statement is matched for it.
        absent: &["<~label>"],
    },
    Case {
        case: "a pinned label is a filter in the converted plan and is folded into the \
               optimized one",
        query: "g.V().hasLabel('airport')",
        traversal: "[GraphStep(vertex,[]), HasStep([~label.eq(airport)])]",
        converted: &[
            "PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 . \
             ContainsFilter(?2 in (<airport>)) .]",
            "}, {path=[Vertex(?1):GraphStep], maxVarId=3}",
        ],
        optimized: &["PatternNode[VL(?1, <~label>, ?2=<airport>, <~>) . project ?1 .]"],
        not_converted: (None, None),
        // One statement carries the label, so the filter is not a second pattern.
        absent: &["HasStep"],
    },
    Case {
        case: "a terminal count over a pushed-down prefix is counted by the optimized plan",
        query: "g.V().hasLabel('airport').count()",
        traversal: "[GraphStep(vertex,[]), HasStep([~label.eq(airport)]), CountGlobalStep]",
        converted: &[
            "NeptuneGraphQueryStep(Vertex) {",
            "PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 . \
             ContainsFilter(?2 in (<airport>)) .]",
            "}, {path=[Vertex(?1):GraphStep], maxVarId=3}",
            "NeptuneTraverserConverterStep",
        ],
        optimized: &[
            "NeptuneCountGlobalStep {",
            "PatternNode[VL(?1, <~label>, ?2=<airport>, <~>) . project ?1 .]",
            "}, {path=[Vertex(?1):GraphStep], maxVarId=3}",
        ],
        not_converted: (
            Some("+ not converted into Neptune steps: CountGlobalStep,"),
            None,
        ),
        absent: &[],
    },
    Case {
        case: "a count over a scan the plan pushed down is counted the same way",
        query: "g.V().count().next()",
        traversal: "[GraphStep(vertex,[]), CountGlobalStep]",
        converted: &["NeptuneGraphQueryStep(Vertex) {", "NeptuneTraverserConverterStep"],
        optimized: &["NeptuneCountGlobalStep {"],
        not_converted: (
            Some("+ not converted into Neptune steps: CountGlobalStep,"),
            None,
        ),
        absent: &["NextStep"],
    },
    Case {
        case: "a label filter and an edge walk, with the range left to the step iterators",
        query: "g.V().hasLabel(\"airport\").out('route').limit(5)",
        traversal: "[GraphStep(vertex,[]), HasStep([~label.eq(airport)]), \
                    VertexStep(OUT,[route],vertex), RangeGlobalStep(0,5)]",
        converted: &[
            "PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 . \
             ContainsFilter(?2 in (<airport>)) .]",
            "PatternNode[(?1, <route>, ?3, ?4) . project ?1,?3 .]",
            "}, {path=[Vertex(?1):GraphStep, Vertex(?3):VertexStep], maxVarId=5}",
        ],
        optimized: &["PatternNode[VL(?1, <~label>, ?2=<airport>, <~>) . project ?1 .]"],
        not_converted: (
            Some("+ not converted into Neptune steps: RangeGlobalStep(0,5),"),
            Some("+ not converted into Neptune steps: RangeGlobalStep(0,5),"),
        ),
        absent: &[],
    },
    Case {
        case: "an id-bound start needs no lookup, and a property binds a value",
        query: "g.V('3').values('code')",
        traversal: "[GraphStep(vertex,[3]), PropertiesStep([code],value)]",
        converted: &[
            "NeptuneGraphQueryStep(Value) {",
            "PatternNode[VL(\"3\", <~label>, ?1, <~>) . project ask .]",
            "PatternNode[(\"3\", <code>, ?2, <~>) . project ?2 .]",
            "}, {path=[Vertex(\"3\"):GraphStep, Value(?2):PropertiesStep], maxVarId=3}",
        ],
        optimized: &["PatternNode[(\"3\", <code>, ?2, <~>) . project ?2 .]"],
        not_converted: (None, None),
        absent: &[],
    },
    Case {
        case: "an edge walk binds both endpoints, so its endpoint step matches nothing more",
        query: "g.V().outE('route').inV()",
        traversal: "[GraphStep(vertex,[]), VertexStep(OUT,[route],edge), EdgeVertexStep(IN)]",
        converted: &[
            "NeptuneGraphQueryStep(Vertex) {",
            "PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]",
            "PatternNode[(?1, <route>, ?3, ?4) . project ?1,?3,?4 .]",
            "}, {path=[Vertex(?1):GraphStep, Vertex(?3):VertexStep, \
             Edge(?4):VertexStep], maxVarId=5}",
        ],
        optimized: &["PatternNode[(?1, <route>, ?3, ?4) . project ?1,?3,?4 .]"],
        not_converted: (None, None),
        absent: &[],
    },
    Case {
        case: "reading the label projects it, so the projection is not made distinct",
        query: "g.V().label()",
        traversal: "[GraphStep(vertex,[]), LabelStep]",
        converted: &[
            "NeptuneGraphQueryStep(Value) {",
            "PatternNode[VL(?1, <~label>, ?3, <~>) . project ?1,?3 .]",
            "}, {path=[Vertex(?1):GraphStep, Value(?3):LabelStep], maxVarId=4}",
        ],
        optimized: &["PatternNode[VL(?1, <~label>, ?3, <~>) . project ?1,?3 .]"],
        not_converted: (None, None),
        absent: &[],
    },
    Case {
        case: "a predicate is not a value, so the filter stays with the step iterators",
        query: "g.V().has('age',gt(30))",
        traversal: "[GraphStep(vertex,[]), HasStep([age.gt(30)])]",
        converted: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        optimized: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        not_converted: (
            Some("+ not converted into Neptune steps: HasStep([age.gt(30)]),"),
            Some("+ not converted into Neptune steps: HasStep([age.gt(30)]),"),
        ),
        absent: &[],
    },
    Case {
        case: "several labels are alternatives, which one pattern does not express",
        query: "g.V().hasLabel('airport','country')",
        traversal: "[GraphStep(vertex,[]), HasStep([~label.within(airport,country)])]",
        converted: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        optimized: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        not_converted: (
            Some(
                "+ not converted into Neptune steps: \
                 HasStep([~label.within(airport,country)]),",
            ),
            Some(
                "+ not converted into Neptune steps: \
                 HasStep([~label.within(airport,country)]),",
            ),
        ),
        absent: &["ContainsFilter"],
    },
    Case {
        case: "whitespace and the other quotation do not change the reading",
        query: "  g . V ( ) . has ( \"code\" , 'ANC' )  ",
        traversal: "[GraphStep(vertex,[]), HasStep([code.eq(ANC)])]",
        converted: &["PatternNode[(?1, <code>, \"ANC\", <~>) . project ?1 .]"],
        optimized: &["PatternNode[(?1, <code>, \"ANC\", <~>) . project ?1 .]"],
        not_converted: (None, None),
        absent: &["<~label>"],
    },
    Case {
        case: "a modulator is printed on the step it configures",
        query: "g.V().order().by('code')",
        traversal: "[GraphStep(vertex,[]), OrderGlobalStep.by(code)]",
        converted: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        optimized: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        not_converted: (
            Some("+ not converted into Neptune steps: OrderGlobalStep.by(code),"),
            Some("+ not converted into Neptune steps: OrderGlobalStep.by(code),"),
        ),
        absent: &["ByStep"],
    },
    Case {
        case: "a step label is printed on its step, which is then not pushed down",
        query: "g.V().as('a').out()",
        traversal: "[GraphStep(vertex,[])@[a], VertexStep(OUT,vertex)]",
        converted: &[],
        optimized: &[],
        not_converted: (
            Some(
                "+ not converted into Neptune steps: GraphStep(vertex,[])@[a], \
                 VertexStep(OUT,vertex),",
            ),
            Some(
                "+ not converted into Neptune steps: GraphStep(vertex,[])@[a], \
                 VertexStep(OUT,vertex),",
            ),
        ),
        absent: &["NeptuneGraphQueryStep", "PatternNode"],
    },
    Case {
        case: "a step this engine does not name explicitly keeps the step-class convention",
        query: "g.V().coalesce(__.out('a'),__.in('b'))",
        traversal: "[GraphStep(vertex,[]), CoalesceStep(__.out(a),__.in(b))]",
        converted: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        optimized: &["PatternNode[VL(?1, <~label>, ?2, <~>) . project distinct ?1 .]"],
        not_converted: (
            Some("+ not converted into Neptune steps: CoalesceStep(__.out(a),__.in(b)),"),
            Some("+ not converted into Neptune steps: CoalesceStep(__.out(a),__.in(b)),"),
        ),
        absent: &[],
    },
];

/// Every section of the report, in the order an explain states them.
const SECTIONS: &[&str] = &[
    "Query String",
    "Original Traversal",
    "Converted Traversal",
    "Optimized Traversal",
    "Predicates",
];

#[tokio::test]
async fn the_report_is_computed_from_the_submitted_traversal() {
    let Emulator { router, .. } = emulator();
    for case in CASES {
        let answer = explain(&router, case.query).await;
        assert_eq!(
            answer.status,
            200,
            "case {:?}: expected 200, got {} with body {:?}",
            case.case,
            answer.status,
            answer.text()
        );
        let report = answer.text();
        assert!(
            report.lines().any(|line| line.trim() == REPORT_TITLE),
            "case {:?}: the answer must be the explain report, got {report:?}",
            case.case
        );

        assert_eq!(
            section(&report, "Query String"),
            vec![case.query.trim().to_string()],
            "case {:?}: the report must echo the query as submitted, got:\n{report}",
            case.case
        );
        assert_eq!(
            section(&report, "Original Traversal"),
            vec![case.traversal.to_string()],
            "case {:?}: expected the steps {:?}, got:\n{report}",
            case.case,
            case.traversal
        );

        for (name, expected, trailer) in [
            ("Converted Traversal", case.converted, case.not_converted.0),
            ("Optimized Traversal", case.optimized, case.not_converted.1),
        ] {
            let plan = section(&report, name);
            for line in expected {
                assert!(
                    plan.iter().any(|printed| printed.trim() == *line),
                    "case {:?}: the {name} must carry the line {line:?}, got {plan:?}",
                    case.case
                );
            }
            for forbidden in case.absent {
                assert!(
                    !plan.iter().any(|printed| printed.contains(forbidden)),
                    "case {:?}: the {name} must not carry {forbidden:?}, got {plan:?}",
                    case.case
                );
            }
            let left_behind: Vec<&String> = plan
                .iter()
                .filter(|printed| printed.starts_with("+ not converted"))
                .collect();
            match trailer {
                Some(expected) => assert_eq!(
                    left_behind,
                    vec![&expected.to_string()],
                    "case {:?}: the {name} must name the steps it left behind, got {plan:?}",
                    case.case
                ),
                None => assert!(
                    left_behind.is_empty(),
                    "case {:?}: the {name} converted every step, so it must name none as \
                     left behind, got {left_behind:?}",
                    case.case
                ),
            }
        }
    }
}

/// RULE: every report carries the same sections, in the same order, under the centred
/// title — whatever the traversal is, and whether or not the plan pushed anything down.
#[tokio::test]
async fn the_report_carries_every_section_in_order() {
    let Emulator { router, .. } = emulator();
    for case in CASES {
        let report = explain(&router, case.query).await.text();
        let lines: Vec<&str> = report.lines().collect();

        // The banner: the rule, the title indented under it, the rule again.
        assert_eq!(
            lines.first().map(|line| line.trim_end()),
            lines.get(2).map(|line| line.trim_end()),
            "case {:?}: the title must be banners between two rules, got:\n{report}",
            case.case
        );
        let title = lines.get(1).copied().unwrap_or_default();
        assert_eq!(
            title.trim(),
            REPORT_TITLE,
            "case {:?}: expected the title on the second line, got {title:?}",
            case.case
        );
        let rule = lines.first().copied().unwrap_or_default();
        let indent = title.len() - title.trim_start().len();
        assert_eq!(
            indent,
            (rule.len() - REPORT_TITLE.len()) / 2,
            "case {:?}: the title must be centred under the rule, got {title:?}",
            case.case
        );

        let mut at = 0;
        for name in SECTIONS {
            let found = lines
                .iter()
                .skip(at)
                .position(|line| line == name)
                .map(|offset| offset + at);
            let Some(found) = found else {
                panic!(
                    "case {:?}: the report must carry the {name:?} section after the ones \
                     before it, got:\n{report}",
                    case.case
                );
            };
            at = found + 1;
        }
    }
}

/// The value of one of the report's header lines — the lines stating a fact of the RUN
/// rather than of the traversal, which stand between the compiled traversal and the plans.
fn header(report: &str, name: &str) -> String {
    let prefix = format!("{name}: ");
    report
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("the report has no {name:?} header line:\n{report}"))
        .to_string()
}

/// Whether `value` is an instant in the form the report states one in: the UTC calendar
/// form, to the millisecond. Checked by SHAPE and never against the clock, so the check
/// says the same thing on any host.
fn is_report_instant(value: &str) -> bool {
    let shape = "0000-00-00T00:00:00.000Z";
    value.len() == shape.len()
        && value
            .chars()
            .zip(shape.chars())
            .all(|(character, expected)| match expected {
                '0' => character.is_ascii_digit(),
                literal => character == literal,
            })
}

/// `report` without its explain instant, which is the one line of it that is a fact of the
/// run rather than of the query and the database.
fn without_the_instant(report: &str) -> String {
    report
        .lines()
        .filter(|line| !line.starts_with("Explain Time: "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The report is derived from the query and from this engine's state, so explaining the
/// same query twice against the same state answers the same report — apart from the
/// instant the explain ran, which is a fact of the run.
#[tokio::test]
async fn the_same_query_explains_the_same_way_twice() {
    let Emulator { router, .. } = emulator();
    for case in CASES {
        let first = explain(&router, case.query).await;
        let second = explain(&router, case.query).await;
        assert_eq!(
            without_the_instant(&first.text()),
            without_the_instant(&second.text()),
            "case {:?}: two explains of one query must answer the same report",
            case.case
        );
    }
}

/// RULE: the report's header lines state the run itself — WHEN the explain ran, in the UTC
/// form to the millisecond, and the engine version of the database that served it, which is
/// the version that database's engine status reports.
#[tokio::test]
async fn the_header_lines_state_the_run_and_the_engine_serving_it() {
    let Emulator { router, .. } = emulator();
    let status = call(&router, "GET", "/status", None).await;
    let reported: Value = serde_json::from_slice(&status.body)
        .unwrap_or_else(|error| panic!("the status answer must be JSON ({error})"));
    let version = reported["dbEngineVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !version.is_empty(),
        "the engine status must report an engine version, got {reported:?}"
    );

    for case in CASES {
        let report = explain(&router, case.query).await.text();
        let instant = header(&report, "Explain Time");
        assert!(
            is_report_instant(&instant),
            "case {:?}: the explain instant must be stated in the UTC form, got {instant:?}",
            case.case
        );
        assert_eq!(
            header(&report, "Engine Version"),
            version,
            "case {:?}: the report must state the engine version the status reports",
            case.case
        );
    }
}

/// One graph an explain is taken against, and how many distinct predicates it holds.
///
/// Every statement an element is stored with is a quad whose predicate position names what
/// the statement says, so the rows differ along what makes a predicate: an element's own
/// label, a property key on a node, a relationship's type and a property key on a
/// relationship — and along a predicate being used by more than one element, which is still
/// one predicate.
#[derive(Debug, Clone, Copy)]
struct Predicates {
    case: &'static str,
    /// The openCypher writes that build the graph.
    writes: &'static [&'static str],
    count: usize,
}

const PREDICATE_COUNTS: &[Predicates] = &[
    Predicates {
        case: "an empty graph holds no predicate",
        writes: &[],
        count: 0,
    },
    Predicates {
        case: "a node with a label alone holds the label predicate",
        writes: &["CREATE (n:Airport)"],
        count: 1,
    },
    Predicates {
        case: "a node with no label holds its property key alone",
        writes: &["CREATE (n {code: 'ANC'})"],
        count: 1,
    },
    Predicates {
        case: "a label and a property key are two predicates",
        writes: &["CREATE (n:Airport {code: 'ANC'})"],
        count: 2,
    },
    Predicates {
        case: "a predicate many elements carry is one predicate",
        writes: &[
            "CREATE (n:Airport {code: 'ANC'})",
            "CREATE (n:Airport {code: 'SEA'})",
            "CREATE (n:Country {code: 'US'})",
        ],
        count: 2,
    },
    Predicates {
        case: "a second property key on a node is a second predicate",
        writes: &["CREATE (n:Airport {code: 'ANC', city: 'Anchorage'})"],
        count: 3,
    },
    Predicates {
        case: "a relationship's type and its own property key are predicates too",
        writes: &[
            "CREATE (a:Airport {code: 'ANC'})-[:ROUTE {dist: 1448}]->(b:Airport {code: 'SEA'})",
        ],
        count: 4,
    },
];

/// RULE: the predicate count the report ends with is how many distinct predicates the GRAPH
/// the plan would be evaluated against holds — so it follows the writes the graph has taken
/// and is not a property of the traversal being explained.
#[tokio::test]
async fn the_predicate_count_is_the_graph_s_own() {
    for case in PREDICATE_COUNTS {
        let Emulator { router, .. } = emulator();
        for write in case.writes {
            let answer = call(
                &router,
                "POST",
                "/opencypher",
                Some(json!({ "query": write })),
            )
            .await;
            assert_eq!(
                answer.status,
                200,
                "case {:?}: the write {write:?} must be taken, got {} with {:?}",
                case.case,
                answer.status,
                answer.text()
            );
        }

        let report = explain(&router, "g.V().hasLabel('Airport').count()")
            .await
            .text();
        assert_eq!(
            section(&report, "Predicates"),
            vec![format!("# of predicates: {}", case.count)],
            "case {:?}: expected {} predicates, got:\n{report}",
            case.case,
            case.count
        );
    }
}

/// RULE: a response announces the media type the MODEL declares for its payload — the text
/// report this operation answers with is text, not an opaque download — while a response
/// carrying this service's JSON envelope announces the JSON media type, and a payload the
/// model declares no media type for keeps the type its blob implies.
#[tokio::test]
async fn the_report_announces_the_media_type_the_model_declares() {
    let Emulator { router, .. } = emulator();

    let served = explain(&router, "g.V().hasLabel('airport').count()").await;
    assert_eq!(
        served.status,
        200,
        "the explain must be served, got {} with {:?}",
        served.status,
        served.text()
    );
    assert_eq!(
        served.content_type, "text/plain",
        "the explain report is text, got {:?} with {:?}",
        served.content_type,
        served.text()
    );

    let refused = explain(&router, "g.V(").await;
    assert_eq!(
        refused.status,
        400,
        "a query that is not a traversal must be refused, got {}",
        refused.status
    );
    assert_eq!(
        refused.content_type, "application/json; charset=UTF-8",
        "a refusal is this service's JSON envelope, got {:?} with {:?}",
        refused.content_type,
        refused.text()
    );

    // The openCypher explain binds a plain blob as its payload, for which the model
    // declares no media type: it keeps the one Smithy implies for a blob.
    let opaque = call(
        &router,
        "POST",
        "/opencypher/explain",
        Some(json!({ "query": "MATCH (n) RETURN n", "explain": "static" })),
    )
    .await;
    assert_eq!(
        opaque.status,
        200,
        "the openCypher explain must be served, got {} with {:?}",
        opaque.status,
        opaque.text()
    );
    assert_eq!(
        opaque.content_type, "application/octet-stream",
        "a payload the model declares no media type for keeps the blob's own, got {:?}",
        opaque.content_type
    );
}

/// The query the engine read and planned is recorded, and the status family reports it
/// from that record: the explain is answered within the call, so the query is finished
/// and is no longer one of the current queries the list reports.
#[tokio::test]
async fn an_explained_query_is_recorded_and_read_back_through_the_status_api() {
    let Emulator { router, engine } = emulator();
    let query = "g.V().hasLabel('airport').has('code','ANC')";

    let answer = explain(&router, query).await;
    assert_eq!(
        answer.status,
        200,
        "the explain must be served, got {} with body {:?}",
        answer.status,
        answer.text()
    );

    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let recorded = store
        .lock()
        .gremlin_queries
        .values()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        recorded.len(),
        1,
        "the explained query must be the one record in the registry, got {recorded:?}"
    );
    let record = &recorded[0];
    assert_eq!(
        record.query_string, query,
        "the record must hold the query as submitted, got {:?}",
        record.query_string
    );
    assert_eq!(
        record.state,
        QueryState::Done,
        "an explain is answered within the call, so its query is finished, got {:?}",
        record.state
    );

    let status = call(
        &router,
        "GET",
        &format!("/gremlin/status/{}", record.query_id),
        None,
    )
    .await;
    assert_eq!(
        status.status,
        200,
        "the recorded query id must be readable, got {} with body {:?}",
        status.status,
        status.text()
    );
    let body: Value = serde_json::from_slice(&status.body)
        .unwrap_or_else(|error| panic!("the status answer must be JSON ({error}): {:?}", status.text()));
    assert_eq!(
        body["queryString"].as_str(),
        Some(query),
        "the status read must report the stored query string, got {body:?}"
    );
    assert_eq!(
        body["queryEvalStats"]["cancelled"].as_bool(),
        Some(false),
        "an explained query was not cancelled, got {body:?}"
    );

    let listed = call(&router, "GET", "/gremlin/status", None).await;
    let listed_body: Value = serde_json::from_slice(&listed.body)
        .unwrap_or_else(|error| panic!("the list answer must be JSON ({error}): {:?}", listed.text()));
    assert_eq!(
        listed_body["acceptedQueryCount"].as_i64(),
        Some(0),
        "a finished query is not one the engine still owes an answer, got {listed_body:?}"
    );
    assert!(
        listed_body["queries"]
            .as_array()
            .is_none_or(|queries| queries.is_empty()),
        "a finished query is not one of the current queries, got {listed_body:?}"
    );
}

/// One query that is not a traversal this engine can plan, and the refusal it must get.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    case: &'static str,
    query: &'static str,
    /// The wire error code, without its shape namespace.
    code: &'static str,
}

const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "an argument list that is never closed",
        query: "g.V(",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a query that starts from no traversal source",
        query: "V().count()",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a query that starts from another name",
        query: "h.V().count()",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "two dots between steps",
        query: "g.V()..count()",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a trailing dot",
        query: "g.V().",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a string that is never closed",
        query: "g.V().has('code)",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a character the Gremlin string form has no token for",
        query: "g.V().filter{it.get()}",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a missing comma between arguments",
        query: "g.V().has('code' 'ANC')",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a modulator with no step before it",
        query: "g.by('code')",
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a query that is only whitespace",
        query: "   ",
        code: "MissingParameterException",
    },
    Refusal {
        case: "an empty query",
        query: "",
        code: "MissingParameterException",
    },
];

#[tokio::test]
async fn a_query_that_is_not_a_traversal_is_refused_and_recorded_nowhere() {
    let Emulator { router, engine } = emulator();
    for refusal in REFUSALS {
        let answer = explain(&router, refusal.query).await;
        assert_eq!(
            answer.status,
            400,
            "case {:?}: expected 400, got {} with body {:?}",
            refusal.case,
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.error_code(),
            refusal.code,
            "case {:?}: expected {}, got body {:?}",
            refusal.case,
            refusal.code,
            answer.text()
        );
        assert!(
            !answer.detailed_message().is_empty(),
            "case {:?}: the refusal must say what was wrong, got body {:?}",
            refusal.case,
            answer.text()
        );

        let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
        let recorded = store.lock().gremlin_queries.len();
        assert_eq!(
            recorded, 0,
            "case {:?}: a refused query must leave the registry empty, it holds {recorded}",
            refusal.case
        );
    }
}

/// A traversal of `steps` walks, which is a long query that is not a nested one.
fn chained(steps: usize) -> String {
    let mut query = "g.V()".to_string();
    for _ in 0..steps {
        query.push_str(".out('route')");
    }
    query
}

/// The length bound is a limit on the query, not on what it says: a query inside the
/// bound is explained however long it is, and one past it is refused as too large rather
/// than read.
#[tokio::test]
async fn length_is_bounded_at_the_published_size() {
    let Emulator { router, .. } = emulator();

    // The longest chain of walks that still fits inside the bound.
    let inside = chained((MAX_QUERY_LENGTH - "g.V()".len()) / ".out('route')".len());
    assert!(
        inside.len() <= MAX_QUERY_LENGTH,
        "the case must be inside the bound, it is {} characters",
        inside.len()
    );
    let answer = explain(&router, &inside).await;
    assert_eq!(
        answer.status,
        200,
        "case a query of {} characters (inside the bound): expected 200, got {}",
        inside.len(),
        answer.status
    );

    for length in [MAX_QUERY_LENGTH + 1, MAX_QUERY_LENGTH * 2] {
        // A query past the bound, made of a valid traversal padded with a comment-free
        // run of whitespace, so it is the LENGTH that is refused and nothing else.
        let query = format!("g.V(){}", " ".repeat(length - "g.V()".len()));
        let answer = explain(&router, &query).await;
        assert_eq!(
            answer.status,
            400,
            "case a query of {} characters (past the bound): expected 400, got {} with body {:?}",
            query.len(),
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.error_code(),
            "QueryTooLargeException",
            "case a query of {} characters (past the bound): expected QueryTooLargeException, got {:?}",
            query.len(),
            answer.text()
        );
    }
}

/// A traversal nested `wrappers` deep: `wrappers` nested `where` steps around an
/// innermost traversal, so the argument nesting the parse walks is `wrappers + 1`.
fn nested(wrappers: usize) -> String {
    let mut inner = "__.out()".to_string();
    for _ in 0..wrappers {
        inner = format!("__.where({inner})");
    }
    format!("g.V().where({inner})")
}

/// The nesting bound is a limit on the query, not on what it says: a query at the bound
/// is explained, and one past it is refused as exceeding a system limit rather than
/// walked to the bottom of the stack.
#[tokio::test]
async fn nesting_is_bounded_at_the_published_depth() {
    let Emulator { router, engine } = emulator();

    for wrappers in [0, 1, MAX_ARGUMENT_DEPTH - 2, MAX_ARGUMENT_DEPTH - 1] {
        let query = nested(wrappers);
        let answer = explain(&router, &query).await;
        assert_eq!(
            answer.status,
            200,
            "case {wrappers} wrappers (at or inside the bound): expected 200, got {} with body {:?}",
            answer.status,
            answer.text()
        );
    }

    for wrappers in [MAX_ARGUMENT_DEPTH, MAX_ARGUMENT_DEPTH + 8] {
        let query = nested(wrappers);
        let answer = explain(&router, &query).await;
        assert_eq!(
            answer.status,
            400,
            "case {wrappers} wrappers (past the bound): expected 400, got {} with body {:?}",
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.error_code(),
            "QueryLimitException",
            "case {wrappers} wrappers (past the bound): expected QueryLimitException, got {:?}",
            answer.text()
        );
    }

    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let recorded = store.lock().gremlin_queries.len();
    assert_eq!(
        recorded, 4,
        "only the queries inside the bound are recorded, the registry holds {recorded}"
    );
}
