// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::SSMQuickSetup::ConfigurationManager` deploys:
//! the manager is created in AWS Systems Manager Quick Setup's own control plane, the stack
//! publishes the ARN that service minted, renaming it in the template renames the manager that
//! service holds, deleting the stack takes it away — and a declaration that service REFUSES
//! fails the resource with its own refusal and rolls the stack back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the stack
//! publishes is what that service answered, and what that service refuses is a FAILED RESOURCE
//! rather than a template-format error.** Three crates have to agree for that to hold
//! (CloudFormation submits the template and makes the call, Quick Setup serves it, and IAM
//! holds the role a local deployment is administered with), so the suite lives here: a
//! crate-local test can only ever see one side of it.
//!
//! The rows of the deploy table differ along what the rule generalises over: which
//! configuration type a definition deploys, how the deployment is targeted (an account by tag,
//! an organizational unit), how many definitions one manager carries, whether the declaration
//! states a name, a description, a type version and the type's own `Tags` at all, whether the
//! roles a local deployment needs are resources of the same stack, which published return value
//! the output reads (`Ref`, `Fn::GetAtt ManagerArn`, `Fn::GetAtt CreatedAt`) and the template
//! spelling (JSON with long-form intrinsics, YAML with the short-form tags).
//!
//! The refusal rows differ along the CLASS of declaration Quick Setup refuses: a configuration
//! type it does not offer, a parameter set that names nowhere to deploy, and a local
//! deployment whose administration role this account's IAM does not show trusting the
//! deploying service. Each is the owning service's own refusal, reported on the resource that
//! failed — never the `Template format error: Unrecognized resource types` a type
//! CloudFormation does not know is refused with, which no row of either table may ever see.
//!
//! Nothing here waits a fixed time: a stack's transition and the manager's deployment are both
//! clock-owed, so every wait polls the condition until it holds or a generous deadline passes
//! ([`poll_until`], [`settled_stack`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery percent-encoder and the XML readers every awsQuery suite in this crate shares,
// written once in `all/query_wire.rs` so no suite re-derives the encoding or the reading its
// assertions depend on.
use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack and the manager it
/// deploys belong to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// How long a clock-owed transition is waited for before the wait is a failure: generously more
/// than the spans the stack and Quick Setup's own deployment owe, so a loaded host still
/// reaches the condition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Quick Setup's signing name, which is also its ARN namespace
/// (`sigv4Name: ssm-quicksetup`, `awsService.arnNamespace: ssm-quicksetup`).
const QUICK_SETUP: &str = "ssm-quicksetup";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The resource type under test, and the logical id every template below declares it under.
const CONFIGURATION_MANAGER_TYPE: &str = "AWS::SSMQuickSetup::ConfigurationManager";
const MANAGER: &str = "ConfigManager";

/// The service principal a local deployment's administration role has to admit: Quick Setup
/// deploys a configuration definition as a CloudFormation stack set, so CloudFormation is what
/// acts as that role.
const DEPLOYER_PRINCIPAL: &str = "cloudformation.amazonaws.com";

/// The ARN prefix a configuration manager of this Region carries (the modelled `ManagerArn`
/// pattern
/// `^arn:aws:ssm-quicksetup:([^:]+):(\d{12}):configuration-manager/<uuid>$`).
fn manager_arn_prefix() -> String {
    format!("arn:aws:ssm-quicksetup:{REGION}:{ACCOUNT_ID}:configuration-manager/")
}

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation, the Quick Setup control plane the
/// stack calls, and IAM — because Quick Setup resolves a local deployment's administration role
/// against this account's IAM records before it stores the manager.
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
        service_com_amazonaws_ssmquicksetup_quicksetup::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
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

