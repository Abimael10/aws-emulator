// SPDX-License-Identifier: CC0-1.0

//! What a topic rule's SQL STATEMENT must be for the service to record it, and the
//! shape a reader of a recorded rule hands back.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what
//! an AWS SDK client would see.
//!
//! The rules under test:
//!
//! * a rule is recorded only if its statement PARSES as the rules engine's grammar
//!   (`SELECT <attributes> FROM <topic filter> [WHERE <condition>]`, the AWS IoT SQL
//!   reference); one that does not is refused with the modelled `SqlParseException`
//!   (HTTP 400), whose message states what the grammar expected, echoes the statement
//!   and marks the 1-based line and column the parse stopped at — and nothing is
//!   recorded, so a refused create leaves no rule and a refused replace leaves the
//!   rule that was there;
//! * the refusal names the error shape the way this service names it, qualified with
//!   its API doc namespace;
//! * `GetTopicRule` reports the rule's `properties` as the list it is — empty for
//!   every rule this service can hold, since no shape a rule is written from declares
//!   one — rather than omitting the member;
//! * every operation that addresses ONE rule by name refuses a name the store does not
//!   hold with the topic-rule family's own sentence, because AWS authorizes each
//!   against the rule's own ARN and so collapses "no such rule" and "not permitted"
//!   into one indistinguishable 401.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::UNREADABLE_SQL_MESSAGE;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. A statement is read the same
/// way in every region, so no assertion depends on it.
const REGION: &str = "us-east-1";

