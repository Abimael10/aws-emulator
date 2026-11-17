// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack does with an `AWS::Lambda::Function` whose property value is a
//! `{{resolve:secretsmanager:<secret id>:SecretString:<json key>}}` DYNAMIC REFERENCE.
//!
//! The rule under test: **a `secretsmanager` dynamic reference naming a `json-key` the
//! referenced secret's `SecretString` associates no value with cannot be resolved, so the
//! resource whose property carries it — whatever its type, and wherever in its properties the
//! reference stands — reaches `CREATE_FAILED` with ONE FIXED reason
//! ([`NO_VALUE_FOR_THE_JSON_KEY`]), the stack rolls back to `ROLLBACK_COMPLETE`, and nothing is
//! left behind in the service that would have owned the resource; a reference naming a value
//! the secret DOES associate resolves to that value, which is what the owning service is asked
//! for the resource with.** The reason carries nothing of the request — not the secret's name
//! or ARN, not the key, not the logical id, not the property — so a client classifying a deploy
//! failure by its reason compares against a constant rather than an environment-dependent
//! sentence.
//!
//! Four services have to agree for that to hold, so this is the suite that can pin it:
//! CloudFormation resolves the reference and makes the calls, Secrets Manager holds the secret,
//! AWS Lambda is the service that owns the resource carrying the reference (and the one that
//! must report nothing for a resource that failed), and IAM holds the execution role the
//! function is declared with. `cloudformation_secretsmanager_dynamic_references.rs` states the
//! same rule for carriers whose property is a scalar the owning service reports straight back;
//! this suite is the half whose carrier is a FUNCTION — the reference stands two members deep
//! in `Environment/Variables`, the resource is withheld behind its role, and what a resolved
//! value has to reach is the environment the function RUNS with, not only the record the stack
//! keeps.
//!
//! The rows differ along what the rule generalises over: WHY the key is associated with no
//! value (a JSON document holding other keys, a key differing from a held one only in case, a
//! `SecretString` that is not a JSON document at all), HOW the reference names the secret (by
//! the ARN Secrets Manager minted, by the secret's name, and with a version stage stated beside
//! the key), and WHERE in the function's properties the reference stands (nested in
//! `Environment/Variables`, and the top-level `Description`).
//!
//! The boundary of the rule is the second test: the same reference form naming keys the secret
//! does associate, and one naming no key at all, deploy to `CREATE_COMPLETE` and the function
//! both REPORTS and SERVES the values the secret holds. Every expected value is computed from
//! what this suite wrote into Secrets Manager, never read back out of the answer being checked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{
    resource_status, status, STACK_RESOURCE_TYPE,
};

