// SPDX-License-Identifier: CC0-1.0

//! What Amazon Personalize's `CreateFilter` judges, and in which order — driven across
//! the services the question spans, mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! RULE: `CreateFilter` answers three questions in this order, and the first one that
//! fails is what the reply says.
//!
//! 1. The expression's INPUT: a character the filter-expression language is not written
//!    in is refused with the service's own sentence naming that input symbol, before
//!    anything about the dataset group is looked at.
//! 2. The dataset group's own STATE: a filter selects what a recommendation may return
//!    for a user, which is decided from the group's interaction records, so a group
//!    holding none — no `INTERACTIONS` dataset a dataset import job completed into, and
//!    no event-interactions stream from an event tracker — takes no filter at all,
//!    whatever its expression reads. The refusal is wholly fixed text: it names neither
//!    the group, the expression nor the datasets the group does hold.
//! 3. The expression's GRAMMAR, which is what the inference plane applies.
//!
//! Two crates have to agree for that to hold (Personalize holds the group, its datasets,
//! their import jobs and its event trackers, and Amazon S3 holds the object an import job
//! reads, so an import can only COMPLETE where both are mounted), so the suite lives here
//! rather than in either of them.
//!
//! The suites are TABLES: the rows vary what the rule generalises over — which source of
//! interaction records the group has and whether it arrived (no dataset, another dataset
//! type, an `INTERACTIONS` dataset with no import, one whose import failed, one whose
//! import completed, an event tracker), the shape of the expression, and the symbol and
//! the position of a character the language does not read.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request is signed for, and every minted ARN is scoped to.
const REGION: &str = "us-east-1";
/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";
/// The bucket an import job reads its records out of.
const BUCKET: &str = "personalize-filter-precondition";

/// What a `CreateFilter` whose dataset group holds no interaction records answers.
///
/// Written out here rather than read from the service crate: a test that asserted the
/// handler's own constant would pass whatever that constant said.
const INTERACTIONS_REQUIRED_MESSAGE: &str = "Dataset group must contain an INTERACTIONS \
     dataset with successfully completed dataset import job or EVENT_INTERACTIONS";

/// The refusal a character the expression language does not read carries, for that
/// character. Written out for the reason above.
fn invalid_input_symbol_message(symbol: char) -> String {
    format!("Invalid input symbol: '{symbol}'")
}

/// How long a poll waits for a resource to leave the statuses a create is still working
/// in. Generous rather than tight: it is only the point at which the work is declared
/// never to have happened, so a loaded host must not be what trips it.
const PATIENCE: Duration = Duration::from_secs(60);
/// How often a poll asks again.
const POLL: Duration = Duration::from_millis(20);
/// The statuses a resource reports while its create is outstanding. Source: the model's
/// own documentation for every create of this service — "CREATE PENDING > CREATE
/// IN_PROGRESS > ACTIVE -or- CREATE FAILED".
const TRANSIENT_STATUSES: &[&str] = &["CREATE PENDING", "CREATE IN_PROGRESS"];

