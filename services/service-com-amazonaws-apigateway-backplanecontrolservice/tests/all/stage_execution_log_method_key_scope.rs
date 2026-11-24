// SPDX-License-Identifier: CC0-1.0

//! WHICH `methodSettings` entry of a deployed REST stage gates the execution log of a request,
//! where the stage carries two SIBLING resources each served by its own `AWS` service
//! integration — one the stage performs, one it cannot perform at all.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! > Keys ... are method paths defined as `{resource_path}/{http_method}` for an individual
//! > method override, or `/*/*` for overriding all methods in the stage", and
//! > `MethodSetting$loggingLevel` ("specifies the logging level for this method ... valid values
//! > are `OFF`, `ERROR`, and `INFO`") and `MethodSetting$dataTraceEnabled` ("specifies whether
//! > data trace logging is enabled for this method") are settings OF one such entry
//! > (`smithy_query(query="shape", shape="MethodSetting")`,
//! > `smithy_query(query="doc", shape="Stage", member="methodSettings")`). So the pair in force
//! > for a request is the entry keyed by the method it reached where the stage carries one and
//! > the stage-wide entry otherwise, and that ONE entry decides what the request leaves in
//! > `API-Gateway-Execution-Logs_{api-id}/{stage}`: nothing at `OFF` or with no entry in force
//! > at all, at `ERROR` only a request the stage answered WITH an error and of it only the
//! > entries reporting the failure, at `INFO` those and the informational steps beside them, and
//! > the payload lines of the transformation points the request crossed only where that same
//! > entry's own data trace is on. Two methods of one stage are therefore gated independently of
//! > each other: an entry keyed by one resource's method decides nothing about a request to a
//! > sibling resource, whether that request was SERVED through its integration or stopped by a
//! > configuration error of it.
//!
//! The rows differ along what the rule generalises over: WHICH of the two methods carries an
//! entry of its own (the served one, the failing one, both, neither), the level that entry
//! carries against the level behind it (a per-method entry over no stage-wide entry at all, over
//! a stage-wide `OFF`, and under a stage-wide `INFO`), and the data trace of the entry in force
//! against the data trace of the entry it overrides — the last of which is what shows the entry
//! in force is the method's own entry WHOLE rather than the stage-wide one amended by it.
//!
//! What the sibling suites pin is the other half of each axis: the level and the data trace of
//! the entry in force over this same pair of service integrations
//! ([`crate::stage_execution_log_service_integration_levels`]), and per-method keys over the
//! VERBS of one resource proxied to a function
//! ([`crate::stage_method_level_logging_level`]). What this suite adds is the per-RESOURCE half:
//! two sibling resources of one stage, one of which the stage cannot serve at all, each gated by
//! its own key.
//!
//! Only a cross-service suite can pin this: the api, its stage's method settings and the
//! emission of the trace are API Gateway's, the table the served integration calls is Amazon
//! DynamoDB's, both execution roles and the account's CloudWatch role are AWS IAM's, and the
//! group the trace lands in is CloudWatch Logs' — all four on ONE engine, as
//! `core/router/src/lib.rs` mounts them. Every assertion reads the outcome back through
//! CloudWatch Logs' own `FilterLogEvents`, so a record that did not reach that store cannot
//! pass.
//!
//! Nothing of the api is stood up here: the table, the two methods, the invoke and the
//! judgement of one request's trace are
//! [`crate::stage_execution_log_service_integration_levels`]'s. What this suite adds is the
//! TABLE of entries and the keys they are patched under.

use serde_json::{json, Value};

use crate::account_logging_role::{install_cloudwatch_role, role_arn};
use crate::dynamodb_service_integration::{given_role, given_table};
use crate::execute_api_data_plane::deploy;
use crate::rest_api_lifecycle::{create_api, gateway_with_dynamodb_iam_and_logs, Gateway};
use crate::stage_access_logging::{traced_line, traced_payload, PAYLOAD_PREFIXES};
use crate::stage_execution_log_service_integration_levels::{
    assert_trace, given_method, request_template, served, Expected, BROKEN_PART,
    CONFIGURATION_ERROR_STATUS, SERVED_PART, STAGE, TABLE,
};