// Polling a stack, the awsQuery form encoder and the XML readers are written ONCE in this
// crate, and so are the Lambda wire call, the two readers over a reported configuration, the
// execution-role trust policy and the two reasons an unresolvable reference fails a resource
// with: a suite that writes its own copy of them drifts from the others — and the fixed reason
// is the very text this rule is about, so it is compared against one declaration of it.
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::cloudformation_lambda_ssm_sourced_update::{
    lambda, reported_configuration, reported_environment, trust_policy,
};
use crate::cloudformation_secretsmanager_dynamic_references::{
    NO_VALUE_FOR_THE_JSON_KEY, UNRESOLVED,
};
use crate::query_wire::{elements, form_encode, members_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for — the Region [`stack_status`] reads in — and so
/// the Region the stacks, their functions and the secrets belong to.
const REGION: &str = "us-east-1";

/// How often the suite asks a stack whether its create has settled, and how long it goes on
/// asking. The wait is on the CONDITION — a status that is no longer `…_IN_PROGRESS` — so the
/// interval is short and the deadline is only ever reached by a create that never settles at
/// all: it is deliberately far longer than the spans a deployment takes, so that a loaded
/// machine does not decide the outcome, and all it is for is that the wait TERMINATES.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// See [`POLL_INTERVAL`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// The logical ids every template here declares its two resources under.
const FUNCTION_LOGICAL_ID: &str = "Fn";
const ROLE_LOGICAL_ID: &str = "ExecRole";

/// The `secret-string` field of a reference, which is the only value that field may carry.
const SECRET_STRING: &str = "SecretString";

/// The runtime every template declares, and the handler the inline package's archived file
/// resolves to (`index` plus the runtime's extension).
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// Lambda's control-plane collection URI, which `ListFunctions` reads.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The acknowledgement a template declaring a NAMED IAM role needs.
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The `State` a function takes invocations in.
const STATE_ACTIVE: &str = "Active";

/// The handler every template declares: it needs nothing of its runtime beyond its own
/// environment, so what an invocation answers is the values the deployment resolved into the
/// function.
const HANDLER_SOURCE: &str = r#"
import os


def handler(event, context):
    answer = {}
    for name in event["read"]:
        answer[name] = os.environ.get(name, "")
    return answer
"#;

/// The four services this rule spans, over ONE engine exactly as `core/router/src/lib.rs` mounts
/// them: CloudFormation resolves the references and makes the calls, Secrets Manager holds the
/// secrets, Lambda owns the resource carrying the reference, IAM holds the execution role.
///
/// Request authentication is off, as in every other suite here; the in-process callback
/// CloudFormation makes signs its request either way (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
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
    let code = response.status().as_u16();
    (code, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Secrets Manager operation over its awsJson1_1 wire.
async fn secretsmanager(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let code = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (code, parsed)
}

/// One source secret a row's reference resolves against: created through Secrets Manager's OWN
/// wire, so a reference resolves against the very secret `CreateSecret` minted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Source {
    name: &'static str,
    secret_string: &'static str,
}

/// A JSON document holding two keys — the recorded shape of a credential secret.
const DOCUMENT: Source = Source {
    name: "cfn-lambda-dynref-document",
    secret_string: r#"{"username":"app","api_key":"ak-example-value"}"#,
};

/// A `SecretString` that is not a JSON document at all, so no key of it can be addressed.
const PLAIN_TEXT: Source = Source {
    name: "cfn-lambda-dynref-plain-text",
    secret_string: "a-plain-text-example-value",
};

/// Every source, created once per test: each row addresses one of them, and several rows
/// address the same one.
const SOURCES: &[Source] = &[DOCUMENT, PLAIN_TEXT];

/// How a row's reference addresses the source secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressedBy {
    /// The ARN Secrets Manager minted for it, which carries colons of its own.
    Arn,
    /// The secret's NAME.
    Name,
}

/// WHERE in the function's properties a row stands the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// `Environment/Variables/<name>`: the recorded carrier, two members deep in the document
    /// the owning service is asked for the function with.
    EnvironmentVariable,
    /// `Description`: a scalar property at the top of that same document.
    Description,
}

/// One row of the failing table: a reference naming a key no value is associated with, and the
/// deployment it fails.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    source: Source,
    addressed_by: AddressedBy,
    /// The `json-key` the reference names, which the source associates no value with.
    json_key: &'static str,
    /// The version STAGE the reference states beside the key, or `""` for a reference that
    /// states none.
    version_stage: &'static str,
    placement: Placement,
}

const ROWS: &[Row] = &[
    Row {
        // The recorded case: a document holding other keys, addressed by its ARN, standing in
        // an environment variable.
        case: "a document that holds other keys, addressed by ARN, in an environment variable",
        source: DOCUMENT,
        addressed_by: AddressedBy::Arn,
        json_key: "no_such_key",
        version_stage: "",
        placement: Placement::EnvironmentVariable,
    },
    Row {
        // A key is matched exactly: one differing from a held key only in case is absent.
        case: "a key differing from a held one only in case, addressed by name",
        source: DOCUMENT,
        addressed_by: AddressedBy::Name,
        json_key: "API_KEY",
        version_stage: "",
        placement: Placement::EnvironmentVariable,
    },
    Row {
        // The secret holds text, but no document for a key to be looked up in — and the
        // reference stands in a top-level property rather than a nested one.
        case: "a SecretString that is not a JSON document, in the top-level Description",
        source: PLAIN_TEXT,
        addressed_by: AddressedBy::Arn,
        json_key: "api_key",
        version_stage: "",
        placement: Placement::Description,
    },
    Row {
        // The reference selects a version as well, which changes nothing about the reason.
        case: "a missing key on a reference that states a version stage too",
        source: DOCUMENT,
        addressed_by: AddressedBy::Name,
        json_key: "no_such_key",
        version_stage: "AWSCURRENT",
        placement: Placement::EnvironmentVariable,
    },
];

/// The environment variable the rows and the control stand their references in.
const API_KEY_VARIABLE: &str = "API_KEY";

/// The role one stack declares, named after the stack so no two stacks collide in IAM.
fn role_name(stack: &str) -> String {
    format!("{stack}-role")
}

/// The function one stack declares, named after the stack: the name is the request's, stated
/// so the leak check below can filter the listing by `<stack name>-` and so no two stacks
/// collide in Lambda.
fn function_name(stack: &str) -> String {
    format!("{stack}-{}", FUNCTION_LOGICAL_ID.to_lowercase())
}

