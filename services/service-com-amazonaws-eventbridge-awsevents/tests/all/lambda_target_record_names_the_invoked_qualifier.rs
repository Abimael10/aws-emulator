// SPDX-License-Identifier: CC0-1.0

//! WHICH function the invocation record of an EventBridge-delivered asynchronous invocation
//! NAMES, end to end over the wire: EventBridge, Lambda, Amazon SQS, IAM and the token service
//! on ONE engine, a real `python3.x` handler that returns, and the record read back off the
//! queue the function's own `DestinationConfig.OnSuccess` names.
//!
//! The RULE pinned here: a rule's function target is handed its event with the ASYNCHRONOUS
//! invocation type, and the invocation record that completion delivers reports
//! `requestContext.functionArn` as the function ARN carrying the QUALIFIER the invocation was
//! routed to — the version or alias the target named, and the unpublished version `$LATEST`
//! where it named none, because "specify a version or alias to invoke a published version of
//! the function" (`Invoke$Qualifier`,
//! `smithy_query(query="operation", operation="Invoke",
//! model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`)
//! makes an invocation that names none an invocation of the unpublished version. So the last
//! colon-separated segment of that ARN is never the function's own NAME: a consumer of
//! destination records tells the version that produced a result from that segment alone.
//!
//! The record is not modelled in Smithy — it is the runtime envelope Lambda writes — so the
//! fixed part of the value is the function's ARN and the member names; its Region, account and
//! function name come from the request, and its qualifier from the state the invocation
//! resolved against.
//!
//! The rows span that class rather than the one recorded case. They differ along the FORM of
//! the qualifier the rule's target ARN carries — none at all, a published version number, an
//! alias of that version, and an alias of a LATER version — so an emulator that echoed the
//! unqualified ARN, that qualified every record with the latest published version, or that
//! reported the version an alias resolves to in place of the alias itself fails here. Every row
//! publishes a version, so the unqualified row also pins that the record names the unpublished
//! version rather than the newest published one; the two alias rows route to different versions,
//! so the alias name and the version that RAN (which the record reports separately, as
//! `responseContext.executedVersion`) cannot be confused for one another.
//!
//! Every row states `MaximumRetryAttempts` 0 (`FunctionEventInvokeConfig$MaximumRetryAttempts`,
//! `@range(min: 0, max: 2)`), so each event has exactly one delivery attempt and the attempt
//! count a record reports is a known number.
//!
//! Only a suite mounting Lambda, Amazon SQS, IAM and the token service beside EventBridge can
//! pin this: EventBridge hands the event over, Lambda runs the handler and writes the record as
//! a session of the function's execution role, the token service vends that session, IAM holds
//! the role's grants, and the queue the record lands on is the queue a client polls.
//!
//! `lambda_target_async_destination_records` drives the same handover over the OUTCOMES a
//! handler reaches and the FORM a target delivers, and
//! `lambda_target_failure_destination_record` over the failure slot's authorization; what this
//! one adds is the qualifier the delivered record NAMES, for every form a target can address.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::handler_execution_role_publication::{
    Emulator, EVENTS_PRINCIPAL, FUNCTIONS_URI, LOOK_AGAIN,
};

/// Where the operations on an asynchronous-invocation configuration are bound
/// (`/2019-09-25/functions/{FunctionName}/event-invoke-config`).
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The `source` and `detail-type` every event this suite publishes carries, and all each rule
/// matches on — so a record on a destination belongs to this suite's own event.
const SOURCE: &str = "com.demo.record-qualifier";
const DETAIL_TYPE: &str = "Record Qualifier Probe";

/// The action an invocation of a function performs, which the grant admitting the rule's
/// handover has to name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The action the record's delivery to its destination queue performs, which the function's
/// execution role has to carry.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The name of the inline identity policy each row writes on its execution role.
const INLINE_POLICY: &str = "write-the-record";

/// The id of the one target each rule of a row carries, and the statement id of the grant each
/// row writes on its function.
const TARGET_ID: &str = "fn";
const STATEMENT_ID: &str = "events-invoke";