/// The region the integration's `uri` names the table action in, which is the Region the grant
/// its execution role carries is written for.
const REGION: &str = "us-east-1";

/// The verb both methods of the stage are put on, which is the half of a `methodSettings` key
/// that follows the resource path.
const VERB: &str = "GET";

/// The `methodSettings` key that names every method of the stage: the whole-stage method path
/// `Stage$methodSettings` documents, as a key.
const EVERY_METHOD_KEY: &str = "*/*";

/// The role the SERVED method's integration is made under, granted the one action it makes.
const ROLE: &str = "apigateway-dynamodb-key-scope";

/// The role the FAILING method's integration names: a well-formed role ARN of this account that
/// IAM holds nothing under, so no call can be made under it and the integration is a
/// configuration error.
const ABSENT_ROLE: &str = "apigateway-dynamodb-key-scope-absent";

/// The account's CloudWatch role, which is what API Gateway writes a stage's execution log as:
/// nothing is delivered before it is installed.
const CLOUDWATCH_ROLE: &str = "apigw-method-key-scope-logs";

/// One `methodSettings` entry a row installs: the two settings of it this rule is about.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// The entry's `loggingLevel`.
    level: &'static str,
    /// The `dataTraceEnabled` patched beside it — always patched explicitly, so what a row
    /// asserts is the entry it named rather than the default an unnamed member would take.
    data_trace: bool,
}

/// One row of the scoping table: the entries the stage carries, keyed by the methods they are
/// patched for.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// The stage-wide `*/*` entry, or `None` for a stage carrying none.
    stage_wide: Option<Entry>,
    /// The entry keyed by the method of the resource the stage SERVES, or `None` for a method
    /// left to the stage-wide entry.
    served: Option<Entry>,
    /// The entry keyed by the method whose integration cannot be performed at all.
    broken: Option<Entry>,
}

impl Row {
    /// The entries this row patches, each with the key it is patched under — the per-method keys
    /// first, so the assertion reads them in the order a reader of the patch wrote them.
    fn entries(&self) -> Vec<(String, Entry)> {
        let mut entries: Vec<(String, Entry)> = Vec::new();
        if let Some(entry) = self.served {
            entries.push((method_key(SERVED_PART), entry));
        }
        if let Some(entry) = self.broken {
            entries.push((method_key(BROKEN_PART), entry));
        }
        if let Some(entry) = self.stage_wide {
            entries.push((EVERY_METHOD_KEY.to_string(), entry));
        }
        entries
    }

    /// The entry in force for the method of `path_part`: that method's own entry where this row
    /// patched one, and the stage-wide entry otherwise.
    fn in_force(&self, path_part: &str) -> Option<Entry> {
        let own = if path_part == SERVED_PART {
            self.served
        } else {
            self.broken
        };
        own.or(self.stage_wide)
    }
}

/// The `methodSettings` key of the `VERB` method of `/{path_part}`: the method path
/// `{resource_path}/{http_method}` as the patch POINTER spells it, where the resource path's own
/// leading separator is the JSON Pointer escape `~1` because inside a pointer that path is one
/// reference token.
fn method_key(path_part: &str) -> String {
    format!("~1{path_part}/{VERB}")
}

/// The trace the rule owes one request: the entry in force decides it, and an entry in force at
/// all is what there has to be for anything to be written — no entry writes nothing, exactly as
/// `OFF` does.
fn expected(entry: Option<Entry>, status: u16, reached_backend: bool) -> Expected {
    let (level, data_trace) = match entry {
        Some(entry) => (entry.level, entry.data_trace),
        None => ("OFF", false),
    };
    Expected::of(level, data_trace, status, reached_backend)
}

