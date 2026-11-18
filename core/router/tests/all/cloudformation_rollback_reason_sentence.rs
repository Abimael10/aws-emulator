// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a rolled-back stack reports, end to end over the awsQuery wire the assembled
//! emulator serves.
//!
//! RULE: the event CloudFormation records when it unwinds a failed create carries the
//! failure's OWN sentence, then ONE space, then the clause saying the rollback was asked for
//! — `<failure> Rollback requested by user.` The failure's text is reproduced character for
//! character and nothing is re-punctuated: a sentence this service composed already ends
//! itself, so the clause adds no second sentence end to it, and a period inside the failure
//! that came from a SUBMITTED template stands exactly where the template put it. The stack's
//! own `StackStatusReason` keeps the failure alone, which is the field a client parses; the
//! clause lives on the event stream. `CreateStackInput$OnFailure` documents `ROLLBACK` as the
//! default, which is why the clause says the rollback was requested.
//!
//! The rows span what the rule generalises over — WHICH failure the create reported and where
//! the periods in it come from: an import of an export nothing publishes, an export name a
//! live stack already holds, an AWS-specifically typed parameter whose value names no resource
//! of the account (which is Amazon EC2's half of the deployment, and why this suite runs on
//! the assembled router), a `Rules` assertion whose description stops mid-sentence, the same
//! assertion with a description that ends in its own period — the one reason where two
//! periods legitimately meet — and a transform that cannot apply a function where the template
//! puts it. Each row's expected reason is written out in full rather than composed by the
//! formatter under test, so the wording is pinned rather than restated.
//!
//! Every reason is read back through `DescribeStacks` and `DescribeStackEvents` — the
//! operations a client polls — so a deployment that did not really fail, or whose events the
//! service does not record, cannot pass.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long a poll of a stack's status waits between reads, and how many times it reads: a
/// create that fails records its failure and then its rollback, each a status transition, so
/// the deadline is far longer than the two of them together.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 240;

/// The clause CloudFormation appends to the failure on the rollback's own event.
const ROLLBACK_CLAUSE: &str = "Rollback requested by user.";

/// The export name no stack of the account publishes, which the first row imports.
const MISSING_EXPORT: &str = "rollback-wording-nothing-exports-this";

/// The export name the live exporter stack publishes, which the second row tries to publish
/// a second time.
const CLAIMED_EXPORT: &str = "rollback-wording-claimed-export";

/// The stack that publishes [`CLAIMED_EXPORT`].
const EXPORTER_STACK: &str = "rollback-wording-exporter";

/// A well-formed VPC id — Amazon EC2's long resource id form — that no VPC of the account
/// holds, which the typed-parameter row is given.
const ABSENT_VPC: &str = "vpc-0ff1ce0ff1ce0ff1c";

/// The parameter the typed-parameter row declares, named in that failure's sentence.
const VPC_PARAMETER: &str = "NetworkVpc";

/// The text one `Rules` row describes its assertion with: text of the SUBMITTER's that stops
/// mid-sentence, so the only sentence end in the reason is the one the service's own template
/// puts there.
const ASSERT_DESCRIPTION: &str = "the Tier parameter has to be gold";

/// The text the other `Rules` row describes its assertion with: text of the SUBMITTER's that
/// ENDS IN ITS OWN PERIOD, which is reproduced character for character — so that period and
/// the one ending the service's sentence both survive, and the clause still adds none.
const ASSERT_DESCRIPTION_ENDING_IN_A_PERIOD: &str =
    "the Tier parameter has to be gold. Pick one of the supported tiers.";

/// The emulator with the two services these deployments need, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One CloudFormation call over its awsQuery wire, as its HTTP status and body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// A `(key, value)` request member.
fn member(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// A template whose only resource needs no owning service: the deployments here fail before
/// any resource is made, and the resource is there so the template declares one at all.
fn with_resource(mut sections: serde_json::Map<String, Value>) -> String {
    sections.insert(
        "Resources".to_string(),
        json!({ "Handle": { "Type": "AWS::CloudFormation::WaitConditionHandle" } }),
    );
    Value::Object(sections).to_string()
}

/// The exporting stack's template: one output published under [`CLAIMED_EXPORT`].
fn exporter_template() -> String {
    let mut sections = serde_json::Map::new();
    sections.insert(
        "Outputs".to_string(),
        json!({ "Published": { "Value": "a published value", "Export": { "Name": CLAIMED_EXPORT } } }),
    );
    with_resource(sections)
}

/// One row: the stack to deploy, its template and request members, the failure the create
/// reports, and the reason the ROLLBACK's own event carries.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    /// Members the `CreateStack` carries beside the name and the body.
    extra: Vec<(String, String)>,
    /// The `StackStatusReason` the rolled-back stack reports: the failure alone.
    failure: String,
    /// The `ResourceStatusReason` of the rollback event: the failure and the clause, with
    /// exactly one sentence end between them.
    event_reason: String,
    /// Whether every word of this row's reason is the SERVICE's own, so no period in it came
    /// from a submitted template and none of them may double.
    service_worded: bool,
}