/// Call one Quick Setup operation over its own restJson1 wire, on the method and URI its
/// `@http` binding names.
async fn quick_setup(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|document| document.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("ssm-quicksetup.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(QUICK_SETUP))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GetConfigurationManager` for one ARN. The ARN is written into the single path segment the
/// `@httpLabel` binds, escaped the way an AWS SDK escapes one — its `:` and `/` included — so
/// it addresses one manager rather than another path.
async fn get_manager(router: &MultiServiceRouter, manager_arn: &str) -> (u16, Value) {
    let uri = format!("/configurationManager/{}", form_encode(manager_arn));
    quick_setup(router, "GET", &uri, None).await
}

/// The ARNs `ListConfigurationManagers` reports for this account and Region.
async fn listed_managers(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, answered) =
        quick_setup(router, "POST", "/listConfigurationManagers", Some(json!({}))).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListConfigurationManagers, got {status} with \
         {answered}"
    );
    answered
        .get("ConfigurationManagersList")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(|summary| summary.get("ManagerArn").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The tags Quick Setup itself holds for one resource, read through its own
/// `ListTagsForResource`.
async fn held_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> BTreeMap<String, String> {
    let uri = format!("/tags/{}", form_encode(arn));
    let (status, answered) = quick_setup(router, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} with {answered}"
    );
    answered
        .get("Tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    Some((
                        tag.get("Key").and_then(Value::as_str)?.to_string(),
                        tag.get("Value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look again until `condition` holds or [`SETTLE_DEADLINE`] passes, panicking with `case` and
/// whatever the last look saw.
///
/// Every wait in this suite goes through here: what is being waited for is a CONDITION the
/// clock makes true, never an elapsed time, so nothing in this file sleeps for a span and then
/// asserts.
async fn poll_until<F, Fut>(case: &str, waiting_for: &str, mut look: F) -> String
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = (bool, String)>,
{
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (held, seen) = look().await;
        if held {
            return seen;
        }
        if Instant::now() >= deadline {
            panic!("case {case:?}: timed out waiting for {waiting_for}; last saw {seen}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether a stack status is one a deployment STOPS in, rather than one it is passing through:
/// CloudFormation's own statuses end in `_IN_PROGRESS` while work is outstanding.
fn is_terminal(stack_status: &str) -> bool {
    stack_status.ends_with("_COMPLETE") || stack_status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack rests in a terminal status, and answer that body.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    poll_until(case, "the stack to rest in a terminal status", || async {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        if status != 200 {
            return (false, format!("HTTP {status} from DescribeStacks: {body}"));
        }
        let settled = opt_element(&body, "StackStatus").is_some_and(|it| is_terminal(&it));
        (settled, body)
    })
    .await
}

/// Create a stack, acknowledging the IAM resources a template may declare, then wait for it to
/// settle and answer the `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            // "If you have IAM resources with custom names, you must specify
            // CAPABILITY_NAMED_IAM", which acknowledges both forms.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an \
         AWS::SSMQuickSetup::ConfigurationManager is a type a template may declare — got \
         {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// One output the stack must report, as the text it carries.
fn published(case: &str, body: &str, key: &str) -> String {
    outputs_of(body)
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"))
        .clone()
}

/// The IAM role a LOCAL configuration deployment is administered with, as a resource of the
/// template: Quick Setup resolves it against this account's IAM before it stores the manager,
/// so the role has to be one IAM really holds with a trust policy admitting the deployer.
///
/// `principal` is the service the trust policy admits, which is what the refusal table varies.
fn administration_role(principal: &str) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {"Service": principal},
                    "Action": "sts:AssumeRole",
                }],
            },
        },
    })
}

/// The role a local deployment EXECUTES as, named by the definition rather than by ARN.
fn execution_role() -> Value {
    administration_role("ssm.amazonaws.com")
}

/// What one row expects of the manager its stack declares.
struct Declared {
    /// The output key whose value is the ARN Quick Setup minted.
    arn_output: &'static str,
    /// The output key whose value is the instant that service recorded, where the row reads one.
    created_at_output: Option<&'static str>,
    /// The `Name` the declaration states, or `None` where it states none.
    name: Option<&'static str>,
    /// The `Description` the declaration states, where it states one.
    description: Option<&'static str>,
    /// The configuration types the declared definitions deploy, in declaration order.
    definition_types: Vec<&'static str>,
    /// The tags the manager must end up carrying.
    tags: BTreeMap<String, String>,
}

/// One row: a template and what the manager it declares must be.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Declared,
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case: one host-management definition deployed locally, administered with
        // a role the same stack creates, with every published return value read back, in JSON
        // with long-form intrinsics.
        Row {
            case: "a locally deployed host-management definition, every return value read back",
            stack_name: "quicksetup-manager-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "AdminRole": administration_role(DEPLOYER_PRINCIPAL),
                    "ExecRole": execution_role(),
                    MANAGER: {
                        "Type": CONFIGURATION_MANAGER_TYPE,
                        "Properties": {
                            "Name": "host-mgmt-fleet",
                            "Description": "the managed host fleet",
                            "ConfigurationDefinitions": [{
                                "Type": "AWSQuickSetupType-SSMHostMgmt",
                                "Parameters": {
                                    "TargetType": "Tags",
                                    "TargetTagKey": "Environment",
                                    "TargetTagValue": "production",
                                    "TargetAccounts": {"Ref": "AWS::AccountId"},
                                    "TargetRegions": {"Ref": "AWS::Region"},
                                },
                                "LocalDeploymentAdministrationRoleArn": {
                                    "Fn::GetAtt": ["AdminRole", "Arn"]
                                },
                                "LocalDeploymentExecutionRoleName": {"Ref": "ExecRole"},
                            }],
                            "Tags": {"team": "platform"},
                        }
                    }
                },
                "Outputs": {
                    "ManagerArnRef": {"Value": {"Ref": MANAGER}},
                    "ManagerArnAtt": {"Value": {"Fn::GetAtt": [MANAGER, "ManagerArn"]}},
                    "CreatedAt": {"Value": {"Fn::GetAtt": [MANAGER, "CreatedAt"]}},
                }
            })
            .to_string(),
            declared: Declared {
                arn_output: "ManagerArnRef",
                created_at_output: Some("CreatedAt"),
                name: Some("host-mgmt-fleet"),
                description: Some("the managed host fleet"),
                definition_types: vec!["AWSQuickSetupType-SSMHostMgmt"],
                tags: [("team".to_string(), "platform".to_string())]
                    .into_iter()
                    .collect(),
            },
        },
        // A manager deployed through an ORGANIZATION rather than into an account — which needs
        // no administration role at all — declaring no name, no description and no tags, in
        // YAML with the short-form intrinsic, read back through `Fn::GetAtt ManagerArn`.
        Row {
            case: "an organization-targeted definition with no name, YAML spelling",
            stack_name: "quicksetup-manager-organization",
            template: format!(
                "\
Resources:
  {MANAGER}:
    Type: {CONFIGURATION_MANAGER_TYPE}
    Properties:
      ConfigurationDefinitions:
        - Type: AWSQuickSetupType-ResourceExplorer
          TypeVersion: '1.1'
          Parameters:
            SelectedAggregatorRegion: {REGION}
            ReplaceExistingAggregator: 'false'
            TargetOrganizationalUnits: ou-1234-abcdefgh
            TargetRegions: {REGION}
Outputs:
  ManagerArn:
    Value: !GetAtt {MANAGER}.ManagerArn
  ManagerCreatedAt:
    Value: !GetAtt {MANAGER}.CreatedAt
"
            ),
            declared: Declared {
                arn_output: "ManagerArn",
                created_at_output: Some("ManagerCreatedAt"),
                name: None,
                description: None,
                definition_types: vec!["AWSQuickSetupType-ResourceExplorer"],
                tags: BTreeMap::new(),
            },
        },
        // TWO definitions in one manager — a second configuration type, with a type version
        // stated — read back through `Ref` alone, with two declared tags.
        Row {
            case: "two definitions in one manager, read back through Ref",
            stack_name: "quicksetup-manager-two-definitions",
            template: json!({
                "Resources": {
                    "AdminRole": administration_role(DEPLOYER_PRINCIPAL),
                    MANAGER: {
                        "Type": CONFIGURATION_MANAGER_TYPE,
                        "Properties": {
                            "Name": "fleet-operations_2026",
                            "ConfigurationDefinitions": [
                                {
                                    "Type": "AWSQuickSetupType-SSMHostMgmt",
                                    "TypeVersion": "1.1",
                                    "Parameters": {
                                        "TargetType": "Tags",
                                        "TargetTagKey": "Environment",
                                        "TargetTagValue": "staging",
                                        "TargetAccounts": {"Ref": "AWS::AccountId"},
                                        "TargetRegions": {"Ref": "AWS::Region"},
                                    },
                                    "LocalDeploymentAdministrationRoleArn": {
                                        "Fn::GetAtt": ["AdminRole", "Arn"]
                                    },
                                },
                                {
                                    "Type": "AWSQuickSetupType-Distributor",
                                    "Parameters": {
                                        "PackagesToInstall": "AWSCloudWatchAgent",
                                        "TargetType": "Tags",
                                        "TargetTagKey": "Environment",
                                        "TargetTagValue": "staging",
                                        "TargetAccounts": {"Ref": "AWS::AccountId"},
                                        "TargetRegions": {"Ref": "AWS::Region"},
                                    },
                                    "LocalDeploymentAdministrationRoleArn": {
                                        "Fn::GetAtt": ["AdminRole", "Arn"]
                                    },
                                },
                            ],
                            "Tags": {"team": "platform", "owner": "fleet-ops"},
                        }
                    }
                },
                "Outputs": {"ManagerArnRef": {"Value": {"Ref": MANAGER}}}
            })
            .to_string(),
            declared: Declared {
                arn_output: "ManagerArnRef",
                created_at_output: None,
                name: Some("fleet-operations_2026"),
                description: None,
                definition_types: vec![
                    "AWSQuickSetupType-SSMHostMgmt",
                    "AWSQuickSetupType-Distributor",
                ],
                tags: [
                    ("owner".to_string(), "fleet-ops".to_string()),
                    ("team".to_string(), "platform".to_string()),
                ]
                .into_iter()
                .collect(),
            },
        },
    ]
}