/// Install this row's entries under the keys it patches them by, and assert the stage REPORTS
/// exactly those entries: the key a patch stored is the key the data plane resolves the gating
/// by, so a stage reporting other keys or other values is gating other methods than the caller
/// named.
async fn gate(gateway: &Gateway, api: &str, row: &Row) {
    let case = row.case;
    let entries = row.entries();
    let operations: Vec<Value> = entries
        .iter()
        .flat_map(|(key, entry)| {
            [
                json!({
                    "op": "replace",
                    "path": format!("/{key}/logging/loglevel"),
                    "value": entry.level,
                }),
                json!({
                    "op": "replace",
                    "path": format!("/{key}/logging/dataTrace"),
                    "value": entry.data_trace.to_string(),
                }),
            ]
        })
        .collect();
    let patched = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({ "patchOperations": operations })),
        )
        .await;
    assert_eq!(
        patched.status, 200,
        "case {case:?}: expected UpdateStage to answer 200, got {} with {:?}",
        patched.status, patched.body
    );

    let read = gateway
        .call("GET", &format!("/restapis/{api}/stages/{STAGE}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected GetStage to answer 200, got {} with {:?}",
        read.status, read.body
    );
    let mut reported: Vec<String> = read
        .body
        .get("methodSettings")
        .and_then(Value::as_object)
        .map(|settings| settings.keys().cloned().collect())
        .unwrap_or_default();
    reported.sort();
    let mut keys: Vec<String> = entries.iter().map(|(key, _)| key.clone()).collect();
    keys.sort();
    assert_eq!(
        reported, keys,
        "case {case:?}: expected the stage to report the methodSettings keys {keys:?}, got \
         {reported:?} in {:?}",
        read.body
    );
    for (key, entry) in &entries {
        let settings = &read.body["methodSettings"][key];
        assert_eq!(
            (&settings["loggingLevel"], &settings["dataTraceEnabled"]),
            (&json!(entry.level), &json!(entry.data_trace)),
            "case {case:?}: expected the entry {key:?} to carry loggingLevel {:?} with \
             dataTraceEnabled {:?}, got {settings:?}",
            entry.level,
            entry.data_trace
        );
    }
}

/// The rows: a per-method entry over no stage-wide entry, over a stage-wide `OFF` on each of the
/// two methods in turn, a per-method `OFF` under a stage-wide `INFO`, both methods carrying their
/// own entry, and a per-method entry whose DATA TRACE differs from the stage-wide one's.
const ROWS: &[Row] = &[
    Row {
        case: "the served method at INFO with its data trace on and no stage-wide entry at all",
        stage_wide: None,
        served: Some(Entry {
            level: "INFO",
            data_trace: true,
        }),
        broken: None,
    },
    Row {
        case: "the stage-wide entry OFF with the served method raised to INFO",
        stage_wide: Some(Entry {
            level: "OFF",
            data_trace: false,
        }),
        served: Some(Entry {
            level: "INFO",
            data_trace: true,
        }),
        broken: None,
    },
    Row {
        case: "the stage-wide entry OFF with the failing method at ERROR",
        stage_wide: Some(Entry {
            level: "OFF",
            data_trace: false,
        }),
        served: None,
        broken: Some(Entry {
            level: "ERROR",
            data_trace: false,
        }),
    },
    Row {
        case: "the stage-wide entry INFO with the served method turned OFF",
        stage_wide: Some(Entry {
            level: "INFO",
            data_trace: false,
        }),
        served: Some(Entry {
            level: "OFF",
            data_trace: false,
        }),
        broken: None,
    },
    Row {
        case: "each method its own entry: the served one at ERROR, the failing one at INFO",
        stage_wide: None,
        served: Some(Entry {
            level: "ERROR",
            data_trace: false,
        }),
        broken: Some(Entry {
            level: "INFO",
            data_trace: true,
        }),
    },
    Row {
        case: "the stage-wide entry at INFO with its data trace on and the served method's own \
               entry at INFO with its data trace off",
        stage_wide: Some(Entry {
            level: "INFO",
            data_trace: true,
        }),
        served: Some(Entry {
            level: "INFO",
            data_trace: false,
        }),
        broken: None,
    },
];

