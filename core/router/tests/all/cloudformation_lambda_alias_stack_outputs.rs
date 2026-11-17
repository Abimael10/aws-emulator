// SPDX-License-Identifier: CC0-1.0

//! What an infrastructure-as-code client reads back after deploying a Lambda function, a
//! published version of it and the alias that fronts the version: the stack's OUTPUTS, and
//! the alias itself in AWS Lambda.
//!
//! The rule under test: **a stack reports one `Outputs` entry per output its template
//! declares and the stack HAS, and every such entry carries the `OutputValue` that output's
//! `Value` expression resolved to where the stack was deployed — whichever intrinsic the
//! expression is written with — and a resource the stack reports created exists in the
//! service that owns it, so AWS Lambda answers `GetAlias` for the alias the template
//! declared, under the ARN the stack published as that resource's physical id, and refuses
//! only a name no alias of the function carries.** Two crates have to agree for that to hold
//! (CloudFormation resolves the outputs and makes the calls, AWS Lambda creates the alias and
//! answers for it, IAM answers for the execution role), so the suite lives here rather than
//! in any one crate.
//!
//! A client that reads `Outputs[i]["OutputValue"]` is the reason the first half matters: an
//! entry reported with its key and no value is a missing member to every SDK, and the
//! handles a versioned-Lambda stack publishes (the function, the version, the alias) are
//! exactly what the calls after the deploy are addressed by.
//!
//! The rows differ along what the rule generalises over: how the alias's `Name` is spelled
//! (a literal, a `Ref` to a parameter, an `Fn::Sub` over one), how the version's and the
//! alias's declarations NAME the function (`Ref` to it, or its ARN through `Fn::GetAtt`),
//! which alias name the stack publishes — so the template's `Condition` holds for some rows
//! and not others — and which `Mappings` entry the stack's parameters select. Within each
//! row the OUTPUTS span the value forms a template writes: `Ref` to a resource, to a
//! parameter and to a pseudo parameter, `Fn::GetAtt`, both `Fn::Sub` forms, `Fn::Join`,
//! `Fn::Select` over `Fn::Split`, `Fn::FindInMap`, `Fn::If`, `Fn::Base64`, an output
//! carrying a `Description`, one carrying an `Export`, and one gated by a `Condition`.
//!
//! Every expected value is computed here from what the row declares, from the template's own
//! mapping table and from the account this emulator mints its ARNs under — never read back
//! out of the answer being checked.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// Polling a stack until it first reports a status is what every suite deploying a resource
// its owning service reports as still coming up needs, and this crate already has it.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`), and the SigV4 header builder it writes once as well.
use crate::query_wire::{block_of, elements, form_encode, pairs_of, physical_ids_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The alias name the template's `Condition` tests for, so a row publishing this name has
/// the conditional output and a row publishing any other name does not.
const LIVE: &str = "live";

/// The version number the first `AWS::Lambda::Version` of a function publishes, which is
/// what the alias of every row invokes.
const FIRST_VERSION: &str = "1";

/// The `Description` the template gives one output, read back to pin that the member travels
/// beside the value rather than instead of it.
const FUNCTION_NAME_DESCRIPTION: &str = "the function this stack created";

/// The template's own `Mappings` table: the single source of both the document the stack is
/// deployed with and the value each row's `Fn::FindInMap` is expected to resolve to.
const CONCURRENCY_BY_STAGE: &[(&str, &str)] = &[("prod", "2"), ("beta", "1")];

/// The account id every ARN this emulator mints carries, computed rather than written out.
fn account() -> &'static str {
    ::emulator_server::emulator_account_id()
}

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. IAM is here because every template declares the
/// execution role its function runs as, which Lambda resolves before it writes the function.
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
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Lambda operation over its restJson1 wire, answered with the HTTP status, the
/// wire error code (`""` on a success) and the JSON body.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str) -> (u16, String, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            headers
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_default();
    (status, code, parsed)
}

/// The ARN a function of `name` carries in this suite's Region: the same total function of
/// the Region, the account and the name both services compute it from.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{}:function:{name}", account())
}

/// The execution role every template declares, with the trust policy Lambda has to be able
/// to assume.
fn execution_role() -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "lambda.amazonaws.com" },
                    "Action": "sts:AssumeRole"
                }]
            }
        }
    })
}