/// The reference one row declares against the secret id it addresses: the `json-key` form, the
/// same form with a version stage stated beside the key, and — for a key stated as `""` — the
/// form naming no key at all, which resolves to the whole `SecretString`.
fn reference(secret_id: &str, json_key: &str, version_stage: &str) -> String {
    let fields = match (json_key.is_empty(), version_stage.is_empty()) {
        (true, true) => secret_id.to_string(),
        (false, true) => format!("{secret_id}:{SECRET_STRING}:{json_key}"),
        (false, false) => format!("{secret_id}:{SECRET_STRING}:{json_key}:{version_stage}"),
        // A version stage is stated BESIDE a key, so no row names one without the other: the
        // reference form would carry an empty `json-key` field, which names no value at all.
        (true, false) => unreachable!("a version stage is stated beside a json-key"),
    };
    format!("{{{{resolve:secretsmanager:{fields}}}}}")
}

/// The template one deployment submits: the execution role, and the function whose properties
/// carry `variables` as its environment and `description` as its `Description`.
fn template(stack: &str, variables: &BTreeMap<String, String>, description: &str) -> String {
    let mut properties = json!({
        "FunctionName": function_name(stack),
        "Runtime": RUNTIME,
        "Handler": HANDLER,
        // The function READS an attribute of the role, so it is deployed only once that role
        // exists — the moment a reference of this form is resolved at.
        "Role": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
        "Code": { "ZipFile": HANDLER_SOURCE },
    });
    if !variables.is_empty() {
        let mut declared = Map::new();
        for (name, value) in variables {
            declared.insert(name.clone(), json!(value));
        }
        properties["Environment"] = json!({ "Variables": Value::Object(declared) });
    }
    if !description.is_empty() {
        properties["Description"] = json!(description);
    }
    json!({
        "Resources": {
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role_name(stack),
                    "AssumeRolePolicyDocument": trust_policy(),
                }
            },
            FUNCTION_LOGICAL_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": properties,
            }
        }
    })
    .to_string()
}

/// Deploy one template, and assert the create was accepted.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) {
    let (code, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", body),
            ("Capabilities.member.1", NAMED_IAM),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {code} with body {answered}"
    );
}

/// Poll `stack` until its create has SETTLED — a `StackStatus` that is no longer
/// `…_IN_PROGRESS` — and answer that status with the `DescribeStacks` answer it was read from.
///
/// The condition is settlement rather than one expected status, so a create that settles the
/// wrong way is reported as soon as it settles instead of being waited out. A stack that never
/// settles at all is a failure of its own kind rather than a status mismatch, so reaching the
/// deadline says so.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut last = stack_status(router, stack).await;
    while last.0.ends_with("_IN_PROGRESS") {
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the create of {stack} to settle within \
             {SETTLE_DEADLINE:?}, and it still reports {:?}: {}",
            last.0,
            last.1
        );
        tokio::time::sleep(POLL_INTERVAL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Create every source secret through Secrets Manager's OWN wire, and answer the ARN that
/// service minted for each, by the name it was created under.
async fn create_sources(router: &MultiServiceRouter) -> BTreeMap<&'static str, String> {
    let mut minted = BTreeMap::new();
    for source in SOURCES {
        let (code, answered) = secretsmanager(
            router,
            "CreateSecret",
            &json!({
                "Name": source.name,
                // An SDK generates this idempotency token itself; a raw request carries its own.
                "ClientRequestToken": format!("{:0<32}", source.name),
                "SecretString": source.secret_string,
            }),
        )
        .await;
        assert_eq!(
            code, 200,
            "CreateSecret({}) must answer 200, got {code} with body {answered}",
            source.name
        );
        let arn = answered
            .get("ARN")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("CreateSecret({}) published no ARN: {answered}", source.name))
            .to_string();
        minted.insert(source.name, arn);
    }
    minted
}

/// The text of one element of an event member, or `None` where the member omits it.
fn first(member: &str, tag: &str) -> Option<String> {
    elements(member, tag).into_iter().next()
}