/// Every row of the table.
fn rows() -> Vec<Row> {
    let import_failure = format!("No export named {MISSING_EXPORT} found.");
    let collision_failure = format!(
        "Export with name {CLAIMED_EXPORT} is already exported by stack {EXPORTER_STACK}."
    );
    let parameter_failure = format!(
        "Parameter validation failed: parameter value {ABSENT_VPC} for parameter name \
         {VPC_PARAMETER} does not exist."
    );
    let assertion_failure =
        format!("Parameter validation failed: assertion error: {ASSERT_DESCRIPTION}.");
    let quoted_assertion_failure = format!(
        "Parameter validation failed: assertion error: {ASSERT_DESCRIPTION_ENDING_IN_A_PERIOD}."
    );
    let transform_failure =
        "Transform AWS::LanguageExtensions failed with: Error parsing the template.".to_string();
    vec![
        Row {
            case: "an import of an export nothing publishes",
            stack_name: "rollback-wording-missing-import",
            template: {
                let mut sections = serde_json::Map::new();
                sections.insert(
                    "Outputs".to_string(),
                    json!({ "Imported": { "Value": { "Fn::ImportValue": MISSING_EXPORT } } }),
                );
                with_resource(sections)
            },
            extra: Vec::new(),
            event_reason: format!("{import_failure} {ROLLBACK_CLAUSE}"),
            failure: import_failure,
            service_worded: true,
        },
        Row {
            case: "an export name a live stack already publishes",
            stack_name: "rollback-wording-rival-exporter",
            template: exporter_template(),
            extra: Vec::new(),
            event_reason: format!("{collision_failure} {ROLLBACK_CLAUSE}"),
            failure: collision_failure,
            service_worded: true,
        },
        Row {
            case: "a VPC-typed parameter whose value names no VPC of the account",
            stack_name: "rollback-wording-absent-vpc",
            template: {
                let mut sections = serde_json::Map::new();
                sections.insert(
                    "Parameters".to_string(),
                    json!({ VPC_PARAMETER: { "Type": "AWS::EC2::VPC::Id" } }),
                );
                with_resource(sections)
            },
            extra: vec![
                member("Parameters.member.1.ParameterKey", VPC_PARAMETER),
                member("Parameters.member.1.ParameterValue", ABSENT_VPC),
            ],
            event_reason: format!("{parameter_failure} {ROLLBACK_CLAUSE}"),
            failure: parameter_failure,
            service_worded: true,
        },
        Row {
            case: "a Rules assertion whose description stops mid-sentence",
            stack_name: "rollback-wording-rules-assertion",
            template: {
                let mut sections = serde_json::Map::new();
                sections.insert(
                    "Parameters".to_string(),
                    json!({ "Tier": { "Type": "String", "Default": "bronze" } }),
                );
                sections.insert(
                    "Rules".to_string(),
                    json!({
                        "TierIsGold": {
                            "Assertions": [{
                                "Assert": { "Fn::Equals": [{ "Ref": "Tier" }, "gold"] },
                                "AssertDescription": ASSERT_DESCRIPTION,
                            }]
                        }
                    }),
                );
                with_resource(sections)
            },
            extra: Vec::new(),
            // The description stops mid-sentence, so the only period in the reason is the one
            // ending the service's own sentence, and the clause follows it.
            event_reason: format!("{assertion_failure} {ROLLBACK_CLAUSE}"),
            failure: assertion_failure,
            service_worded: true,
        },
        Row {
            case: "a Rules assertion whose description ends in its own period",
            stack_name: "rollback-wording-rules-quoted-period",
            template: {
                let mut sections = serde_json::Map::new();
                sections.insert(
                    "Parameters".to_string(),
                    json!({ "Tier": { "Type": "String", "Default": "bronze" } }),
                );
                sections.insert(
                    "Rules".to_string(),
                    json!({
                        "TierIsGold": {
                            "Assertions": [{
                                "Assert": { "Fn::Equals": [{ "Ref": "Tier" }, "gold"] },
                                "AssertDescription": ASSERT_DESCRIPTION_ENDING_IN_A_PERIOD,
                            }]
                        }
                    }),
                );
                with_resource(sections)
            },
            extra: Vec::new(),
            // The description is the SUBMITTER's and is reproduced as written, so its period
            // and the one ending the service's sentence both stand — and the clause still
            // adds none of its own. This is the one row where two periods meet, and it is why
            // the rule is "add no sentence end to a sentence that has one" rather than "never
            // let two periods meet".
            event_reason: format!("{quoted_assertion_failure} {ROLLBACK_CLAUSE}"),
            failure: quoted_assertion_failure,
            service_worded: false,
        },
        Row {
            case: "an Fn::ForEach in a section the declared transform does not define it for",
            stack_name: "rollback-wording-foreach-placement",
            template: {
                let mut sections = serde_json::Map::new();
                sections.insert(
                    "Transform".to_string(),
                    json!("AWS::LanguageExtensions"),
                );
                sections.insert(
                    "Metadata".to_string(),
                    json!({
                        "Fn::ForEach::Tiers": [
                            "Tier",
                            ["bronze", "gold"],
                            { "${Tier}Note": "one note per tier" }
                        ]
                    }),
                );
                with_resource(sections)
            },
            extra: vec![member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND")],
            event_reason: format!("{transform_failure} {ROLLBACK_CLAUSE}"),
            failure: transform_failure,
            service_worded: true,
        },
    ]
}

/// RULE: for every row, the rolled-back stack reports the failure alone and its rollback
/// event reports that failure followed by ONE sentence end and the clause.
#[tokio::test]
async fn a_rolled_back_create_words_the_failure_and_the_clause_as_one_sentence_each() {
    let router = build_router();

    // The live stack whose export name the collision row then tries to publish: it has to be
    // COMPLETE for its export to be the one in the way.
    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[
            member("StackName", EXPORTER_STACK),
            member("TemplateBody", exporter_template()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the exporting stack must be accepted, got {status} with body {created}"
    );
    let settled = settled_status(&router, "the exporting stack", EXPORTER_STACK).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "the exporting stack must reach CREATE_COMPLETE before the rival is deployed, \
         got {settled:?}"
    );

    for row in rows() {
        let case = row.case;
        let mut members = vec![
            member("StackName", row.stack_name),
            member("TemplateBody", row.template.clone()),
        ];
        members.extend(row.extra.iter().cloned());
        let (status, created) = cloudformation(&router, "CreateStack", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must accept the request and fail the DEPLOYMENT, \
             got {status} with body {created}"
        );

        let reported = settled_status(&router, case, row.stack_name).await;
        assert_eq!(
            reported, "ROLLBACK_COMPLETE",
            "case {case:?}: the create must fail and roll back, got {reported:?}"
        );

        // The stack's own reason is the failure, with no clause on it: that is the field a
        // client parses.
        let reason = stack_status_reason(&router, case, row.stack_name).await;
        assert_eq!(
            reason, row.failure,
            "case {case:?}: expected the stack's reason to be {:?}, got {reason:?}",
            row.failure
        );

        // The rollback's own event carries the failure AND the clause, as one sentence
        // followed by another.
        let reasons = event_reasons(&router, case, row.stack_name).await;
        assert!(
            reasons.contains(&row.event_reason),
            "case {case:?}: expected an event carrying {:?}, got {reasons:?}",
            row.event_reason
        );
        // And for a reason that is the service's own words throughout, no sentence on that
        // stream is ended twice over: a doubled period can only come from text a template
        // supplied, which the row above it covers.
        if row.service_worded {
            for recorded in &reasons {
                assert!(
                    !recorded.contains(".."),
                    "case {case:?}: a reason worded by the service alone may not double a \
                     sentence end, got {recorded:?}"
                );
            }
        }
    }
}

/// The status `stack_name` settles at: polled until it is no longer in flight, or the
/// deadline passes.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[member("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must report {stack_name}, got {status} with \
             body {body}"
        );
        described = body;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {stack_name} never settled: {described}");
}

/// The `StackStatusReason` `stack_name` reports.
async fn stack_status_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStacks", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStacks must report {stack_name}, got {status} with body {body}"
    );
    opt_element(&body, "StackStatusReason").unwrap_or_default()
}

/// Every `ResourceStatusReason` `stack_name` has recorded.
async fn event_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
) -> Vec<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackEvents must report {stack_name}'s events, got {status} \
         with body {body}"
    );
    elements(&body, "ResourceStatusReason")
}
