// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack calls the Amazon Redshift cluster parameter group it declares,
//! and what the sibling cluster configured from it then resolves.
//!
//! The rule under test: **a declaration that STATES the name property of its type carries that
//! evaluated value as the resource's physical id — whatever intrinsic the template spells it
//! with — and CloudFormation generates a `<stack>-<logical id>-<unique id>` name only where the
//! property is absent; the resource is created in the service that OWNS it under exactly that
//! physical id, so `Ref` and that service's own read agree and a sibling resource that depends
//! on it BY NAME resolves it.** The owning service's not-found refusal is therefore reachable
//! only for a name nothing holds, and then it names the value the request carried.
//!
//! Two crates have to agree for that to hold — CloudFormation decides the physical id and makes
//! the calls, Amazon Redshift owns both the group and the cluster configured from it — so the
//! suite lives here: a crate-local test target can only ever see one side of it.
//!
//! The deploy rows differ along what the rule generalises over: HOW the template states the
//! name (a literal, an `Fn::Sub` over a template parameter, an `Fn::Join` of segments, and no
//! name at all), the template spelling (JSON with long-form intrinsics, YAML with the
//! short-form tags), which published family the group belongs to, and the logical id and stack
//! name a generated name is built from. The single configuration an issue happens to record —
//! one `Fn::Sub` name beside one single-node cluster — is one row of that table.
//!
//! The refusal rows differ along how the cluster's `ClusterParameterGroupName` misses: a name
//! nothing in the account was ever created under, a name shaped like the one CloudFormation
//! generates, and a near miss of a group the very same stack declares. Each must report the
//! owning service's own sentence naming THAT value, never the name of a group that does exist.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{
    block_of, elements, form_encode, opt_element, outputs_of, physical_ids_of,
};

/// The region every request here is signed for, and so the region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: the stack's own clock-owed transition and the created cluster's
/// provisioning span both have to come due, and the suite waits for the CONDITION (the stack's
/// status) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The resource types the rows declare, and the property the group states its name in — the
/// member `CreateClusterParameterGroup` creates a group under
/// (`smithy_query(query="operation", operation="CreateClusterParameterGroup")`).
const GROUP_TYPE: &str = "AWS::Redshift::ClusterParameterGroup";
const CLUSTER_TYPE: &str = "AWS::Redshift::Cluster";
const NAME_PROPERTY: &str = "ParameterGroupName";

/// The admin password every row declares. It must never be echoed by either service.
const PASSWORD: &str = "Cfn-Redsh1ft-Pg";

/// The node type and cluster type every row's cluster is created with: one node, so no row
/// waits for a topology it does not assert anything about.
const NODE_TYPE: &str = "ra3.xlplus";

/// How long a cluster parameter group's name may be, and the alphabet Amazon Redshift holds it
/// to: "Must be 1 to 255 alphanumeric characters or hyphens", "First character must be a
/// letter", "Cannot end with a hyphen or contain two consecutive hyphens", "This value is
/// stored as a lower-case string" (`smithy_query(query="doc",
/// shape="CreateClusterParameterGroupMessage", member="ParameterGroupName")`).
const MAX_GROUP_NAME_CHARACTERS: usize = 255;

/// The fixed text Amazon Redshift words a `ClusterParameterGroupNotFoundFault` with for an
/// operation that DEREFERENCES a group it does not hold — `CreateCluster` among them — and the
/// `HandlerErrorCode` class a refusal naming a resource that is not there falls in.
const NOT_FOUND_TEXT: &str = "ClusterParameterGroup not found: ";
const NOT_FOUND_CLASS: &str = "HandlerErrorCode: NotFound";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
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
        service_com_amazonaws_redshift_redshiftserviceversion20121201::EmulatedService::new(
            engine,
        ),
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

/// Call one query-protocol operation of `service`, at `version`, with these form members.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// Call one Amazon Redshift operation over its awsQuery wire.
async fn redshift(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "redshift", "2012-12-01", action, members).await
}