/// Every `(LogicalResourceId, ResourceStatus, ResourceStatusReason)` the stack recorded for a
/// resource of its own template that FAILED — the stack's own events (whose resource type is
/// the stack type) are the operation's summary, not a resource that failed.
async fn resource_failures(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String)> {
    let (code, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {code} with body {body}"
    );
    members_of(&body)
        .into_iter()
        .filter(|member| first(member, "ResourceType").as_deref() != Some(STACK_RESOURCE_TYPE))
        .filter_map(|member| {
            let reported = first(member, "ResourceStatus")?;
            if !reported.ends_with("_FAILED") {
                return None;
            }
            Some((
                first(member, "LogicalResourceId").unwrap_or_default(),
                reported,
                first(member, "ResourceStatusReason").unwrap_or_default(),
            ))
        })
        .collect()
}

/// Every function name Lambda lists right now.
async fn listed_function_names(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (code, _, body) = lambda(router, "GET", FUNCTIONS_URI, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListFunctions must answer 200, got {code} with body {body}"
    );
    body.get("Functions")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(|function| {
                    function
                        .get("FunctionName")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The value the source's document associates with `key`, read out of what this suite WROTE
/// into the store — so an expectation is never read back out of the answer being checked.
fn held(source: Source, key: &str) -> String {
    if key.is_empty() {
        // A reference naming no key resolves to the whole secret text.
        return source.secret_string.to_string();
    }
    let document: Value = serde_json::from_str(source.secret_string)
        .unwrap_or_else(|_| panic!("the source {} holds a JSON document", source.name));
    document
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the source {} associates {key:?} with a value", source.name))
        .to_string()
}

/// Every row: the function carrying a reference to a key no value is associated with is the ONE
/// resource that fails, with the fixed reason; the stack settles in `ROLLBACK_COMPLETE`; and
/// Lambda holds no function of that stack afterwards.
#[tokio::test]
async fn a_reference_to_a_json_key_the_secret_does_not_associate_rolls_the_stack_back() {
    let router = build_router();
    let minted = create_sources(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-lambda-dynref-missing-{index}");
        let arn = minted[row.source.name].clone();
        let secret_id = match row.addressed_by {
            AddressedBy::Arn => arn.clone(),
            AddressedBy::Name => row.source.name.to_string(),
        };
        let reference = reference(&secret_id, row.json_key, row.version_stage);
        let (variables, description) = match row.placement {
            Placement::EnvironmentVariable => (
                [(API_KEY_VARIABLE.to_string(), reference.clone())]
                    .into_iter()
                    .collect(),
                String::new(),
            ),
            Placement::Description => (BTreeMap::new(), reference.clone()),
        };
        let body = template(&stack, &variables, &description);
        create_stack(&router, case, &stack, &body).await;

        let (reported, described) = settled_stack(&router, case, &stack).await;
        let failures = resource_failures(&router, case, &stack).await;
        assert_eq!(
            reported,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the unresolvable reference to roll the stack back, got \
             {described} with resource failures {failures:?}"
        );
        assert_eq!(
            failures,
            vec![(
                FUNCTION_LOGICAL_ID.to_string(),
                resource_status::CREATE_FAILED.to_string(),
                NO_VALUE_FOR_THE_JSON_KEY.to_string(),
            )],
            "case {case:?}: expected the function carrying the reference to be the one failure, \
             stating {NO_VALUE_FOR_THE_JSON_KEY:?}, got {failures:?}"
        );
        // That reason is a CONSTANT: none of the per-run values the reference was written with
        // is interpolated into it, so it compares equal in any account and Region — and it is
        // not the resolver's own diagnostic for some other unresolvable reference either.
        //
        // Read over the values long enough to say anything: a value of a character or two is a
        // substring of most English sentences, so one of those would report how the reason is
        // WORDED rather than whether the request is interpolated into it (the equality above is
        // what holds the wording).
        let reason = failures[0].2.clone();
        for interpolated in [
            arn.as_str(),
            row.source.name,
            row.json_key,
            stack.as_str(),
            FUNCTION_LOGICAL_ID,
            API_KEY_VARIABLE,
            UNRESOLVED,
        ]
        .into_iter()
        .filter(|value| value.len() > 3)
        {
            assert!(
                !reason.contains(interpolated),
                "case {case:?}: the reason must carry nothing of the request, but {reason:?} \
                 carries {interpolated:?}"
            );
        }

        // ...and nothing was left behind in the service that would have owned the function: no
        // function of this stack is listed, and the name the declaration asked for resolves to
        // nothing at all.
        let listed = listed_function_names(&router, case).await;
        let leaked: Vec<&String> = listed
            .iter()
            .filter(|name| name.starts_with(&format!("{stack}-")))
            .collect();
        assert!(
            leaked.is_empty(),
            "case {case:?}: expected the rollback to leave no function of {stack:?} behind, got \
             {leaked:?} among {listed:?}"
        );
        let declared = function_name(&stack);
        let (code, refused) = reported_configuration(&router, &declared).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected Lambda to resolve no function {declared:?}, got {code} with \
             body {refused}"
        );
        let refusal = refused
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            refusal.ends_with("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException for {declared:?}, got {refused}"
        );
    }
}

/// One variable of the boundary case: the same reference form, naming a value the secret DOES
/// associate.
#[derive(Debug, Clone, Copy)]
struct Honoured {
    case: &'static str,
    variable: &'static str,
    source: Source,
    addressed_by: AddressedBy,
    /// The `json-key` the reference names, or `""` for a reference naming NO key, which resolves
    /// to the whole `SecretString`.
    json_key: &'static str,
}

const HONOURED: &[Honoured] = &[
    Honoured {
        case: "a key of the document, addressed by the ARN the store minted",
        variable: "API_KEY",
        source: DOCUMENT,
        addressed_by: AddressedBy::Arn,
        json_key: "api_key",
    },
    Honoured {
        case: "another key of the same document, addressed by the secret's name",
        variable: "USER_NAME",
        source: DOCUMENT,
        addressed_by: AddressedBy::Name,
        json_key: "username",
    },
    Honoured {
        case: "a reference naming no key at all, which resolves to the whole SecretString",
        variable: "WHOLE_SECRET",
        source: PLAIN_TEXT,
        addressed_by: AddressedBy::Name,
        json_key: "",
    },
];

/// The stack the boundary case deploys.
const HONOURED_STACK: &str = "cfn-lambda-dynref-held";

/// The boundary of the rule: the same references naming values the secret DOES associate deploy,
/// and the function both reports and SERVES those values.
#[tokio::test]
async fn a_reference_to_a_value_the_secret_does_associate_reaches_the_function() {
    let router = build_router();
    let minted = create_sources(&router).await;

    let mut variables = BTreeMap::new();
    let mut expected = BTreeMap::new();
    for row in HONOURED {
        let secret_id = match row.addressed_by {
            AddressedBy::Arn => minted[row.source.name].clone(),
            AddressedBy::Name => row.source.name.to_string(),
        };
        variables.insert(
            row.variable.to_string(),
            reference(&secret_id, row.json_key, ""),
        );
        expected.insert(row.variable.to_string(), held(row.source, row.json_key));
    }
    let body = template(HONOURED_STACK, &variables, "");
    let case = "the honoured references";
    create_stack(&router, case, HONOURED_STACK, &body).await;

    let (reported, described) = settled_stack(&router, case, HONOURED_STACK).await;
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "case {case:?}: expected a reference the store can honour to deploy, got {described} \
         with resource failures {:?}",
        resource_failures(&router, case, HONOURED_STACK).await
    );

    let declared = function_name(HONOURED_STACK);
    let (code, configuration) = reported_configuration(&router, &declared).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected Lambda to resolve the function the completed deployment \
         published, got {code} with body {configuration}"
    );
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some(STATE_ACTIVE),
        "case {case:?}: expected Lambda to report the function {STATE_ACTIVE}, got {configuration}"
    );
    let reported_variables = reported_environment(&configuration);
    for row in HONOURED {
        let wanted = expected[row.variable].clone();
        let carried = reported_variables.get(row.variable).cloned();
        assert_eq!(
            carried.as_deref(),
            Some(wanted.as_str()),
            "case {:?}: expected {} to carry the value the secret associates ({wanted:?}), got \
             {carried:?}",
            row.case,
            row.variable
        );
    }
    assert_eq!(
        reported_variables, expected,
        "case {case:?}: expected the function to carry exactly the values the secrets associate \
         ({expected:?}), got {reported_variables:?}"
    );

    // ...and what the function SERVES is those values: the resolution reached the environment
    // the function RUNS with, not only the record the stack keeps of it.
    let read: Vec<&str> = HONOURED.iter().map(|row| row.variable).collect();
    let (code, headers, payload) = lambda(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{declared}/invocations"),
        json!({ "read": read }).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {code} with payload {payload}"
    );
    let failure = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        failure, None,
        "case {case:?}: expected no function error, got {failure:?} with payload {payload}"
    );
    let wanted = Value::Object(
        expected
            .iter()
            .map(|(name, value)| (name.clone(), json!(value)))
            .collect(),
    );
    assert_eq!(
        payload, wanted,
        "case {case:?}: expected the handler to answer {wanted}, got {payload}"
    );
}