/// The retries every configuration below asks for: none, the floor of the member's own
/// `@range(min: 0, max: 2)`, so each event has exactly ONE delivery attempt.
const NO_RETRIES: i64 = 0;

/// The `requestContext.condition` of the record of an event whose handler RETURNED.
const CONDITION_SUCCESS: &str = "Success";

/// The status the `responseContext` of a record of an invocation that RAN reports.
const INVOKED_STATUS: i64 = 200;

/// The unpublished version, as the model spells it: the qualifier an invocation that names none
/// is routed to, and so the one such a record names.
const UNPUBLISHED_VERSION: &str = "$LATEST";

/// The versions the rows publish, as `PublishVersion` numbers them from one.
const FIRST_VERSION: &str = "1";
const SECOND_VERSION: &str = "2";

/// How long a row waits for work the CLOCK owes — the asynchronous delivery of the event, the
/// handler's run, the record that follows it.
///
/// A ceiling on PATIENCE and never a span anything here asserts on: every wait below looks again
/// until what it waits for is there or this deadline passes, so a loaded host is waited out and
/// only something that never arrives fails a row.
const RECORD_WAIT: Duration = Duration::from_secs(90);

/// The handler every row deploys: it answers a document built from what it was handed, so the
/// `responsePayload` of a record is the handler's own answer and names the row's event.
const ECHO_SOURCE: &str = r#"
def lambda_handler(event, context):
    detail = event.get("detail") or {}
    return {"echoed": detail.get("marker")}
"#;

/// The qualifier form a row's rule target addresses the function by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressed {
    /// No qualifier at all: the target names the function's unqualified ARN.
    Unqualified,
    /// The first published version, by number.
    Version,
    /// An alias of the first published version.
    AliasOfFirstVersion,
    /// An alias of a LATER published version.
    AliasOfSecondVersion,
}

impl Addressed {
    /// The qualifier the rule's target ARN carries, and `None` for the unqualified form.
    fn target_qualifier(self) -> Option<&'static str> {
        match self {
            Self::Unqualified => None,
            Self::Version => Some(FIRST_VERSION),
            Self::AliasOfFirstVersion => Some(FIRST_ALIAS),
            Self::AliasOfSecondVersion => Some(SECOND_ALIAS),
        }
    }

    /// The qualifier the record's `requestContext.functionArn` has to carry: the one the
    /// invocation was routed to, which for a target naming none is the unpublished version.
    fn record_qualifier(self) -> &'static str {
        self.target_qualifier().unwrap_or(UNPUBLISHED_VERSION)
    }

    /// The version the invocation RESOLVES to, which the record reports separately as
    /// `responseContext.executedVersion`: the version itself, the version an alias points at,
    /// and the unpublished version for a target naming no qualifier.
    fn executed_version(self) -> &'static str {
        match self {
            Self::Unqualified => UNPUBLISHED_VERSION,
            Self::Version | Self::AliasOfFirstVersion => FIRST_VERSION,
            Self::AliasOfSecondVersion => SECOND_VERSION,
        }
    }

    /// How many versions the row publishes before wiring its target: the second alias routes
    /// to a second version, so its row publishes one more.
    fn published_versions(self) -> usize {
        match self {
            Self::AliasOfSecondVersion => 2,
            _ => 1,
        }
    }

    /// The alias the row creates, and the version it points at — `None` for a row addressing
    /// no alias.
    fn alias(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::AliasOfFirstVersion => Some((FIRST_ALIAS, FIRST_VERSION)),
            Self::AliasOfSecondVersion => Some((SECOND_ALIAS, SECOND_VERSION)),
            _ => None,
        }
    }
}

/// The aliases the rows create, inside `Alias`'s own pattern (`^(?!^[0-9]+$)([a-zA-Z0-9-_]+)$`).
const FIRST_ALIAS: &str = "live";
const SECOND_ALIAS: &str = "blue";

/// One row: what it is called, and the qualifier form its target addresses.
struct Row {
    case: &'static str,
    addressed: Addressed,
}