/// Whether `arn` has the form a `ManagerArn` publishes: this Region's and account's
/// configuration-manager path, with a uuid inside it.
fn is_manager_arn(arn: &str) -> bool {
    let Some(uuid) = arn.strip_prefix(&manager_arn_prefix()) else {
        return false;
    };
    uuid.len() == 36 && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// One instant as the seconds it denotes, however the wire wrote it: Quick Setup's timestamps
/// are `date-time` strings, and a stack's published attribute is the ISO-8601 text
/// CloudFormation publishes an instant as.
fn seconds_of(what: &str, written: &Value) -> f64 {
    if let Some(seconds) = written.as_f64() {
        return seconds;
    }
    let text = written.as_str().unwrap_or_default();
    instant_seconds(what, text)
}

/// The same reading of an instant written as text.
fn instant_seconds(what: &str, text: &str) -> f64 {
    aws_smithy_types::DateTime::from_str(text, aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| panic!("the {what} {text:?} is no ISO-8601 instant: {error}"))
        .as_secs_f64()
}

/// The configuration types one reported manager's definitions deploy, in the order that service
/// reports them.
fn reported_definition_types(reported: &Value) -> Vec<String> {
    reported
        .get("ConfigurationDefinitions")
        .and_then(Value::as_array)
        .map(|definitions| {
            definitions
                .iter()
                .filter_map(|definition| definition.get("Type").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_configuration_manager_quick_setup_minted() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body}"
        );

        let arn = published(case, &body, row.declared.arn_output);
        assert!(
            is_manager_arn(&arn),
            "case {case:?}: expected the published value to be an ARN of the modelled \
             configuration-manager form, got {arn:?}"
        );

        // The identifier the stack published is one the OWNING service resolves, and the
        // manager it resolves to is the one the template declared.
        let (status, reported) = get_manager(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Quick Setup to resolve the published ARN {arn:?}, got \
             {status} with {reported}"
        );
        assert_eq!(
            reported.get("ManagerArn").and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected the read to report the published ARN, got {reported}"
        );
        assert_eq!(
            reported.get("Name").and_then(Value::as_str),
            row.declared.name,
            "case {case:?}: expected the declared name, got {reported}"
        );
        assert_eq!(
            reported.get("Description").and_then(Value::as_str),
            row.declared.description,
            "case {case:?}: expected the declared description, got {reported}"
        );
        assert_eq!(
            reported_definition_types(&reported),
            row.declared.definition_types,
            "case {case:?}: expected the declared definitions to be deployed, got {reported}"
        );
        assert_eq!(
            held_tags(&router, case, &arn).await,
            row.declared.tags,
            "case {case:?}: expected the manager to carry the declared tags, read through \
             Quick Setup's own tag plane"
        );

        // ...and that service LISTS it, as the one manager this account and Region holds.
        let listed = listed_managers(&router, case).await;
        assert_eq!(
            listed,
            vec![arn.clone()],
            "case {case:?}: expected ListConfigurationManagers to report the published ARN \
             alone, got {listed:?}"
        );

        if let Some(key) = row.declared.created_at_output {
            let answered = published(case, &body, key);
            // The attribute is TEXT, and the text a `CreatedAt` carries is the ISO-8601
            // instant...
            assert!(
                answered.contains('T') && answered.ends_with('Z'),
                "case {case:?}: expected the published CreatedAt to be an ISO-8601 instant, got \
                 {answered:?}"
            );
            // ...of the very instant Quick Setup recorded for the manager: that service
            // records an instant at the precision its own `date-time` members report one, and
            // CloudFormation publishes a return value's instant at that same precision, so the
            // two are ONE instant rather than two roundings of it.
            let reported_at = seconds_of(
                "reported CreatedAt",
                reported
                    .get("CreatedAt")
                    .unwrap_or_else(|| panic!("case {case:?}: no CreatedAt in {reported}")),
            );
            let published_at = instant_seconds("published CreatedAt", &answered);
            let drift = reported_at - published_at;
            assert!(
                drift == 0.0,
                "case {case:?}: expected the published CreatedAt to be the instant Quick Setup \
                 recorded ({reported_at}), got {answered:?} ({drift} seconds apart)"
            );
        }

        // The second published return value, where the row reads both, is the same ARN: the
        // type's `Ref` and its `ManagerArn` attribute are one value.
        if let Some(other) = outputs_of(&body).get("ManagerArnAtt") {
            assert_eq!(
                other, &arn,
                "case {case:?}: expected Ref and Fn::GetAtt ManagerArn to publish one ARN, got \
                 {other:?} beside {arn:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_stack_update_renames_the_manager_quick_setup_holds() {
    let router = build_router();
    let stack_name = "quicksetup-manager-renamed";
    let manager = |name: &str, description: &str| {
        json!({
            "Resources": {
                "AdminRole": administration_role(DEPLOYER_PRINCIPAL),
                MANAGER: {
                    "Type": CONFIGURATION_MANAGER_TYPE,
                    "Properties": {
                        "Name": name,
                        "Description": description,
                        "ConfigurationDefinitions": [{
                            "Type": "AWSQuickSetupType-SSMHostMgmt",
                            "Parameters": {
                                "TargetType": "Tags",
                                "TargetTagKey": "Environment",
                                "TargetTagValue": "production",
                                "TargetAccounts": {"Ref": "AWS::AccountId"},
                                "TargetRegions": {"Ref": "AWS::Region"},
                            },
                            "LocalDeploymentAdministrationRoleArn": {
                                "Fn::GetAtt": ["AdminRole", "Arn"]
                            },
                        }],
                        "Tags": {"team": "platform"},
                    }
                }
            },
            "Outputs": {"ManagerArnRef": {"Value": {"Ref": MANAGER}}}
        })
        .to_string()
    };

    let created = create_and_settle(
        &router,
        "the first deployment",
        stack_name,
        &manager("first-name", "the first description"),
    )
    .await;
    assert_eq!(
        opt_element(&created, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {created}"
    );
    let arn = published("the first deployment", &created, "ManagerArnRef");

    // The rows differ along which of the two members `UpdateConfigurationManager` takes the
    // re-declaration changes: the name alone, then the name and the description together, and
    // the tag set with them.
    let rows: [(&str, &str, &str, BTreeMap<String, String>); 2] = [
        (
            "the name alone",
            "second-name",
            "the first description",
            [("team".to_string(), "platform".to_string())]
                .into_iter()
                .collect(),
        ),
        (
            "the name, the description and the tags together",
            "third name",
            "the third description",
            [("owner".to_string(), "fleet-ops".to_string())]
                .into_iter()
                .collect(),
        ),
    ];

    for (case, name, description, tags) in rows {
        let mut template: Value =
            serde_json::from_str(&manager(name, description)).expect("the template parses");
        template["Resources"][MANAGER]["Properties"]["Tags"] = json!(tags);
        let (status, body) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", stack_name),
                ("TemplateBody", &template.to_string()),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );
        let described = settled_stack(&router, case, stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {described}"
        );
        assert_eq!(
            outputs_of(&described).get("ManagerArnRef"),
            Some(&arn),
            "case {case:?}: expected the manager to keep the identity Quick Setup minted, got \
             {described}"
        );

        // The change reached the manager THAT service holds, not just the stack's record.
        let (status, reported) = get_manager(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Quick Setup to still hold {arn:?}, got {status} with \
             {reported}"
        );
        assert_eq!(
            reported.get("Name").and_then(Value::as_str),
            Some(name),
            "case {case:?}: expected the re-declared name, got {reported}"
        );
        assert_eq!(
            reported.get("Description").and_then(Value::as_str),
            Some(description),
            "case {case:?}: expected the re-declared description, got {reported}"
        );
        assert_eq!(
            held_tags(&router, case, &arn).await,
            tags,
            "case {case:?}: expected the re-declared tag set"
        );
        let listed = listed_managers(&router, case).await;
        assert_eq!(
            listed,
            vec![arn.clone()],
            "case {case:?}: expected the one manager to be changed rather than a second one \
             created, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_configuration_manager_it_created() {
    let router = build_router();
    let case = "a manager deleted with its stack";
    let stack_name = "quicksetup-manager-deleted";
    let template = json!({
        "Resources": {
            "AdminRole": administration_role(DEPLOYER_PRINCIPAL),
            MANAGER: {
                "Type": CONFIGURATION_MANAGER_TYPE,
                "Properties": {
                    "Name": "deleted-with-its-stack",
                    "ConfigurationDefinitions": [{
                        "Type": "AWSQuickSetupType-SSMHostMgmt",
                        "Parameters": {
                            "TargetType": "Tags",
                            "TargetTagKey": "Environment",
                            "TargetTagValue": "production",
                            "TargetAccounts": {"Ref": "AWS::AccountId"},
                            "TargetRegions": {"Ref": "AWS::Region"},
                        },
                        "LocalDeploymentAdministrationRoleArn": {
                            "Fn::GetAtt": ["AdminRole", "Arn"]
                        },
                    }],
                }
            }
        },
        "Outputs": {"ManagerArnRef": {"Value": {"Ref": MANAGER}}}
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body}"
    );
    let arn = published(case, &body, "ManagerArnRef");
    assert_eq!(
        listed_managers(&router, case).await,
        vec![arn.clone()],
        "case {case:?}: expected Quick Setup to hold the manager the stack created"
    );

    let (status, deleted) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );

    // The manager is gone from the service that held it — read both ways: its own listing no
    // longer reports it, and a read of the ARN the stack published answers that service's own
    // not-found.
    let last = poll_until(case, "Quick Setup to stop holding the manager", || async {
        let listed = listed_managers(&router, case).await;
        (listed.is_empty(), format!("{listed:?}"))
    })
    .await;
    assert_eq!(
        last, "[]",
        "case {case:?}: expected the manager to be gone from Quick Setup, got {last}"
    );
    let (status, reported) = get_manager(&router, &arn).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected Quick Setup to answer its own not-found for the deleted \
         manager, got {status} with {reported}"
    );
}

/// One refusal row: a declaration Quick Setup refuses, and what the failed resource must
/// report.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    /// The service principal the administration role's trust policy admits.
    role_principal: &'static str,
    /// The configuration type the definition deploys.
    definition_type: &'static str,
    /// The parameters the definition states.
    parameters: Value,
    /// The FIXED text of the sentence Quick Setup words that class of refusal with, which the
    /// resource's failure reason has to carry.
    sentence: &'static str,
}

fn refused_rows() -> Vec<Refused> {
    vec![
        // A configuration type that service does not offer names no template it could deploy.
        Refused {
            case: "a configuration type Quick Setup does not offer",
            stack_name: "quicksetup-manager-unknown-type",
            role_principal: DEPLOYER_PRINCIPAL,
            definition_type: "AWSQuickSetupType-NotAType",
            parameters: json!({
                "TargetAccounts": {"Ref": "AWS::AccountId"},
                "TargetRegions": {"Ref": "AWS::Region"},
            }),
            sentence: "must name a Quick Setup configuration type this service offers",
        },
        // A parameter set naming no Regions to deploy into says nowhere to deploy.
        Refused {
            case: "a parameter set naming no Regions to deploy into",
            stack_name: "quicksetup-manager-no-regions",
            role_principal: DEPLOYER_PRINCIPAL,
            definition_type: "AWSQuickSetupType-SSMHostMgmt",
            parameters: json!({"TargetAccounts": {"Ref": "AWS::AccountId"}}),
            sentence: "TargetRegions must be defined in parameters.",
        },
        // A local deployment whose administration role this account's IAM does not show
        // trusting the deploying service could never be administered.
        Refused {
            case: "an administration role that does not trust the deploying service",
            stack_name: "quicksetup-manager-untrusted-role",
            role_principal: "ec2.amazonaws.com",
            definition_type: "AWSQuickSetupType-SSMHostMgmt",
            parameters: json!({
                "TargetAccounts": {"Ref": "AWS::AccountId"},
                "TargetRegions": {"Ref": "AWS::Region"},
            }),
            sentence: "LocalDeploymentAdministrationRole trust policy does not allow AssumeRole",
        },
    ]
}

#[tokio::test]
async fn a_declaration_quick_setup_refuses_rolls_the_stack_back() {
    for row in refused_rows() {
        let router = build_router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "AdminRole": administration_role(row.role_principal),
                MANAGER: {
                    "Type": CONFIGURATION_MANAGER_TYPE,
                    "Properties": {
                        "Name": "refused-manager",
                        "ConfigurationDefinitions": [{
                            "Type": row.definition_type,
                            "Parameters": row.parameters,
                            "LocalDeploymentAdministrationRoleArn": {
                                "Fn::GetAtt": ["AdminRole", "Arn"]
                            },
                        }],
                    }
                }
            },
            "Outputs": {"ManagerArnRef": {"Value": {"Ref": MANAGER}}}
        })
        .to_string();

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             ARN Quick Setup never minted, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, and
        // never a refusal of the template: no event reports a template-format error at all.
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack_name)]).await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains(row.sentence)
                    && reason.contains("HandlerErrorCode: InvalidRequest")),
            "case {case:?}: expected a failure reason carrying {:?} and the InvalidRequest \
             handler class, got {reasons:?}",
            row.sentence
        );

        // And nothing was created: Quick Setup holds no manager at all.
        let listed = listed_managers(&router, case).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected Quick Setup to hold no manager, got {listed:?}"
        );
    }
}