/// The `x-amzn-errortype` a statement the service cannot parse is refused under: the
/// shape name qualified with AWS IoT's API doc namespace, which is how this service's
/// validation layer names the shapes it refuses with.
const QUALIFIED_SQL_PARSE_EXCEPTION: &str =
    "SqlParseException:http://iot.amazonaws.com/doc/2015-10-02/";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` of an error body.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn create_rule(&self, rule_name: &str, payload: &Value) -> Reply {
        self.call("POST", &format!("/rules/{rule_name}"), Some(payload))
            .await
    }

    async fn replace_rule(&self, rule_name: &str, payload: &Value) -> Reply {
        self.call("PATCH", &format!("/rules/{rule_name}"), Some(payload))
            .await
    }

    async fn get_rule(&self, rule_name: &str) -> Reply {
        self.call("GET", &format!("/rules/{rule_name}"), None).await
    }

    async fn enable_rule(&self, rule_name: &str) -> Reply {
        self.call("POST", &format!("/rules/{rule_name}/enable"), None)
            .await
    }

    async fn disable_rule(&self, rule_name: &str) -> Reply {
        self.call("POST", &format!("/rules/{rule_name}/disable"), None)
            .await
    }
}

/// A `TopicRulePayload` carrying `sql` and the `@required` empty action list.
fn payload(sql: &str) -> Value {
    json!({ "sql": sql, "actions": [] })
}

/// What the refusal's message must say about a statement.
enum Expected {
    /// The grammar's own expectation, with the 1-based place it stopped: the whole
    /// message is then known, so it is asserted character for character.
    Stopped {
        token: &'static str,
        line: usize,
        column: usize,
    },
    /// The parser reported the stop itself — the expectation is its wording, so what
    /// is asserted is the RENDERING: the expectation line, the statement echoed, and a
    /// caret line whose column agrees with the position it reports.
    Rendered,
    /// The parser refused the statement without placing it — a statement that stops
    /// where the grammar expects more has no token to point at — so the message is the
    /// modelled error's own sentence.
    Unreadable,
}

/// One statement the service must refuse, with what the refusal must say.
struct Refused {
    label: &'static str,
    sql: &'static str,
    expected: Expected,
}

/// The statements a writer must refuse.
///
/// The rows differ along WHERE the parse stops and WHAT stops it: before any token has
/// been read (the grammar's opening keyword), at a token the opening keyword was
/// expected at, on a later line and column than the first, inside the statement's
/// clauses, and at a literal the tokenizer cannot even close.
fn refused() -> Vec<Refused> {
    vec![
        Refused {
            label: "a statement that opens with a misspelled keyword",
            sql: "SELEKT !!! FROM WHERE",
            expected: Expected::Stopped {
                token: "SELECT",
                line: 1,
                column: 1,
            },
        },
        Refused {
            label: "a statement that opens with another statement's keyword",
            sql: "INSERT INTO sensors VALUES (1)",
            expected: Expected::Stopped {
                token: "SELECT",
                line: 1,
                column: 1,
            },
        },
        Refused {
            label: "a statement that opens with no keyword at all",
            sql: "* FROM 'sensors/one'",
            expected: Expected::Stopped {
                token: "SELECT",
                line: 1,
                column: 1,
            },
        },
        Refused {
            label: "an empty statement",
            sql: "",
            expected: Expected::Stopped {
                token: "SELECT",
                line: 1,
                column: 1,
            },
        },
        Refused {
            label: "a statement whose opening token follows blank lines and a comment",
            sql: "\n\n  -- everything in the message\n  selekt * FROM 'sensors/one'",
            expected: Expected::Stopped {
                token: "SELECT",
                line: 4,
                column: 3,
            },
        },
        Refused {
            label: "a statement that stops after its FROM clause",
            sql: "SELECT * FROM",
            expected: Expected::Unreadable,
        },
        Refused {
            label: "a statement whose condition is unfinished",
            sql: "SELECT * FROM 'sensors/one' WHERE",
            expected: Expected::Unreadable,
        },
        Refused {
            label: "a statement carrying a token after its topic filter",
            sql: "SELECT * FROM 'sensors/one' 42",
            expected: Expected::Rendered,
        },
        Refused {
            label: "a statement whose topic filter never closes",
            sql: "SELECT * FROM 'never/closed",
            expected: Expected::Unreadable,
        },
    ]
}

/// The statements a writer must ACCEPT, so the refusals above are about the statement
/// and not about the operation: every form the AWS IoT SQL reference's grammar takes
/// that this service reads.
const ACCEPTED: &[(&str, &str)] = &[
    ("a plain topic filter", "SELECT * FROM 'sensors/one'"),
    (
        "lowercase keywords and a condition",
        "select temperature from 'rooms/#' where temperature > 10",
    ),
    (
        "newlines and tabs around the clauses",
        "SELECT *\n  FROM\t'multi/line/topic'\n  WHERE device = 'a'",
    ),
    (
        "a comment before the FROM clause",
        "SELECT * /* everything\n in the message */ FROM 'block/comment/topic'",
    ),
    (
        "a quote escaped inside the topic filter",
        "SELECT * FROM 'quote''s/topic'",
    ),
    (
        "a function call in the condition",
        "SELECT * FROM 'sensors/one' WHERE startswith(marker, 'm')",
    ),
    (
        "an unquoted relation, which names no topic filter",
        "SELECT * FROM sensors",
    ),
    ("no FROM clause at all", "SELECT 1"),
];

/// The message a refusal whose stop is known must carry: the expectation, a blank
/// line, the statement as submitted, and the caret line marking the column.
fn expected_message(token: &str, sql: &str, line: usize, column: usize) -> String {
    let caret = " ".repeat(column - 1);
    format!("Expected '{token}'\n\n{sql}\n{caret}^ at {line}:{column}")
}

/// Check that `message` renders a stop at all: the expectation line, the statement on
/// its own line, and a caret line whose column is the one the position reports.
fn assert_rendered(label: &str, sql: &str, message: &str) {
    assert!(
        message.starts_with("Expected '"),
        "case {label:?}: expected the message to open with the grammar's expectation, got \
         {message:?}"
    );
    assert!(
        message.contains(&format!("\n\n{sql}\n")),
        "case {label:?}: expected the message to echo the statement on its own line, got \
         {message:?}"
    );
    let caret_line = message
        .lines()
        .next_back()
        .unwrap_or_else(|| panic!("case {label:?}: the message has no lines: {message:?}"));
    let (caret, position) = caret_line
        .split_once("^ at ")
        .unwrap_or_else(|| panic!("case {label:?}: no caret line in {message:?}"));
    let (line, column) = position
        .split_once(':')
        .unwrap_or_else(|| panic!("case {label:?}: no line:column in {message:?}"));
    let line: usize = line
        .parse()
        .unwrap_or_else(|_| panic!("case {label:?}: {line:?} is no line number"));
    let column: usize = column
        .parse()
        .unwrap_or_else(|_| panic!("case {label:?}: {column:?} is no column number"));
    assert!(
        line >= 1 && column >= 1,
        "case {label:?}: expected a 1-based position, got {line}:{column}"
    );
    assert_eq!(
        caret.len(),
        column - 1,
        "case {label:?}: the caret sits at column {} but the position reports {column}: \
         {message:?}",
        caret.len() + 1
    );
}

/// A statement the rules engine cannot parse is refused, with the modelled
/// `SqlParseException` and a message that places the failure — and no rule is
/// recorded.
#[tokio::test]
async fn a_statement_that_does_not_parse_is_refused_and_records_no_rule() {
    let emulator = Emulator::new();

    for (index, case) in refused().into_iter().enumerate() {
        let rule_name = format!("refused_{index}");
        let reply = emulator.create_rule(&rule_name, &payload(case.sql)).await;

        assert_eq!(
            reply.status, 400,
            "case {:?}: expected CreateTopicRule to refuse 400, got {reply:?}",
            case.label
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some(QUALIFIED_SQL_PARSE_EXCEPTION),
            "case {:?}: expected the qualified SqlParseException, got {reply:?}",
            case.label
        );
        match case.expected {
            Expected::Stopped {
                token,
                line,
                column,
            } => assert_eq!(
                reply.message(),
                expected_message(token, case.sql, line, column),
                "case {:?}: got {:?}",
                case.label,
                reply.message()
            ),
            Expected::Rendered => assert_rendered(case.label, case.sql, &reply.message()),
            Expected::Unreadable => assert_eq!(
                reply.message(),
                UNREADABLE_SQL_MESSAGE,
                "case {:?}: got {:?}",
                case.label,
                reply.message()
            ),
        }

        // Nothing was recorded: the rule the refused create named does not exist, so
        // reading it is the not-found refusal and not a 200.
        let read = emulator.get_rule(&rule_name).await;
        assert_eq!(
            read.status, 401,
            "case {:?}: a refused create must record no rule, got {read:?}",
            case.label
        );
    }
}

/// The same reading guards the other writer: a replace carrying a statement that does
/// not parse is refused and the rule the store holds is untouched.
#[tokio::test]
async fn a_replace_carrying_a_statement_that_does_not_parse_leaves_the_rule_alone() {
    let emulator = Emulator::new();
    let working = "SELECT * FROM 'sensors/one'";
    let created = emulator.create_rule("replaced", &payload(working)).await;
    assert_eq!(
        created.status, 200,
        "the rule a replace is tried against must be creatable, got {created:?}"
    );

    for case in refused() {
        let reply = emulator.replace_rule("replaced", &payload(case.sql)).await;
        assert_eq!(
            reply.status, 400,
            "case {:?}: expected ReplaceTopicRule to refuse 400, got {reply:?}",
            case.label
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some(QUALIFIED_SQL_PARSE_EXCEPTION),
            "case {:?}: expected the qualified SqlParseException, got {reply:?}",
            case.label
        );

        let read = emulator.get_rule("replaced").await;
        assert_eq!(
            read.body["rule"]["sql"].as_str(),
            Some(working),
            "case {:?}: a refused replace changed the stored statement, got {read:?}",
            case.label
        );
    }
}

/// Every statement the grammar admits is recorded and read back as it was written —
/// and every rule a reader hands back carries `properties`, the empty list it is.
#[tokio::test]
async fn every_statement_the_grammar_admits_is_recorded_and_read_back_with_its_properties() {
    let emulator = Emulator::new();

    for (index, (label, sql)) in ACCEPTED.iter().enumerate() {
        let rule_name = format!("accepted_{index}");
        let created = emulator.create_rule(&rule_name, &payload(sql)).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected CreateTopicRule to answer 200, got {created:?}"
        );

        let read = emulator.get_rule(&rule_name).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: the created rule must be readable, got {read:?}"
        );
        assert_eq!(
            read.body["rule"]["sql"].as_str(),
            Some(*sql),
            "case {label:?}: the reader must report the statement that was written, got {read:?}"
        );
        assert_eq!(
            read.body["rule"].get("properties"),
            Some(&json!([])),
            "case {label:?}: expected the rule's properties to be the empty list, got {read:?}"
        );
    }
}

/// `properties` is reported for every rule, whatever the rest of the payload carries:
/// the member is part of the shape a reader hands back, not of what the caller sent.
#[tokio::test]
async fn every_rule_reports_its_properties_whatever_its_payload_carries() {
    let emulator = Emulator::new();

    // The rows differ along the payload members a rule may carry: the optional ones
    // absent, each present, several actions, and a disabled rule.
    let payloads: &[(&str, Value)] = &[
        (
            "only the required members",
            json!({ "sql": "SELECT * FROM 'a'", "actions": [] }),
        ),
        (
            "a description and an explicit SQL version",
            json!({
                "sql": "SELECT * FROM 'b'",
                "actions": [],
                "description": "a described rule",
                "awsIotSqlVersion": "2016-03-23",
            }),
        ),
        (
            "two actions and an error action",
            json!({
                "sql": "SELECT * FROM 'c'",
                "actions": [
                    { "republish": { "roleArn": "arn:aws:iam::000000000000:role/r", "topic": "out" } },
                    { "kinesis": { "roleArn": "arn:aws:iam::000000000000:role/r", "streamName": "s" } },
                ],
                "errorAction": {
                    "republish": { "roleArn": "arn:aws:iam::000000000000:role/r", "topic": "errors" }
                },
            }),
        ),
        (
            "a disabled rule",
            json!({ "sql": "SELECT * FROM 'd'", "actions": [], "ruleDisabled": true }),
        ),
    ];

    for (index, (label, body)) in payloads.iter().enumerate() {
        let rule_name = format!("properties_{index}");
        let created = emulator.create_rule(&rule_name, body).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected CreateTopicRule to answer 200, got {created:?}"
        );
        let read = emulator.get_rule(&rule_name).await;
        assert_eq!(
            read.body["rule"].get("properties"),
            Some(&json!([])),
            "case {label:?}: expected the rule's properties to be the empty list, got {read:?}"
        );
    }
}

/// Every operation that addresses ONE rule by name refuses a name the store does not
/// hold with the topic-rule family's own sentence — one indistinguishable 401, with
/// the name the URI label carried and nothing else.
#[tokio::test]
async fn addressing_a_rule_the_store_does_not_hold_is_one_refusal_everywhere() {
    let emulator = Emulator::new();
    // The names differ along the form a `RuleName` takes (`^[a-zA-Z0-9_]+$`, length
    // 1..128): the shortest, a plain one, one carrying digits and underscores, and the
    // longest.
    let names = [
        "r".to_string(),
        "absent_rule".to_string(),
        "Rule_9_absent".to_string(),
        "n".repeat(128),
    ];

    for name in names {
        let expected = format!("Access to topic rule '{name}' was denied");
        let replies = [
            ("GetTopicRule", emulator.get_rule(&name).await),
            (
                "ReplaceTopicRule",
                emulator
                    .replace_rule(&name, &payload("SELECT * FROM 'sensors/one'"))
                    .await,
            ),
            ("EnableTopicRule", emulator.enable_rule(&name).await),
            ("DisableTopicRule", emulator.disable_rule(&name).await),
        ];
        for (operation, reply) in replies {
            assert_eq!(
                reply.status, 401,
                "case {name:?}/{operation}: expected 401, got {reply:?}"
            );
            assert_eq!(
                reply.error_code.as_deref(),
                Some("UnauthorizedException"),
                "case {name:?}/{operation}: expected UnauthorizedException, got {reply:?}"
            );
            assert_eq!(
                reply.message(),
                expected,
                "case {name:?}/{operation}: got {:?}",
                reply.message()
            );
        }
    }
}