/// The rows, spanning the forms a function target's ARN can carry.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a target naming the function's unqualified ARN",
            addressed: Addressed::Unqualified,
        },
        Row {
            case: "a target naming a published version by number",
            addressed: Addressed::Version,
        },
        Row {
            case: "a target naming an alias of that version",
            addressed: Addressed::AliasOfFirstVersion,
        },
        Row {
            case: "a target naming an alias of a later version",
            addressed: Addressed::AliasOfSecondVersion,
        },
    ]
}

/// The execution role `role_name`, trusted by Lambda and carrying the one grant a destination
/// delivery needs: the send on `destination`. The record is written as a SESSION of this role,
/// so that grant is what admits the write. Answers its ARN.
async fn execution_role(
    emulator: &Emulator,
    case: &str,
    role_name: &str,
    destination: &str,
) -> String {
    let role = emulator.execution_role(case, role_name).await;
    emulator
        .role_policy(
            case,
            role_name,
            INLINE_POLICY,
            Some(
                &json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": SEND_MESSAGE_ACTION,
                        "Resource": destination,
                    }],
                })
                .to_string(),
            ),
        )
        .await;
    role
}

/// The unqualified ARN Lambda assigned the function `name`, read back off its own
/// configuration.
async fn function_arn(emulator: &Emulator, case: &str, name: &str) -> String {
    let (status, _headers, configuration) = emulator
        .lambda("GET", &format!("{FUNCTIONS_URI}/{name}/configuration"), &[])
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading the configuration of {name:?}, got {status} \
         ({configuration})"
    );
    configuration["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {configuration}"))
        .to_string()
}

/// `PublishVersion` on `name`, answering the version it published.
///
/// Each call states its own description, because "Lambda doesn't publish a version if the
/// function's configuration and code haven't changed since the last version": a second publish
/// of an unchanged function would answer the version already held.
async fn publish_version(emulator: &Emulator, case: &str, name: &str, description: &str) -> String {
    let (status, _headers, published) = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/versions"),
            &serde_json::to_vec(&json!({ "Description": description }))
                .expect("the publish body serializes"),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from PublishVersion, got {status} ({published})"
    );
    published["Version"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Version, got {published}"))
        .to_string()
}

/// `CreateAlias`: the alias `alias` of `name`, pointing at `version`.
async fn create_alias(emulator: &Emulator, case: &str, name: &str, alias: &str, version: &str) {
    let (status, _headers, created) = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/aliases"),
            &serde_json::to_vec(&json!({ "Name": alias, "FunctionVersion": version }))
                .expect("the alias body serializes"),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateAlias, got {status} ({created})"
    );
    assert_eq!(
        created["FunctionVersion"],
        json!(version),
        "case {case:?}: expected the alias to point at {version:?}, got {created}"
    );
}

/// The request URI of an operation on the function `name`, with the `Qualifier` query parameter
/// a qualified row carries and none for the unqualified one.
fn qualified_uri(base: &str, name: &str, leaf: &str, qualifier: Option<&str>) -> String {
    match qualifier {
        Some(qualifier) => format!("{base}/{name}/{leaf}?Qualifier={qualifier}"),
        None => format!("{base}/{name}/{leaf}"),
    }
}

/// Grant this service the invocation of the qualifier the row addresses — a resource-based
/// policy is held per qualifier, so the grant names the one the target invokes — scoped by
/// `SourceArn` to the rule that makes the call.
async fn add_permission(
    emulator: &Emulator,
    case: &str,
    name: &str,
    qualifier: Option<&str>,
    rule_arn: &str,
) {
    let (status, _headers, answered) = emulator
        .lambda(
            "POST",
            &qualified_uri(FUNCTIONS_URI, name, "policy", qualifier),
            &serde_json::to_vec(&json!({
                "StatementId": STATEMENT_ID,
                "Action": INVOKE_ACTION,
                "Principal": EVENTS_PRINCIPAL,
                "SourceArn": rule_arn,
            }))
            .expect("the permission body serializes"),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from AddPermission, got {status} ({answered})"
    );
}