/// The `Mappings` section every template carries, built from [`CONCURRENCY_BY_STAGE`].
fn mappings() -> Value {
    let mut by_stage = Map::new();
    for (stage, concurrency) in CONCURRENCY_BY_STAGE {
        by_stage.insert((*stage).to_string(), json!({ "Concurrency": concurrency }));
    }
    json!({ "ByStage": Value::Object(by_stage) })
}

/// The value one stage's `Fn::FindInMap` resolves to, read from the same table the template
/// declares.
fn concurrency_of(stage: &str) -> &'static str {
    CONCURRENCY_BY_STAGE
        .iter()
        .find(|(name, _)| *name == stage)
        .map(|(_, concurrency)| *concurrency)
        .expect("every row's stage is a key of the declared mapping")
}

/// How a row's template spells the alias's `Name` property.
enum Spelling {
    /// The name written out.
    Literal,
    /// A `Ref` to the stack parameter that carries it.
    ParameterRef,
    /// An `Fn::Sub` over that parameter.
    Sub,
}

/// One row: how the alias is named, how the function is named, which alias name the stack
/// publishes and which stage its mapping lookup selects.
struct Row {
    case: &'static str,
    function: &'static str,
    alias: &'static str,
    stage: &'static str,
    spelling: Spelling,
    /// `true` for declarations that name the function through `Fn::GetAtt … Arn`, `false`
    /// for ones that name it with `Ref` (which "returns the function name").
    by_arn: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the alias named by a literal, the function by Ref, the condition holding",
        function: "orders-api",
        alias: LIVE,
        stage: "prod",
        spelling: Spelling::Literal,
        by_arn: false,
    },
    Row {
        case: "the alias named through a parameter, the function by its ARN, the condition \
               not holding",
        function: "billing-api",
        alias: "blue",
        stage: "beta",
        spelling: Spelling::ParameterRef,
        by_arn: true,
    },
    Row {
        case: "the alias named by Fn::Sub over a parameter, the function by Ref",
        function: "shipping-api",
        alias: "green-2",
        stage: "prod",
        spelling: Spelling::Sub,
        by_arn: false,
    },
];

/// The template a row deploys: a function, one published version of it, one alias of that
/// version, and the outputs that publish each handle through a different value form.
fn template(row: &Row) -> String {
    let function_reference = if row.by_arn {
        json!({ "Fn::GetAtt": ["Fn", "Arn"] })
    } else {
        json!({ "Ref": "Fn" })
    };
    let alias_name = match row.spelling {
        Spelling::Literal => json!(row.alias),
        Spelling::ParameterRef => json!({ "Ref": "AliasName" }),
        Spelling::Sub => json!({ "Fn::Sub": "${AliasName}" }),
    };
    json!({
        "Parameters": {
            "AliasName": { "Type": "String", "Default": LIVE },
            "Stage": { "Type": "String", "Default": "prod" }
        },
        "Mappings": mappings(),
        "Conditions": {
            "IsLive": { "Fn::Equals": [{ "Ref": "AliasName" }, LIVE] }
        },
        "Resources": {
            "ExecRole": execution_role(),
            "Fn": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["ExecRole", "Arn"] },
                    "Code": { "ZipFile": "def handler(event, context):\n    return 1\n" }
                }
            },
            "FnVersion": {
                "Type": "AWS::Lambda::Version",
                "Properties": { "FunctionName": function_reference.clone() }
            },
            "FnAlias": {
                "Type": "AWS::Lambda::Alias",
                "Properties": {
                    "FunctionName": function_reference,
                    "Name": alias_name,
                    "FunctionVersion": { "Fn::GetAtt": ["FnVersion", "Version"] }
                }
            }
        },
        "Outputs": {
            "FunctionName": {
                "Value": { "Ref": "Fn" },
                "Description": FUNCTION_NAME_DESCRIPTION
            },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Fn", "Arn"] } },
            "VersionNumber": { "Value": { "Fn::GetAtt": ["FnVersion", "Version"] } },
            "VersionArn": { "Value": { "Fn::GetAtt": ["FnVersion", "FunctionArn"] } },
            "AliasRef": { "Value": { "Ref": "FnAlias" } },
            "AliasArnAttribute": { "Value": { "Fn::GetAtt": ["FnAlias", "AliasArn"] } },
            "AliasNameFromArn": {
                "Value": {
                    "Fn::Select": [
                        "7",
                        { "Fn::Split": [":", { "Fn::GetAtt": ["FnAlias", "AliasArn"] }] }
                    ]
                }
            },
            "QualifiedHandle": {
                "Value": {
                    "Fn::Sub": [
                        "${Name}:${Qualifier}",
                        { "Name": { "Ref": "Fn" }, "Qualifier": { "Ref": "AliasName" } }
                    ]
                }
            },
            "JoinedVersionHandle": {
                "Value": {
                    "Fn::Join": [
                        ":",
                        [{ "Ref": "Fn" }, { "Fn::GetAtt": ["FnVersion", "Version"] }]
                    ]
                }
            },
            "Region": { "Value": { "Ref": "AWS::Region" } },
            "Account": { "Value": { "Ref": "AWS::AccountId" } },
            "StackHandle": { "Value": { "Fn::Sub": "${AWS::StackName}/${Stage}" } },
            "MappedConcurrency": {
                "Value": { "Fn::FindInMap": ["ByStage", { "Ref": "Stage" }, "Concurrency"] }
            },
            "TrafficTarget": {
                "Value": {
                    "Fn::If": [
                        "IsLive",
                        { "Fn::GetAtt": ["FnAlias", "AliasArn"] },
                        { "Fn::GetAtt": ["FnVersion", "FunctionArn"] }
                    ]
                }
            },
            "EncodedAliasName": { "Value": { "Fn::Base64": { "Ref": "AliasName" } } },
            "ExportedAliasArn": {
                "Value": { "Fn::GetAtt": ["FnAlias", "AliasArn"] },
                "Export": { "Name": { "Fn::Sub": "${AWS::StackName}-alias-arn" } }
            },
            "LiveOnly": {
                "Condition": "IsLive",
                "Value": { "Ref": "FnAlias" }
            }
        }
    })
    .to_string()
}