/// RULE: the `methodSettings` entry keyed by the method a request reached is the entry that gates
/// its execution log — the stage-wide entry gating exactly the methods carrying none of their own
/// — and that one entry's level and data trace decide what the request leaves in the stage's
/// execution log group, whether the stage served it through its integration or could not perform
/// that integration at all.
///
/// Driven on a MULTI-THREADED runtime, as the assembled emulator serves a request: the stage's
/// request awaits a call this same process answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_entry_keyed_by_the_method_gates_what_its_requests_leave() {
    let gateway = gateway_with_dynamodb_iam_and_logs();
    install_cloudwatch_role(&gateway, CLOUDWATCH_ROLE).await;
    given_table(&gateway, TABLE).await;
    // The served method's execution role, granted the one table action its integration makes.
    let granted = given_role(
        &gateway,
        ROLE,
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "dynamodb:UpdateItem",
                "Resource": format!(
                    "arn:aws:dynamodb:{REGION}:{account}:table/{TABLE}",
                    account = engine::emulator_account_id(),
                ),
            }],
        }),
    )
    .await;
    let absent = role_arn("/", ABSENT_ROLE);

    for (rank, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // Each row stands its own api up, so the entries that gate it are exactly the ones this
        // row patched and the group read back holds only this row's requests.
        let (api, root) = create_api(&gateway, &format!("method-key-scope-{rank}")).await;
        given_method(&gateway, &api, &root, SERVED_PART, &granted).await;
        given_method(&gateway, &api, &root, BROKEN_PART, &absent).await;
        deploy(&gateway, &api, STAGE).await;
        gate(&gateway, &api, row).await;

        // The request the stage SERVES through its integration: the marker is this row's, so the
        // document the backend was sent and the item the table holds are this call's.
        let marker = format!("marker-{rank}");
        let answered = served(&gateway, &api, case, SERVED_PART, &marker).await;
        assert_eq!(
            (answered.status, answered.body.as_str()),
            (200, format!(r#"{{"marker":"{marker}"}}"#).as_str()),
            "case {case:?}: expected the table's answer mapped to the api's document, got {} \
             with {:?}",
            answered.status,
            answered.body
        );
        let in_force = row.in_force(SERVED_PART);
        assert_trace(
            case,
            &answered.request_id,
            &answered.trace,
            &expected(in_force, answered.status, true),
        );
        if in_force.is_some_and(|entry| entry.level == "INFO" && entry.data_trace) {
            // The payload line of the integration's own request point, which only the entry in
            // force for THIS method can have written: the document its request template
            // rendered, carrying the value this request's own parameter did.
            let sent = traced_payload(
                &answered.trace,
                case,
                &traced_line(&answered.request_id, PAYLOAD_PREFIXES[1]),
            );
            assert_eq!(
                sent,
                request_template(&marker),
                "case {case:?}: expected the integration request as it was sent, got {sent:?}"
            );
        }

        // The sibling resource, whose integration names a role IAM holds nothing under: no call
        // is made, so no backend is reached and the stage answers a configuration error.
        let refused = served(&gateway, &api, case, BROKEN_PART, &marker).await;
        assert_eq!(
            refused.status, CONFIGURATION_ERROR_STATUS,
            "case {case:?}: expected HTTP {CONFIGURATION_ERROR_STATUS} for an integration whose \
             execution role does not exist, got {} with {:?}",
            refused.status, refused.body
        );
        assert_trace(
            case,
            &refused.request_id,
            &refused.trace,
            &expected(row.in_force(BROKEN_PART), refused.status, false),
        );
    }
}