/// Why a stack's resources failed, as CloudFormation records it on the stack's own events:
/// quoted in an assertion message, so a stack that did not complete says what the owning
/// service refused rather than only that it rolled back.
async fn failure_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the
/// stack has settled.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let settled = opt_element(&body, "StackStatus")
            .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || std::time::Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create the stack `stack_name` from `template` and answer its settled `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// Whether `name` is a name Amazon Redshift's published syntax for a cluster parameter group
/// admits, which is what a name CloudFormation GENERATES for one has to be inside.
fn is_a_published_group_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_GROUP_NAME_CHARACTERS
        && name.starts_with(|first: char| first.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// The logical ids a row's template declares its two resources under.
const CLUSTER_LOGICAL_ID: &str = "Warehouse";

/// The setting the rows' groups declare, with the value they declare: one of the parameters a
/// `redshift-*` family publishes (`DescribeDefaultClusterParameters` lists
/// `enable_user_activity_logging` among them, whose engine default is `false`).
///
/// The type's `Parameters` is the `Parameter` LIST `ModifyClusterParameterGroup` takes, so a
/// declaration carrying one is a group created and then written, which is the ordinary shape of
/// such a declaration.
fn declared_parameters() -> serde_json::Value {
    json!([{"ParameterName": "enable_user_activity_logging", "ParameterValue": "true"}])
}

/// One row of the deploy table: the stack it creates and the name its group must come out
/// carrying.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The logical id the group is declared under, which a GENERATED name is built from.
    group_logical_id: &'static str,
    cluster_identifier: &'static str,
    /// The published family the group declares
    /// (`CreateClusterParameterGroup$ParameterGroupFamily`).
    family: &'static str,
    /// The name the declaration asks for, once the template's intrinsics are evaluated, or
    /// `None` where the declaration states no name at all.
    expected_name: Option<&'static str>,
    template: String,
}

/// The rows: the axis is how the template states the group's name, so the rule is not keyed to
/// one declaration shape. Each stack also declares the cluster configured FROM that group by
/// `Ref`, which is the dependency that only resolves if the group really is held under the
/// published name.
fn rows() -> Vec<Row> {
    vec![
        // A name stated as a literal, in JSON: the plainest form of the rule.
        Row {
            case: "a group whose name the template states literally",
            stack_name: "rs-pg-literal",
            group_logical_id: "Settings",
            cluster_identifier: "cfn-pg-literal-cluster",
            family: "redshift-1.0",
            expected_name: Some("cfn-pg-literal"),
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Settings": {
                        "Type": GROUP_TYPE,
                        "Properties": {
                            NAME_PROPERTY: "cfn-pg-literal",
                            "ParameterGroupFamily": "redshift-1.0",
                            "Description": "the group the stack names literally",
                            "Parameters": declared_parameters(),
                        }
                    },
                    CLUSTER_LOGICAL_ID: {
                        "Type": CLUSTER_TYPE,
                        "Properties": {
                            "ClusterIdentifier": "cfn-pg-literal-cluster",
                            "ClusterType": "single-node",
                            "NodeType": NODE_TYPE,
                            "MasterUsername": "admin",
                            "MasterUserPassword": PASSWORD,
                            "ClusterParameterGroupName": {"Ref": "Settings"},
                        }
                    }
                },
                "Outputs": {
                    "GroupRef": {"Value": {"Ref": "Settings"}},
                    "ClusterRef": {"Value": {"Ref": CLUSTER_LOGICAL_ID}}
                }
            })
            .to_string(),
        },
        // The recorded shape: the name composed by an `Fn::Sub` over a template parameter, so
        // the physical id is the EVALUATED value rather than the text the template carries.
        Row {
            case: "a group whose name an Fn::Sub over a parameter composes",
            stack_name: "rs-pg-substituted",
            group_logical_id: "RedshiftParameterGroup",
            cluster_identifier: "cfn-pg-sub-global",
            family: "redshift-2.0",
            expected_name: Some("cfn-pg-sub-pg"),
            template: json!({
                "Parameters": {"Token": {"Type": "String", "Default": "cfn-pg-sub"}},
                "Resources": {
                    "RedshiftParameterGroup": {
                        "Type": GROUP_TYPE,
                        "Properties": {
                            NAME_PROPERTY: {"Fn::Sub": "${Token}-pg"},
                            "ParameterGroupFamily": "redshift-2.0",
                            "Description": "the group the stack names through a parameter",
                            "Parameters": declared_parameters(),
                        }
                    },
                    CLUSTER_LOGICAL_ID: {
                        "Type": CLUSTER_TYPE,
                        "Properties": {
                            "ClusterIdentifier": {"Fn::Sub": "${Token}-global"},
                            "ClusterType": "single-node",
                            "NodeType": NODE_TYPE,
                            "DBName": "dev",
                            "MasterUsername": "admin",
                            "MasterUserPassword": PASSWORD,
                            "Encrypted": true,
                            "ClusterParameterGroupName": {"Ref": "RedshiftParameterGroup"},
                        }
                    }
                },
                "Outputs": {
                    "GroupRef": {"Value": {"Ref": "RedshiftParameterGroup"}},
                    "ClusterRef": {"Value": {"Ref": CLUSTER_LOGICAL_ID}}
                }
            })
            .to_string(),
        },
        // The same rule over a name JOINED from segments, in YAML with the short-form tags:
        // another intrinsic, another spelling, the same physical id.
        Row {
            case: "a group whose name an Fn::Join builds, in YAML",
            stack_name: "rs-pg-joined",
            group_logical_id: "JoinedSettings",
            cluster_identifier: "cfn-pg-joined-cluster",
            family: "redshift-1.0",
            expected_name: Some("cfn-pg-joined"),
            template: format!(
                "\
Resources:
  JoinedSettings:
    Type: {GROUP_TYPE}
    Properties:
      {NAME_PROPERTY}: !Join ['-', ['cfn', 'pg', 'joined']]
      ParameterGroupFamily: redshift-1.0
      Description: the group the stack names by joining segments
      Parameters:
        - ParameterName: enable_user_activity_logging
          ParameterValue: 'true'
  {CLUSTER_LOGICAL_ID}:
    Type: {CLUSTER_TYPE}
    Properties:
      ClusterIdentifier: cfn-pg-joined-cluster
      ClusterType: single-node
      NodeType: {NODE_TYPE}
      MasterUsername: admin
      MasterUserPassword: {PASSWORD}
      ClusterParameterGroupName: !Ref JoinedSettings
Outputs:
  GroupRef:
    Value: !Ref JoinedSettings
  ClusterRef:
    Value: !Ref {CLUSTER_LOGICAL_ID}
"
            ),
        },
        // And the other side of the rule: a declaration that states NO name, whose physical id
        // is the `<stack>-<logical id>-<unique id>` CloudFormation generates — which the owning
        // service then has to hold the group under just as exactly.
        Row {
            case: "a group the template does not name at all",
            stack_name: "rs-pg-generated",
            group_logical_id: "UnnamedSettings",
            cluster_identifier: "cfn-pg-generated-cluster",
            family: "redshift-2.0",
            expected_name: None,
            template: json!({
                "Resources": {
                    "UnnamedSettings": {
                        "Type": GROUP_TYPE,
                        "Properties": {
                            "ParameterGroupFamily": "redshift-2.0",
                            "Description": "the group CloudFormation names",
                        }
                    },
                    CLUSTER_LOGICAL_ID: {
                        "Type": CLUSTER_TYPE,
                        "Properties": {
                            "ClusterIdentifier": "cfn-pg-generated-cluster",
                            "ClusterType": "single-node",
                            "NodeType": NODE_TYPE,
                            "MasterUsername": "admin",
                            "MasterUserPassword": PASSWORD,
                            "ClusterParameterGroupName": {"Ref": "UnnamedSettings"},
                        }
                    }
                },
                "Outputs": {
                    "GroupRef": {"Value": {"Ref": "UnnamedSettings"}},
                    "ClusterRef": {"Value": {"Ref": CLUSTER_LOGICAL_ID}}
                }
            })
            .to_string(),
        },
    ]
}

/// The physical id CloudFormation reports for one of the stack's resources.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    logical_id: &str,
) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    let ids: BTreeMap<String, String> = physical_ids_of(&body);
    ids.get(logical_id)
        .unwrap_or_else(|| panic!("case {case:?}: no physical id for {logical_id} in {body}"))
        .clone()
}

#[tokio::test]
async fn a_declared_parameter_group_name_is_the_name_redshift_holds_the_group_under() {
    let router = build_router();

    for row in rows() {
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {body} with the failure reasons \
             {:?}",
            failure_reasons(&router, row.stack_name).await
        );
        let outputs = outputs_of(&body);
        let published = outputs
            .get("GroupRef")
            .unwrap_or_else(|| panic!("case {case:?}: no GroupRef output in {body}"))
            .clone();

        // THE RULE's first half: what the stack publishes for the group is the name the
        // declaration ASKED for, and a name CloudFormation generated only where it asked for
        // none.
        match row.expected_name {
            Some(declared) => assert_eq!(
                published, declared,
                "case {case:?}: expected the evaluated {NAME_PROPERTY} as the group's Ref, got \
                 {published:?}"
            ),
            None => {
                let prefix =
                    format!("{}-{}-", row.stack_name, row.group_logical_id).to_lowercase();
                let drawn = published.strip_prefix(&prefix).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected a generated <stack>-<logical id>-<unique id> \
                         name under {prefix:?}, got {published:?}"
                    )
                });
                assert!(
                    !drawn.is_empty() && drawn.chars().all(|c| c.is_ascii_alphanumeric()),
                    "case {case:?}: expected a unique segment after {prefix:?}, got \
                     {published:?}"
                );
                assert!(
                    is_a_published_group_name(&published),
                    "case {case:?}: expected a generated name inside Amazon Redshift's \
                     published syntax, got {published:?}"
                );
            }
        }

        // The handle the stack publishes IS the resource's physical id, which is what every
        // later call about it — a cross-stack reference, a modify, the delete — is keyed on.
        let recorded = physical_id(&router, case, row.stack_name, row.group_logical_id).await;
        assert_eq!(
            recorded, published,
            "case {case:?}: expected the group's physical id to be the published name, got \
             {recorded:?}"
        );

        // THE RULE's second half: Amazon Redshift holds a group under exactly that name, with
        // the family the declaration stated.
        let (status, groups) = redshift(
            &router,
            "DescribeClusterParameterGroups",
            &[(NAME_PROPERTY, published.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon Redshift to report a group under {published:?}, got \
             {status} with body {groups}"
        );
        let group = block_of(&groups, "ClusterParameterGroup")
            .unwrap_or_else(|| panic!("case {case:?}: no ClusterParameterGroup in {groups}"));
        assert_eq!(
            (
                opt_element(group, NAME_PROPERTY).as_deref(),
                opt_element(group, "ParameterGroupFamily").as_deref()
            ),
            (Some(published.as_str()), Some(row.family)),
            "case {case:?}: expected the group to be held under the published name and family, \
             got {group}"
        );

        // ...and the sibling that depends on it BY NAME resolved it: the cluster the stack
        // created is available and configured from that very group, so no `Ref` handed Amazon
        // Redshift a name it holds nothing under.
        let (status, reported) = redshift(
            &router,
            "DescribeClusters",
            &[("ClusterIdentifier", row.cluster_identifier)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon Redshift to report the cluster the stack created, \
             got {status} with body {reported}"
        );
        let cluster = block_of(&reported, "Cluster")
            .unwrap_or_else(|| panic!("case {case:?}: no Cluster in {reported}"));
        assert_eq!(
            opt_element(cluster, "ClusterStatus").as_deref(),
            Some("available"),
            "case {case:?}: expected a completed stack to leave the cluster available, got \
             {cluster}"
        );
        let memberships = block_of(cluster, "ClusterParameterGroups").unwrap_or_else(|| {
            panic!("case {case:?}: no ClusterParameterGroups on the cluster in {reported}")
        });
        assert_eq!(
            opt_element(memberships, NAME_PROPERTY).as_deref(),
            Some(published.as_str()),
            "case {case:?}: expected the cluster to be configured from the published group, got \
             {memberships}"
        );
        assert_eq!(
            outputs.get("ClusterRef").map(String::as_str),
            Some(row.cluster_identifier),
            "case {case:?}: expected the declared identifier as the cluster's Ref, got \
             {outputs:?}"
        );

        // The refusal this suite's second half pins must not be reachable here at all: the
        // group the same stack created is a group that exists.
        let reasons = failure_reasons(&router, row.stack_name).await;
        assert!(
            !reasons.iter().any(|reason| reason.contains(NOT_FOUND_TEXT)),
            "case {case:?}: expected no not-found refusal for a group the same stack created, \
             got {reasons:?}"
        );
        assert!(
            !reported.contains(PASSWORD) && !groups.contains(PASSWORD),
            "case {case:?}: the admin password must never reach the wire, got {reported}"
        );
    }
}

/// One refusal row: how the cluster's `ClusterParameterGroupName` misses.
struct Missing {
    case: &'static str,
    stack_name: &'static str,
    cluster_identifier: &'static str,
    /// The group the template declares beside the cluster, or `None` where it declares none.
    declared_group: Option<&'static str>,
    /// The name the cluster asks for, which nothing in the account holds a group under.
    named: &'static str,
}

/// The rows: each names a group that is genuinely absent, in a different relation to the groups
/// the account does hold.
const MISSING: &[Missing] = &[
    // A name nothing in the account was ever created under.
    Missing {
        case: "a parameter group name nothing holds",
        stack_name: "rs-pg-absent",
        cluster_identifier: "cfn-pg-absent-cluster",
        declared_group: None,
        named: "cfn-pg-never-created",
    },
    // A name SHAPED like the one CloudFormation generates, which is what the defect this suite
    // pins used to hand Amazon Redshift for a group the stack had just created under another
    // name: absent is absent, and the refusal names the value asked for.
    Missing {
        case: "a name shaped like a generated physical id",
        stack_name: "rs-pg-generated-shape",
        cluster_identifier: "cfn-pg-generated-shape-cluster",
        declared_group: None,
        named: "rs-pg-generated-shape-settings-1a2b3c4d",
    },
    // A NEAR MISS of a group the very same stack declares: the refusal has to name what the
    // request carried, not the group that does exist.
    Missing {
        case: "a near miss of the group the same stack declares",
        stack_name: "rs-pg-near-miss",
        cluster_identifier: "cfn-pg-near-miss-cluster",
        declared_group: Some("cfn-pg-near-miss"),
        named: "cfn-pg-near-miss-2",
    },
];

#[tokio::test]
async fn a_cluster_naming_an_absent_parameter_group_fails_with_that_name() {
    for row in MISSING {
        let router = build_router();
        let case = row.case;
        let group_resource = match row.declared_group {
            None => String::new(),
            Some(name) => format!(
                r#","Settings":{{"Type":"{GROUP_TYPE}","Properties":{{"{NAME_PROPERTY}":"{name}",
                   "ParameterGroupFamily":"redshift-1.0","Description":"the group that exists"}}}}"#
            ),
        };
        let template = format!(
            r#"{{"Resources":{{"{CLUSTER_LOGICAL_ID}":{{"Type":"{CLUSTER_TYPE}","Properties":{{
                 "ClusterIdentifier":"{}","ClusterType":"single-node","NodeType":"{NODE_TYPE}",
                 "MasterUsername":"admin","MasterUserPassword":"{PASSWORD}",
                 "ClusterParameterGroupName":"{}"}}}}{group_resource}}},
               "Outputs":{{"ClusterRef":{{"Value":{{"Ref":"{CLUSTER_LOGICAL_ID}"}}}}}}}}"#,
            row.cluster_identifier, row.named
        );

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back rather than publish a \
             cluster Amazon Redshift never held, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        // The owning service's own sentence, naming the value the REQUEST carried, reported on
        // the resource that failed and classified as a resource that is not there. The closing
        // quote of the handler envelope is part of what is matched, so the sentence has to END
        // with that value rather than merely begin with it.
        let sentence = format!("{NOT_FOUND_TEXT}{}\"", row.named);
        let reasons = failure_reasons(&router, row.stack_name).await;
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains(&sentence) && reason.contains(NOT_FOUND_CLASS)),
            "case {case:?}: expected a failure reason carrying {sentence:?} and \
             {NOT_FOUND_CLASS:?}, got {reasons:?}"
        );
        // And never the name of a group that DOES exist: a refusal naming another group would
        // send its reader looking for the wrong missing dependency.
        if let Some(existing) = row.declared_group {
            let misnamed = format!("{NOT_FOUND_TEXT}{existing}\"");
            assert!(
                !reasons.iter().any(|reason| reason.contains(&misnamed)),
                "case {case:?}: expected the refusal to name the value asked for rather than \
                 {existing:?}, got {reasons:?}"
            );
        }

        // Nothing was created: Amazon Redshift holds no cluster under the declared identifier.
        let (status, reported) = redshift(
            &router,
            "DescribeClusters",
            &[("ClusterIdentifier", row.cluster_identifier)],
        )
        .await;
        assert_eq!(
            (status, opt_element(&reported, "Code").as_deref()),
            (404, Some("ClusterNotFound")),
            "case {case:?}: expected Amazon Redshift to hold no cluster named {:?}, got {status} \
             with body {reported}",
            row.cluster_identifier
        );
    }
}