/// Every output the row's stack HAS, with the value its expression resolves to — computed
/// from the row and the declared mapping table, never read off the answer.
fn expected_outputs(row: &Row, stack: &str) -> BTreeMap<String, String> {
    let function_arn = function_arn(row.function);
    let version_arn = format!("{function_arn}:{FIRST_VERSION}");
    let alias_arn = format!("{function_arn}:{}", row.alias);
    let mut expected: BTreeMap<String, String> = [
        ("FunctionName", row.function.to_string()),
        ("FunctionArn", function_arn),
        ("VersionNumber", FIRST_VERSION.to_string()),
        ("VersionArn", version_arn.clone()),
        ("AliasRef", alias_arn.clone()),
        ("AliasArnAttribute", alias_arn.clone()),
        ("AliasNameFromArn", row.alias.to_string()),
        ("QualifiedHandle", format!("{}:{}", row.function, row.alias)),
        (
            "JoinedVersionHandle",
            format!("{}:{FIRST_VERSION}", row.function),
        ),
        ("Region", REGION.to_string()),
        ("Account", account().to_string()),
        ("StackHandle", format!("{stack}/{}", row.stage)),
        ("MappedConcurrency", concurrency_of(row.stage).to_string()),
        (
            "TrafficTarget",
            if row.alias == LIVE {
                alias_arn.clone()
            } else {
                version_arn
            },
        ),
        (
            "EncodedAliasName",
            ::aws_smithy_types::base64::encode(row.alias),
        ),
        ("ExportedAliasArn", alias_arn.clone()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect();
    // "Output values that are conditionally created": the gated output is one the stack HAS
    // only where the condition holds.
    if row.alias == LIVE {
        expected.insert("LiveOnly".to_string(), alias_arn);
    }
    expected
}

/// Every row: the stack completes, each output it declares is reported WITH the value its
/// expression resolved to, the alias the template declared exists in AWS Lambda under the ARN
/// the stack published for it, and only a name no alias of the function carries is refused.
#[tokio::test]
async fn a_versioned_alias_stack_publishes_every_output_and_the_alias_itself() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let stack_name = format!("lambda-alias-outputs-{}", row.function);
        let body = template(row);
        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Parameters.member.1.ParameterKey", "AliasName"),
                ("Parameters.member.1.ParameterValue", row.alias),
                ("Parameters.member.2.ParameterKey", "Stage"),
                ("Parameters.member.2.ParameterValue", row.stage),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body \
             {answered}"
        );

        let (status, described) = wait_for_stack_status(&router, &stack_name, CREATE_COMPLETE).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {status} with \
             the reasons {:?}",
            elements(&described, "StackStatusReason")
        );

        // The outputs the stack HAS, each with the value its expression resolved to. The
        // pairing is `query_wire::outputs_of`'s, applied to the `Outputs` BLOCK rather than
        // to the whole answer, because this suite also reads the keys on their own and the
        // other keyed members (`Description`, `ExportName`) out of the same block — so every
        // read here is scoped to the list the pairs come from.
        let outputs = block_of(&described, "Outputs").unwrap_or_default();
        let reported_keys = elements(outputs, "OutputKey");
        let reported = pairs_of(outputs, "OutputKey", "OutputValue");
        let expected = expected_outputs(row, &stack_name);
        let valueless: Vec<&String> = reported_keys
            .iter()
            .filter(|key| !reported.contains_key(*key))
            .collect();
        assert!(
            valueless.is_empty(),
            "case {case:?}: expected every Outputs entry to carry its OutputValue, got none \
             for {valueless:?} in {described}"
        );
        let mut sorted_keys = reported_keys.clone();
        sorted_keys.sort();
        let expected_keys: Vec<String> = expected.keys().cloned().collect();
        assert_eq!(
            sorted_keys, expected_keys,
            "case {case:?}: expected the stack to report these outputs, got {sorted_keys:?}"
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected each output to carry the value its expression resolves \
             to, got {reported:?}"
        );

        // The two members an output carries BESIDE its value, where it declares them.
        let descriptions = pairs_of(outputs, "OutputKey", "Description");
        assert_eq!(
            descriptions.get("FunctionName").map(String::as_str),
            Some(FUNCTION_NAME_DESCRIPTION),
            "case {case:?}: expected the declared Description beside the value, got \
             {descriptions:?}"
        );
        let expected_export = format!("{stack_name}-alias-arn");
        let exports = pairs_of(outputs, "OutputKey", "ExportName");
        assert_eq!(
            exports.get("ExportedAliasArn").map(String::as_str),
            Some(expected_export.as_str()),
            "case {case:?}: expected the resolved ExportName {expected_export:?}, got \
             {exports:?}"
        );

        // The alias EXISTS in the service that owns it, and the ARN the stack published as
        // the resource's physical id is the one that addresses it there.
        let alias_arn = format!("{}:{}", function_arn(row.function), row.alias);
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let physical_ids = physical_ids_of(&resources);
        assert_eq!(
            physical_ids.get("FnAlias").map(String::as_str),
            Some(alias_arn.as_str()),
            "case {case:?}: expected the alias's ARN as its physical id, got {physical_ids:?}"
        );

        let (status, code, alias) = lambda(
            &router,
            "GET",
            &format!(
                "/2015-03-31/functions/{}/aliases/{}",
                row.function, row.alias
            ),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (200, ""),
            "case {case:?}: expected GetAlias on the declared alias to be served, got {status} \
             {code:?} with {alias}"
        );
        assert_eq!(
            (
                alias.get("Name").and_then(Value::as_str),
                alias.get("FunctionVersion").and_then(Value::as_str),
                alias.get("AliasArn").and_then(Value::as_str),
            ),
            (
                Some(row.alias),
                Some(FIRST_VERSION),
                Some(alias_arn.as_str())
            ),
            "case {case:?}: expected the alias the template declared, invoking the version the \
             stack published, got {alias}"
        );

        // ...and only a name no alias of that function carries is the one it does not hold.
        let undeclared = format!("{}-undeclared", row.alias);
        let (status, code, refused) = lambda(
            &router,
            "GET",
            &format!(
                "/2015-03-31/functions/{}/aliases/{undeclared}",
                row.function
            ),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected GetAlias on a name nothing declared to be refused, got \
             {status} {code:?} with {refused}"
        );
        let expected_message = format!(
            "Cannot find alias arn: {}:{undeclared}",
            function_arn(row.function)
        );
        let message = refused
            .get("Message")
            .or_else(|| refused.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected the refusal to name the alias ARN it was asked about, got \
             {message:?}"
        );
    }
}