/// The services this question spans, over ONE shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signatures are verified by the emulator, never by a service; this suite is
        // about behaviour, so it signs with a fixed scope and verifies nothing.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_personalize_amazonpersonalize::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router }
    }

    /// One control-plane call (`AmazonPersonalize.<Operation>` on `POST /`).
    async fn control(&self, operation: &str, body: &Value) -> (u16, Value) {
        let host = format!("personalize.{REGION}.amazonaws.com");
        let request = ::http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("AmazonPersonalize.{operation}"))
            .header("authorization", sigv4("personalize"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// A control-plane call that must succeed.
    async fn control_ok(&self, operation: &str, body: &Value, case: &str) -> Value {
        let (status, parsed) = self.control(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to answer 200, got {status} {parsed}"
        );
        parsed
    }

    /// One path-style request to the emulator's S3 front door.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> u16 {
        let host = format!("s3.{REGION}.amazonaws.com");
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("authorization", sigv4("s3"))
            .body(Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        self.router.dispatch(request).await.status().as_u16()
    }

    /// Create [`BUCKET`] and upload one import file to it.
    ///
    /// The create names NO `LocationConstraint`: the unspecified constraint stands for
    /// us-east-1, which is the Region this suite signs for.
    async fn upload(&self, key: &str, body: &str) {
        let created = self.s3("PUT", &format!("/{BUCKET}"), &[]).await;
        assert!(
            created == 200 || created == 409,
            "CreateBucket {BUCKET}: expected 200 or 409, got {created}"
        );
        let put = self
            .s3("PUT", &format!("/{BUCKET}/{key}"), body.as_bytes())
            .await;
        assert_eq!(put, 200, "PutObject {key}: expected 200, got {put}");
    }

    /// Create a dataset group and return its ARN once it reports a terminal status.
    async fn group(&self, suffix: &str, case: &str) -> String {
        let arn = self
            .control_ok(
                "CreateDatasetGroup",
                &json!({"name": format!("group-{suffix}")}),
                case,
            )
            .await["datasetGroupArn"]
            .as_str()
            .expect("a datasetGroupArn")
            .to_string();
        let group = self
            .terminal("DescribeDatasetGroup", "datasetGroupArn", "datasetGroup", &arn, case)
            .await;
        assert_eq!(
            group["status"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: the dataset group must become ACTIVE, got {group}"
        );
        arn
    }

    /// Create a schema and a dataset of `dataset_type` in `group`, and return the dataset
    /// ARN.
    async fn dataset(&self, suffix: &str, group: &str, dataset_type: &str, case: &str) -> String {
        let schema = self
            .control_ok(
                "CreateSchema",
                &json!({
                    "name": format!("{}-{suffix}", dataset_type.to_ascii_lowercase()),
                    "schema": avro_schema(dataset_type),
                }),
                case,
            )
            .await["schemaArn"]
            .as_str()
            .expect("a schemaArn")
            .to_string();
        self.control_ok(
            "CreateDataset",
            &json!({
                "name": format!("{}-{suffix}", dataset_type.to_ascii_lowercase()),
                "datasetGroupArn": group,
                "datasetType": dataset_type,
                "schemaArn": schema,
            }),
            case,
        )
        .await["datasetArn"]
            .as_str()
            .expect("a datasetArn")
            .to_string()
    }

    /// Submit one dataset import job reading `key` out of [`BUCKET`], and return the
    /// terminal status it settles in.
    async fn import(&self, suffix: &str, dataset: &str, key: &str, case: &str) -> String {
        let job = self
            .control_ok(
                "CreateDatasetImportJob",
                &json!({
                    "jobName": format!("import-{suffix}"),
                    "datasetArn": dataset,
                    "dataSource": {"dataLocation": format!("s3://{BUCKET}/{key}")},
                    "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/PersonalizeImport"),
                }),
                case,
            )
            .await["datasetImportJobArn"]
            .as_str()
            .expect("a datasetImportJobArn")
            .to_string();
        let reported = self
            .terminal(
                "DescribeDatasetImportJob",
                "datasetImportJobArn",
                "datasetImportJob",
                &job,
                case,
            )
            .await;
        reported["status"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// Register an event tracker for `group`, which is what gives the group its
    /// event-interactions stream.
    async fn tracker(&self, suffix: &str, group: &str, case: &str) {
        self.control_ok(
            "CreateEventTracker",
            &json!({"name": format!("tracker-{suffix}"), "datasetGroupArn": group}),
            case,
        )
        .await;
    }

    /// Read `operation` until the resource it reports leaves the transient statuses, and
    /// return that resource.
    ///
    /// Waits for the CONDITION, never for a span: a create of this emulator finishes its
    /// own work before it answers, so the first read is already terminal and this returns
    /// on its first iteration.
    async fn terminal(
        &self,
        operation: &str,
        arn_member: &str,
        resource_member: &str,
        arn: &str,
        case: &str,
    ) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let resource = self
                .control_ok(operation, &json!({arn_member: arn}), case)
                .await[resource_member]
                .clone();
            let reported = resource["status"].as_str().unwrap_or("").to_string();
            if !reported.is_empty() && !TRANSIENT_STATUSES.contains(&reported.as_str()) {
                return resource;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {operation} never left {reported:?}"
            );
            ::tokio::time::sleep(POLL).await;
        }
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the
/// routing signal the router reads first.
fn sigv4(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// The wire error code of a failure response, with any namespace stripped.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` member of a failure response.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The fields a dataset schema of each dataset type must declare. Source: the Amazon
/// Personalize required-field sets the control plane checks a document against
/// (`REQUIRED_SCHEMA_FIELDS` in the service crate's `src/state.rs`), upper-cased the way
/// an import file's header spells a column.
fn required_fields(dataset_type: &str) -> &'static [&'static str] {
    match dataset_type {
        "Interactions" => &["USER_ID", "ITEM_ID", "TIMESTAMP"],
        "Items" => &["ITEM_ID"],
        "Users" => &["USER_ID"],
        other => panic!("no required-field set is written here for {other}"),
    }
}

/// An Avro record schema for `dataset_type`, in the Avro JSON format `CreateSchema`
/// documents, declaring exactly the fields that type requires.
fn avro_schema(dataset_type: &str) -> String {
    let fields: Vec<Value> = required_fields(dataset_type)
        .iter()
        .map(|field| json!({"name": field, "type": "string"}))
        .collect();
    json!({
        "type": "record",
        "name": dataset_type,
        "namespace": "com.amazonaws.personalize.schema",
        "fields": fields,
        "version": "1.0",
    })
    .to_string()
}

/// One interactions import file: three users interacting with three items.
fn interactions_csv() -> String {
    let mut csv = String::from("USER_ID,ITEM_ID,TIMESTAMP\n");
    for index in 1..=3 {
        csv.push_str(&format!(
            "user-{index},item-{index},{}\n",
            1_700_000_000 + index
        ));
    }
    csv
}

/// The expressions every state row is tried with: an entity-dataset clause, an
/// interaction-dataset clause (the published "exclude what this user already interacted
/// with" filter) and a two-clause expression with a `$placeholder`. All three are
/// expressions the language reads, so what refuses them is the group's state alone.
const EXPRESSIONS: &[(&str, &str)] = &[
    (
        "an Items clause",
        "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\")",
    ),
    (
        "an Interactions clause",
        "EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN (\"purchase\")",
    ),
    (
        "two clauses and a placeholder",
        "INCLUDE ItemID WHERE Items.GENRE IN ($GENRE) AND Items.YEAR >= 2010",
    ),
];

/// The ARN `CreateFilter` mints for `name`.
fn filter_arn(name: &str) -> String {
    format!("arn:aws:personalize:{REGION}:{ACCOUNT}:filter/{name}")
}

/// Assert `CreateFilter` with `expression` in `group` is refused with exactly `message`,
/// and that it left no filter behind.
async fn assert_filter_refused(
    emulator: &Emulator,
    group: &str,
    name: &str,
    expression: &str,
    message: &str,
    case: &str,
) {
    let (status, body) = emulator
        .control(
            "CreateFilter",
            &json!({
                "name": name,
                "datasetGroupArn": group,
                "filterExpression": expression,
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected CreateFilter to answer HTTP 400, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidInputException",
        "case {case:?}: expected InvalidInputException, got {body}"
    );
    let reported = error_message(&body);
    assert_eq!(
        reported, message,
        "case {case:?}: expected the refusal {message:?}, got {reported:?}"
    );

    // A refused create leaves nothing behind: the ARN its name would have minted
    // addresses no filter.
    let (status, read) = emulator
        .control("DescribeFilter", &json!({"filterArn": filter_arn(name)}))
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected DescribeFilter to refuse the ARN of the filter that was \
         not created, got {status} {read}"
    );
    assert_eq!(
        error_code(&read),
        "ResourceNotFoundException",
        "case {case:?}: expected the refused create to store no filter, got {read}"
    );
}

/// Assert `CreateFilter` with `expression` in `group` is accepted, and that
/// `DescribeFilter` reports the expression back.
async fn assert_filter_created(
    emulator: &Emulator,
    group: &str,
    name: &str,
    expression: &str,
    case: &str,
) {
    let created = emulator
        .control_ok(
            "CreateFilter",
            &json!({
                "name": name,
                "datasetGroupArn": group,
                "filterExpression": expression,
            }),
            case,
        )
        .await;
    assert_eq!(
        created["filterArn"].as_str(),
        Some(filter_arn(name).as_str()),
        "case {case:?}: expected the minted filter ARN, got {created}"
    );
    let read = emulator
        .terminal(
            "DescribeFilter",
            "filterArn",
            "filter",
            &filter_arn(name),
            case,
        )
        .await;
    assert_eq!(
        read["filterExpression"].as_str(),
        Some(expression),
        "case {case:?}: expected DescribeFilter to report the expression as created, got {read}"
    );
    assert_eq!(
        read["datasetGroupArn"].as_str(),
        Some(group),
        "case {case:?}: expected the filter to belong to the group it was created in, got {read}"
    );
}

#[tokio::test]
async fn a_filter_is_refused_until_its_dataset_group_holds_interaction_records() {
    let emulator = Emulator::new();
    let case = "the interactions precondition";

    // Rows that hold NO interaction records, varying what the group does hold.
    let no_records = "a group holding nothing";
    let empty = emulator.group("empty", no_records).await;

    let other_type = "a group holding an Items dataset only";
    let items_only = emulator.group("items-only", other_type).await;
    emulator
        .dataset("items-only", &items_only, "Items", other_type)
        .await;

    let not_imported = "a group whose INTERACTIONS dataset has no import job";
    let unimported = emulator.group("unimported", not_imported).await;
    emulator
        .dataset("unimported", &unimported, "Interactions", not_imported)
        .await;

    let failed_import = "a group whose INTERACTIONS import job failed";
    let failed = emulator.group("failed", failed_import).await;
    let failed_dataset = emulator
        .dataset("failed", &failed, "Interactions", failed_import)
        .await;
    // Nothing is uploaded under this key, so the job's own work cannot be carried out and
    // it settles in the documented failure status — which is not a COMPLETED import.
    let status = emulator
        .import("failed", &failed_dataset, "absent/nothing-here.csv", failed_import)
        .await;
    assert_eq!(
        status, "CREATE FAILED",
        "case {failed_import:?}: the import job must settle in CREATE FAILED, got {status:?}"
    );

    for (row, (group_case, group)) in [
        (no_records, &empty),
        (other_type, &items_only),
        (not_imported, &unimported),
        (failed_import, &failed),
    ]
    .iter()
    .enumerate()
    {
        for (index, (expression_case, expression)) in EXPRESSIONS.iter().enumerate() {
            assert_filter_refused(
                &emulator,
                group,
                &format!("refused-{row}-{index}"),
                expression,
                INTERACTIONS_REQUIRED_MESSAGE,
                &format!("{group_case}, with {expression_case}"),
            )
            .await;
        }
    }

    // The group that DOES hold bulk interaction records: the same expressions are
    // accepted, so the refusals above belong to the group's state and not to the
    // expressions.
    let bulk = "a group whose INTERACTIONS import completed";
    let imported = emulator.group("imported", bulk).await;
    let interactions = emulator
        .dataset("imported", &imported, "Interactions", bulk)
        .await;
    emulator.upload("imported/interactions.csv", &interactions_csv()).await;
    let status = emulator
        .import(
            "imported",
            &interactions,
            "imported/interactions.csv",
            bulk,
        )
        .await;
    assert_eq!(
        status, "ACTIVE",
        "case {bulk:?}: the import job must complete, got {status:?}"
    );
    for (index, (expression_case, expression)) in EXPRESSIONS.iter().enumerate() {
        assert_filter_created(
            &emulator,
            &imported,
            &format!("bulk-{index}"),
            expression,
            &format!("{bulk}, with {expression_case}"),
        )
        .await;
    }

    // The OTHER source the precondition names: the event-interactions stream an event
    // tracker gives the group. This group holds no dataset at all, so the tracker is the
    // only reason the filter is taken.
    let streamed = "a group with an event tracker and no datasets";
    let tracked = emulator.group("tracked", streamed).await;
    for (index, (expression_case, expression)) in EXPRESSIONS.iter().enumerate() {
        assert_filter_refused(
            &emulator,
            &tracked,
            &format!("before-tracker-{index}"),
            expression,
            INTERACTIONS_REQUIRED_MESSAGE,
            &format!("{streamed}, before the tracker exists, with {expression_case}"),
        )
        .await;
    }
    emulator.tracker("tracked", &tracked, streamed).await;
    for (index, (expression_case, expression)) in EXPRESSIONS.iter().enumerate() {
        assert_filter_created(
            &emulator,
            &tracked,
            &format!("streamed-{index}"),
            expression,
            &format!("{streamed}, with {expression_case}"),
        )
        .await;
    }

    // A group that does not exist is still answered as a group that does not exist, so
    // the precondition did not swallow the not-found class.
    let absent = format!("arn:aws:personalize:{REGION}:{ACCOUNT}:dataset-group/never-created");
    let (status, body) = emulator
        .control(
            "CreateFilter",
            &json!({
                "name": "for-an-absent-group",
                "datasetGroupArn": absent,
                "filterExpression": EXPRESSIONS[0].1,
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected CreateFilter against an absent group to answer 400, got \
         {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "case {case:?}: expected the group's not-found class, got {body}"
    );
}

/// One row of the input-symbol table: the character the language does not read and the
/// expression it was written into.
struct SymbolRow {
    case: &'static str,
    symbol: char,
    expression: &'static str,
}

/// Characters that are NOT part of the filter-expression language, and where they can
/// appear.
///
/// Source: the alphabet the language is written in — a keyword's and an identifier's
/// letters, a number's digits, the `_` and `.` of a `<Dataset>.<COLUMN>` reference, the
/// `$` of a placeholder, the sign and decimal point of a number, the `( ) ,` of a value
/// set, the `= <> < <= > >=` comparisons, double quotes and whitespace (the grammar in
/// the Amazon Personalize Runtime crate's `src/state/filter.rs` module docs). Every row
/// below writes a character outside it; the recorded case the issue cited is `^`.
const UNREAD_SYMBOLS: &[SymbolRow] = &[
    SymbolRow {
        case: "a caret where a clause should start",
        symbol: '^',
        expression: "EXCLUDE ItemID WHERE ^ IN (\"Comedy\")",
    },
    SymbolRow {
        case: "a caret inside a column reference",
        symbol: '^',
        expression: "EXCLUDE ItemID WHERE Items.GEN^RE IN (\"Comedy\")",
    },
    SymbolRow {
        case: "a caret at the very start of the expression",
        symbol: '^',
        expression: "^EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\")",
    },
    SymbolRow {
        case: "a percent sign in a bare value",
        symbol: '%',
        expression: "EXCLUDE ItemID WHERE Items.GENRE IN (Com%edy)",
    },
    SymbolRow {
        case: "an ampersand between clauses",
        symbol: '&',
        expression: "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\") & Items.YEAR >= 2010",
    },
    SymbolRow {
        case: "a semicolon ending the expression",
        symbol: ';',
        expression: "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\");",
    },
    SymbolRow {
        case: "a brace around a clause",
        symbol: '{',
        expression: "EXCLUDE ItemID WHERE {Items.GENRE} IN (\"Comedy\")",
    },
    SymbolRow {
        case: "a backslash in a column reference",
        symbol: '\\',
        expression: "EXCLUDE ItemID WHERE Items\\GENRE IN (\"Comedy\")",
    },
];

/// Expressions whose only unusual characters are inside a QUOTED literal, which is the
/// value's own text rather than input of the language: each must be accepted.
const QUOTED_LITERALS: &[(&str, &str)] = &[
    (
        "a slash and a percent sign in a quoted value",
        "EXCLUDE ItemID WHERE Items.GENRE IN (\"Sci-Fi/Fantasy 100%\")",
    ),
    (
        "a caret in a quoted value",
        "EXCLUDE ItemID WHERE Items.GENRE IN (\"^Comedy^\")",
    ),
    (
        "braces and a semicolon in a quoted value",
        "INCLUDE ItemID WHERE Items.GENRE IN (\"{Drama};\")",
    ),
];

#[tokio::test]
async fn an_input_symbol_the_expression_language_does_not_read_is_named_in_the_refusal() {
    let emulator = Emulator::new();

    // A group that holds interaction records, so nothing here is refused for its state,
    // and one that holds none, so the ORDER of the two screens is pinned: the symbol is
    // what the reply names even where the group's own precondition is unmet too.
    let populated_case = "a group holding interaction records";
    let populated = emulator.group("symbols-populated", populated_case).await;
    let interactions = emulator
        .dataset(
            "symbols-populated",
            &populated,
            "Interactions",
            populated_case,
        )
        .await;
    emulator
        .upload("symbols/interactions.csv", &interactions_csv())
        .await;
    let status = emulator
        .import(
            "symbols-populated",
            &interactions,
            "symbols/interactions.csv",
            populated_case,
        )
        .await;
    assert_eq!(
        status, "ACTIVE",
        "case {populated_case:?}: the import job must complete, got {status:?}"
    );

    let bare_case = "a group holding no interaction records";
    let bare = emulator.group("symbols-bare", bare_case).await;

    for (index, row) in UNREAD_SYMBOLS.iter().enumerate() {
        let expected = invalid_input_symbol_message(row.symbol);
        for (tag, group_case, group) in [
            ("populated", populated_case, &populated),
            ("bare", bare_case, &bare),
        ] {
            assert_filter_refused(
                &emulator,
                group,
                &format!("symbol-{index}-{tag}"),
                row.expression,
                &expected,
                &format!("{}, in {group_case}", row.case),
            )
            .await;
        }
    }

    // The same characters INSIDE a quoted literal are the value's text, not input of the
    // language, so the expression reads and the filter is created.
    for (index, (case, expression)) in QUOTED_LITERALS.iter().enumerate() {
        assert_filter_created(
            &emulator,
            &populated,
            &format!("quoted-{index}"),
            expression,
            case,
        )
        .await;
    }
}