/// State the asynchronous-invocation configuration of the qualifier the row addresses — no
/// retries and the `OnSuccess` destination `on_success` names — and read it back through
/// `GetFunctionEventInvokeConfig`, which is where a client confirms what it configured.
async fn event_invoke_config(
    emulator: &Emulator,
    case: &str,
    name: &str,
    qualifier: Option<&str>,
    on_success: &str,
    configured_arn: &str,
) {
    let uri = qualified_uri(EVENT_INVOKE_CONFIG_URI, name, "event-invoke-config", qualifier);
    let (status, _headers, answered) = emulator
        .lambda(
            "PUT",
            &uri,
            &serde_json::to_vec(&json!({
                "MaximumRetryAttempts": NO_RETRIES,
                "DestinationConfig": { "OnSuccess": { "Destination": on_success } },
            }))
            .expect("the configuration body serializes"),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutFunctionEventInvokeConfig, got {status} \
         ({answered})"
    );
    let (status, _headers, read) = emulator.lambda("GET", &uri, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetFunctionEventInvokeConfig, got {status} \
         ({read})"
    );
    // The configuration is the one OF A QUALIFIER, so the read names the function qualified by
    // the one it was stated for — the same qualifier the record later names.
    assert_eq!(
        read["FunctionArn"],
        json!(configured_arn),
        "case {case:?}: expected the stored configuration to name {configured_arn}, got {read}"
    );
    assert_eq!(
        read["DestinationConfig"]["OnSuccess"],
        json!({ "Destination": on_success }),
        "case {case:?}: expected the stored configuration to report the success destination it \
         was given, got {read}"
    );
}

/// The rule matching this suite's event; answers its ARN, which is the source the grant names.
async fn rule(emulator: &Emulator, case: &str, name: &str) -> String {
    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    let created = emulator
        .events(
            case,
            "PutRule",
            &json!({ "Name": name, "EventPattern": pattern, "State": "ENABLED" }),
        )
        .await;
    created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {created}"))
        .to_string()
}

/// Wire `target_arn` as the only target of the rule `name`.
async fn put_target(emulator: &Emulator, case: &str, name: &str, target_arn: &str) {
    let wired = emulator
        .events(
            case,
            "PutTargets",
            &json!({
                "Rule": name,
                "Targets": [{ "Id": TARGET_ID, "Arn": target_arn }],
            }),
        )
        .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the function target {target_arn} to be accepted, got {wired}"
    );
}

/// Publish the one matching entry of a row, carrying `marker` so a record belongs to it.
async fn publish(emulator: &Emulator, case: &str, marker: &str) {
    let published = emulator
        .events(
            case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "marker": marker }).to_string(),
                }],
            }),
        )
        .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {published}"
    );
}

/// The ONE invocation record waiting on the queue at `url`, as the document its body holds —
/// looking again until it is there or [`RECORD_WAIT`] has passed, so nothing sleeps for a span.
async fn one_record(emulator: &Emulator, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + RECORD_WAIT;
    let mut delivered = loop {
        let held = emulator.raw_messages(case, url).await;
        if !held.is_empty() {
            break held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no invocation record ever reached the OnSuccess destination"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    // One receive answers only what it happened to pick up, so a second record written at the
    // same instant is taken off by another look before the count below is asserted.
    delivered.extend(emulator.raw_messages(case, url).await);
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: expected exactly one invocation record at the destination, got \
         {delivered:?}"
    );
    let body = delivered[0]["Body"].as_str().unwrap_or_default();
    serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: expected an invocation record document, got {body:?} ({error})")
    })
}

/// RULE: the invocation record an EventBridge-delivered asynchronous invocation completes into
/// names the function QUALIFIED by the qualifier that invocation was routed to — the version or
/// alias the target addressed, and the unpublished version where it addressed none — so the last
/// segment of `requestContext.functionArn` is never the function's own name.
#[tokio::test]
async fn an_eventbridge_lambda_targets_destination_record_names_the_invoked_qualifier() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();

        let (destination_url, destination_arn) =
            emulator.queue(case, &format!("record-qualifier-{index}")).await;
        let role_name = format!("record-qualifier-role-{index}");
        let role = execution_role(&emulator, case, &role_name, &destination_arn).await;

        let function_name = format!("records-its-qualifier-{index}");
        emulator
            .function(case, &function_name, &role, ECHO_SOURCE)
            .await;
        let unqualified = function_arn(&emulator, case, &function_name).await;

        // Every row publishes at least one version, so the unqualified row pins that the record
        // names the UNPUBLISHED version rather than the newest published one.
        for version in 1..=row.addressed.published_versions() {
            let published = publish_version(
                &emulator,
                case,
                &function_name,
                &format!("revision {version}"),
            )
            .await;
            assert_eq!(
                published,
                version.to_string(),
                "case {case:?}: expected PublishVersion to number the versions from one, got \
                 {published:?}"
            );
        }
        if let Some((alias, version)) = row.addressed.alias() {
            create_alias(&emulator, case, &function_name, alias, version).await;
        }

        // What the rule's target ARN is, and what the record it completes into has to name: the
        // same ARN for a qualified target, and the ARN of the unpublished version for a target
        // naming no qualifier at all.
        let target_arn = match row.addressed.target_qualifier() {
            Some(qualifier) => format!("{unqualified}:{qualifier}"),
            None => unqualified.clone(),
        };
        let recorded_arn = format!("{unqualified}:{}", row.addressed.record_qualifier());

        event_invoke_config(
            &emulator,
            case,
            &function_name,
            row.addressed.target_qualifier(),
            &destination_arn,
            &recorded_arn,
        )
        .await;

        let rule_name = format!("record-qualifier-rule-{index}");
        let rule_arn = rule(&emulator, case, &rule_name).await;
        add_permission(
            &emulator,
            case,
            &function_name,
            row.addressed.target_qualifier(),
            &rule_arn,
        )
        .await;
        put_target(&emulator, case, &rule_name, &target_arn).await;

        let marker = format!("marker-{index}");
        publish(&emulator, case, &marker).await;

        let record = one_record(&emulator, case, &destination_url).await;
        let context = &record["requestContext"];
        assert_eq!(
            context["condition"],
            json!(CONDITION_SUCCESS),
            "case {case:?}: expected the condition {CONDITION_SUCCESS:?}, got {record}"
        );
        assert_eq!(
            context["approximateInvokeCount"],
            json!(1),
            "case {case:?}: expected the one attempt no retry allows, got {record}"
        );
        // The qualifier the record names is the one the invocation was ROUTED to.
        assert_eq!(
            context["functionArn"],
            json!(recorded_arn),
            "case {case:?}: expected the record to name {recorded_arn}, got {record}"
        );
        // Stated as the rule itself: the last segment of that ARN is a qualifier and never the
        // function's own name, whichever form the target addressed.
        let named = context["functionArn"].as_str().unwrap_or_default();
        assert_eq!(
            named.rsplit(':').next(),
            Some(row.addressed.record_qualifier()),
            "case {case:?}: expected the ARN's last segment to be the qualifier {:?} rather \
             than the function name, got {named:?}",
            row.addressed.record_qualifier()
        );
        assert!(
            context["requestId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the invocation's own request id, got {record}"
        );

        // WHICH version ran is reported separately, so an alias is not read back as the version
        // it routes to, nor the other way round.
        let response = &record["responseContext"];
        assert_eq!(
            response["statusCode"],
            json!(INVOKED_STATUS),
            "case {case:?}: expected the status of a run that executed, got {record}"
        );
        assert_eq!(
            response["executedVersion"],
            json!(row.addressed.executed_version()),
            "case {case:?}: expected the executed version {:?}, got {record}",
            row.addressed.executed_version()
        );
        // `requestPayload` is what EventBridge delivered, and `responsePayload` the handler's
        // own answer to it — so the record read here belongs to this row's event.
        assert_eq!(
            record["requestPayload"]["detail"],
            json!({ "marker": marker }),
            "case {case:?}: expected the published detail in the request payload, got {record}"
        );
        assert_eq!(
            record["responsePayload"],
            json!({ "echoed": marker }),
            "case {case:?}: expected the handler's own answer as the response payload, got \
             {record}"
        );
    }
}
