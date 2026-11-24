// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Athena::WorkGroup` or an
//! `AWS::Athena::DataCatalog` really creates it in Amazon Athena, and the handle the stack
//! publishes addresses the resource that service holds.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API and with every declared
//! property applied — so the identifier a stack publishes for it resolves in that service's own
//! read right after the stack completes, and deleting the stack releases it there.** For these
//! two types the identifier is the NAME: `Ref` returns the workgroup name and the data catalog
//! name, which is what `GetWorkGroup`, `GetDataCatalog`, `StartQueryExecution` and every
//! metadata read address one by. Two crates have to agree for that to hold (CloudFormation makes
//! the calls, Athena serves them), so the suite lives here: a crate-local test can only ever see
//! one side of it.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the
//! workgroup and the catalog or leaves both names to CloudFormation, whether it is written in
//! JSON or YAML, which optional properties it declares (description, state, enforcement, output
//! location, bytes-scanned cutoff, requester pays, CloudWatch metrics, tags), which
//! `DataCatalogType` the catalog is and therefore which parameter set it carries — including the
//! one type Athena creates ASYNCHRONOUSLY, whose stack is held open until that service reports
//! the catalog complete — and the Region the stack is deployed in.
//!
//! The deletion half and the refusal half are rows of the same rule: deleting the stack deletes
//! what it created in Athena, and a declaration Athena REFUSES leaves the stack
//! `ROLLBACK_COMPLETE` with nothing created behind it.
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares, or what the OWNING service reports for the resource.
//!
//! The awsQuery form encoder and the XML element readers are this test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of};

/// How long one `DescribeStacks` poll waits before the next, and how many are made.
///
/// A stack's own status transitions and the creation Athena carries out asynchronously for a
/// federated catalog are owed on their clocks, so the suite waits for the STATUS rather than for
/// a span, with a deadline generous enough for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 600;

/// The account id every ARN this emulator mints carries, which is the account whose Athena
/// resources these stacks create and the one a tag read addresses them in.
const ACCOUNT: &str = "000000000000";

/// The `x-amz-target` prefix of Amazon Athena: the local name of its Smithy service shape
/// (`com.amazonaws.athena#AmazonAthena`), which is how the awsJson protocols build that header.
const ATHENA_TARGET_PREFIX: &str = "AmazonAthena";

/// The longest a workgroup name may be (`WorkGroupName`, `^[a-zA-Z0-9._-]{1,128}$` in Athena's
/// own model), which a name CloudFormation GENERATES is held to.
const MAX_WORK_GROUP_NAME: usize = 128;

/// The longest a data catalog name may be ("can use a maximum of 127 alphanumeric, underscore,
/// at sign, or hyphen characters", `DataCatalog$Name`).
const MAX_CATALOG_NAME: usize = 127;

/// The state a workgroup a declaration states none for is in: `CreateWorkGroup` takes no state
/// member at all, so a created workgroup is `ENABLED`.
const ENABLED: &str = "ENABLED";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_athena_amazonathena::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon Athena operation over the awsJson1_1 wire its model declares, signed for
/// `region`.
async fn athena(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ATHENA_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("athena", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// Wait for `stack` to report `wanted`, looking again until it does or the deadline passes. A
/// stack that reaches another TERMINAL status fails the suite at once, quoting the answer: the
/// reason a resource failed is in it.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported_status == wanted {
            return body;
        }
        assert!(
            !reported_status.ends_with("_FAILED") && !reported_status.starts_with("ROLLBACK"),
            "stack {stack:?}: expected {wanted:?}, got {reported_status:?} with answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached {wanted:?}; last answer was {body}");
}

/// Wait for `stack` to reach any TERMINAL status, which is what a deployment that fails ends in.
async fn wait_for_terminal_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported_status.ends_with("_IN_PROGRESS") {
            return (reported_status, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached a terminal status; last answer was {body}");
}

/// One output of a `DescribeStacks` answer, or a panic naming the case and what was there.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// The ARN of one Athena resource, which is what that service's tag reads address
/// (`arn:${Partition}:athena:${Region}:${Account}:<type>/<name>`, the "Resource types defined by
/// Amazon Athena" table of the IAM Service Authorization Reference).
fn athena_arn(region: &str, resource_type: &str, name: &str) -> String {
    format!("arn:aws:athena:{region}:{ACCOUNT}:{resource_type}/{name}")
}

/// The tags Athena holds on one resource, read through its own `ListTagsForResource`.
async fn tags_on(
    router: &MultiServiceRouter,
    region: &str,
    resource_arn: &str,
    case: &str,
) -> BTreeMap<String, String> {
    let (status, listed) = athena(
        router,
        region,
        "ListTagsForResource",
        &json!({ "ResourceARN": resource_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource on {resource_arn}, got \
         {status} with body {listed}"
    );
    listed
        .get("Tags")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|tag| {
            Some((
                tag.get("Key").and_then(Value::as_str)?.to_string(),
                tag.get("Value").and_then(Value::as_str)?.to_string(),
            ))
        })
        .collect()
}

/// One row of the table: a template, and what the rule says the workgroup and the catalog it
/// declares must then be in Athena.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the resources are created in.
    region: &'static str,
    /// The stack name the row deploys under, whose NAME is the first segment of any name
    /// CloudFormation generates for a resource the template does not name.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The workgroup name the template declares, or `None` for a template naming none — whose
    /// workgroup carries the name CloudFormation generated.
    declared_work_group: Option<&'static str>,
    /// The catalog name it declares, under the same rule.
    declared_catalog: Option<&'static str>,
    /// The `State` `GetWorkGroup` must report.
    expected_state: &'static str,
    /// The `Description` it must report, if the row declares one.
    expected_description: Option<&'static str>,
    /// The `Configuration` members it must report, as `(the member path, the JSON value)`.
    expected_configuration: &'static [(&'static str, &'static str)],
    /// The `Type` `GetDataCatalog` must report.
    expected_catalog_type: &'static str,
    /// The `Parameters` it must report.
    expected_parameters: &'static [(&'static str, &'static str)],
    /// The tags the workgroup's own declaration asks for.
    expected_tags: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a named workgroup enforcing an output location, with a GLUE catalog, in JSON",
        region: "us-east-1",
        stack: "athena-json-stack",
        template: r#"{
          "AWSTemplateFormatVersion": "2010-09-09",
          "Resources": {
            "Analytics": {
              "Type": "AWS::Athena::WorkGroup",
              "Properties": {
                "Name": "orders-analytics",
                "Description": "the orders analytics workgroup",
                "RecursiveDeleteOption": true,
                "Tags": [{"Key": "Owner", "Value": "analytics"}],
                "WorkGroupConfiguration": {
                  "EnforceWorkGroupConfiguration": true,
                  "PublishCloudWatchMetricsEnabled": true,
                  "BytesScannedCutoffPerQuery": 20000000,
                  "ResultConfiguration": {"OutputLocation": "s3://orders-results/out/"}
                }
              }
            },
            "Catalog": {
              "Type": "AWS::Athena::DataCatalog",
              "Properties": {
                "Name": "orders_glue",
                "Type": "GLUE",
                "Description": "the orders Glue catalog",
                "Parameters": {"catalog-id": "000000000000"}
              }
            }
          },
          "Outputs": {
            "WorkGroupRef": {"Value": {"Ref": "Analytics"}},
            "CatalogRef": {"Value": {"Ref": "Catalog"}}
          }
        }"#,
        declared_work_group: Some("orders-analytics"),
        declared_catalog: Some("orders_glue"),
        expected_state: ENABLED,
        expected_description: Some("the orders analytics workgroup"),
        expected_configuration: &[
            ("/EnforceWorkGroupConfiguration", "true"),
            ("/PublishCloudWatchMetricsEnabled", "true"),
            ("/BytesScannedCutoffPerQuery", "20000000"),
            (
                "/ResultConfiguration/OutputLocation",
                "\"s3://orders-results/out/\"",
            ),
        ],
        expected_catalog_type: "GLUE",
        expected_parameters: &[("catalog-id", "000000000000")],
        expected_tags: &[("Owner", "analytics")],
    },
    Row {
        case: "a workgroup CloudFormation names, DISABLED, with a HIVE catalog, in YAML",
        region: "us-west-2",
        stack: "athena-yaml-stack",
        template: concat!(
            "Resources:\n",
            "  Reporting:\n",
            "    Type: AWS::Athena::WorkGroup\n",
            "    Properties:\n",
            "      State: DISABLED\n",
            "      WorkGroupConfiguration:\n",
            "        RequesterPaysEnabled: true\n",
            "        ResultConfiguration:\n",
            "          OutputLocation: s3://reporting-results/out/\n",
            "          ExpectedBucketOwner: '000000000000'\n",
            "  Metastore:\n",
            "    Type: AWS::Athena::DataCatalog\n",
            "    Properties:\n",
            "      Type: HIVE\n",
            "      Parameters:\n",
            "        metadata-function: arn:aws:lambda:us-west-2:000000000000:function:hive-meta\n",
            "Outputs:\n",
            "  WorkGroupRef:\n",
            "    Value: !Ref Reporting\n",
            "  CatalogRef:\n",
            "    Value: !Ref Metastore\n",
        ),
        declared_work_group: None,
        declared_catalog: None,
        expected_state: "DISABLED",
        expected_description: None,
        expected_configuration: &[
            ("/RequesterPaysEnabled", "true"),
            (
                "/ResultConfiguration/OutputLocation",
                "\"s3://reporting-results/out/\"",
            ),
            ("/ResultConfiguration/ExpectedBucketOwner", "\"000000000000\""),
        ],
        expected_catalog_type: "HIVE",
        expected_parameters: &[(
            "metadata-function",
            "arn:aws:lambda:us-west-2:000000000000:function:hive-meta",
        )],
        expected_tags: &[],
    },
    Row {
        case: "a workgroup whose quoted switches a String parameter feeds, with a LAMBDA catalog \
               carrying the composite function parameter",
        region: "eu-west-1",
        stack: "athena-parameterised-stack",
        template: r#"{
          "Parameters": {
            "Enforce": {"Type": "String", "Default": "true"},
            "Cutoff": {"Type": "String", "Default": "10000000"}
          },
          "Resources": {
            "Federated": {
              "Type": "AWS::Athena::WorkGroup",
              "Properties": {
                "Name": "sales.workgroup_1",
                "WorkGroupConfiguration": {
                  "EnforceWorkGroupConfiguration": {"Ref": "Enforce"},
                  "BytesScannedCutoffPerQuery": {"Ref": "Cutoff"},
                  "ResultConfiguration": {"OutputLocation": "s3://sales-results/out/"}
                }
              }
            },
            "Connector": {
              "Type": "AWS::Athena::DataCatalog",
              "Properties": {
                "Name": "sales@connector-1",
                "Type": "LAMBDA",
                "Parameters": {
                  "function": "arn:aws:lambda:eu-west-1:000000000000:function:sales-connector"
                }
              }
            }
          },
          "Outputs": {
            "WorkGroupRef": {"Value": {"Ref": "Federated"}},
            "CatalogRef": {"Value": {"Ref": "Connector"}}
          }
        }"#,
        declared_work_group: Some("sales.workgroup_1"),
        declared_catalog: Some("sales@connector-1"),
        expected_state: ENABLED,
        expected_description: None,
        expected_configuration: &[
            ("/EnforceWorkGroupConfiguration", "true"),
            ("/BytesScannedCutoffPerQuery", "10000000"),
            (
                "/ResultConfiguration/OutputLocation",
                "\"s3://sales-results/out/\"",
            ),
        ],
        expected_catalog_type: "LAMBDA",
        // A composite function is the wiring of both roles, beside the name of the catalog
        // it is asked about — which is what the catalog holds, whichever path registered it.
        expected_parameters: &[
            ("catalog", "sales@connector-1"),
            (
                "metadata-function",
                "arn:aws:lambda:eu-west-1:000000000000:function:sales-connector",
            ),
            (
                "record-function",
                "arn:aws:lambda:eu-west-1:000000000000:function:sales-connector",
            ),
        ],
        expected_tags: &[],
    },
    Row {
        case: "a FEDERATED catalog, whose creation Athena completes asynchronously, beside a \
               workgroup carrying only its output location",
        region: "us-east-2",
        stack: "athena-federated-stack",
        template: r#"{
          "Resources": {
            "Lake": {
              "Type": "AWS::Athena::WorkGroup",
              "Properties": {
                "Name": "lake-workgroup",
                "Tags": [
                  {"Key": "Owner", "Value": "lake"},
                  {"Key": "Tier", "Value": "prod"}
                ],
                "WorkGroupConfiguration": {
                  "ResultConfiguration": {"OutputLocation": "s3://lake-results/out/"}
                }
              }
            },
            "Redshift": {
              "Type": "AWS::Athena::DataCatalog",
              "Properties": {
                "Name": "lake_redshift",
                "Type": "FEDERATED",
                "Parameters": {"connection-type": "REDSHIFT"}
              }
            }
          },
          "Outputs": {
            "WorkGroupRef": {"Value": {"Ref": "Lake"}},
            "CatalogRef": {"Value": {"Ref": "Redshift"}}
          }
        }"#,
        declared_work_group: Some("lake-workgroup"),
        declared_catalog: Some("lake_redshift"),
        expected_state: ENABLED,
        expected_description: None,
        expected_configuration: &[(
            "/ResultConfiguration/OutputLocation",
            "\"s3://lake-results/out/\"",
        )],
        expected_catalog_type: "FEDERATED",
        expected_parameters: &[("connection-type", "REDSHIFT")],
        expected_tags: &[("Owner", "lake"), ("Tier", "prod")],
    },
];

/// Every row: the names the stack publishes for its Athena resources address a workgroup and a
/// data catalog Athena really holds, configured as the template declared them, and deleting the
/// stack releases both.
#[tokio::test]
async fn a_stack_provisions_the_athena_resources_its_template_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack = format!("{}-{index}", row.stack);

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", row.template),
                ("Tags.member.1.Key", "Stack"),
                ("Tags.member.1.Value", "athena-suite"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = wait_for_stack_status(&router, region, &stack, "CREATE_COMPLETE").await;
        let outputs = outputs_of(&described);
        let work_group = output(&outputs, "WorkGroupRef", case);
        let catalog = output(&outputs, "CatalogRef", case);
        if let Some(declared) = row.declared_work_group {
            assert_eq!(
                work_group, declared,
                "case {case:?}: expected the workgroup to carry the declared name {declared:?}, \
                 got {work_group:?}"
            );
        }
        if let Some(declared) = row.declared_catalog {
            assert_eq!(
                catalog, declared,
                "case {case:?}: expected the catalog to carry the declared name {declared:?}, got \
                 {catalog:?}"
            );
        }
        // A name CloudFormation generated is still a name the owning service's own rules admit:
        // `WorkGroupName`'s pattern for the workgroup, and the documented character set and
        // length of a catalog name for the catalog.
        assert!(
            !work_group.is_empty()
                && work_group.len() <= MAX_WORK_GROUP_NAME
                && work_group.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || character == '.'
                        || character == '_'
                        || character == '-'
                }),
            "case {case:?}: expected a name inside Athena's own WorkGroupName rule, got \
             {work_group:?}"
        );
        assert!(
            !catalog.is_empty()
                && catalog.len() <= MAX_CATALOG_NAME
                && catalog.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || character == '_'
                        || character == '@'
                        || character == '-'
                }),
            "case {case:?}: expected a name inside Athena's own catalog-name rule, got {catalog:?}"
        );

        // Athena itself resolves the published name to the workgroup the template declared.
        let (status, answered) = athena(
            &router,
            region,
            "GetWorkGroup",
            &json!({ "WorkGroup": work_group }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetWorkGroup on the name the stack published, \
             got {status} with body {answered}"
        );
        let reported = answered.get("WorkGroup").cloned().unwrap_or(Value::Null);
        assert_eq!(
            reported.get("Name").and_then(Value::as_str),
            Some(work_group.as_str()),
            "case {case:?}: expected the workgroup to report the name {work_group:?}, got \
             {reported}"
        );
        assert_eq!(
            reported.get("State").and_then(Value::as_str),
            Some(row.expected_state),
            "case {case:?}: expected the declared state {:?}, got {reported}",
            row.expected_state
        );
        assert_eq!(
            reported.get("Description").and_then(Value::as_str),
            row.expected_description,
            "case {case:?}: expected the declared description, got {reported}"
        );
        assert!(
            reported.get("CreationTime").is_some(),
            "case {case:?}: expected the workgroup to report a CreationTime, got {reported}"
        );
        let configuration = reported.get("Configuration").cloned().unwrap_or(Value::Null);
        for (path, expected) in row.expected_configuration {
            let wanted: Value =
                serde_json::from_str(expected).expect("the expected value is JSON");
            assert_eq!(
                configuration.pointer(path),
                Some(&wanted),
                "case {case:?}: expected the declared configuration at {path:?} to be {wanted}, \
                 got {configuration}"
            );
        }

        // And to the data catalog, with the type and the parameter set its declaration carried.
        let (status, answered) =
            athena(&router, region, "GetDataCatalog", &json!({ "Name": catalog })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetDataCatalog on the name the stack \
             published, got {status} with body {answered}"
        );
        let reported = answered.get("DataCatalog").cloned().unwrap_or(Value::Null);
        assert_eq!(
            reported.get("Name").and_then(Value::as_str),
            Some(catalog.as_str()),
            "case {case:?}: expected the catalog to report the name {catalog:?}, got {reported}"
        );
        assert_eq!(
            reported.get("Type").and_then(Value::as_str),
            Some(row.expected_catalog_type),
            "case {case:?}: expected the declared catalog type {:?}, got {reported}",
            row.expected_catalog_type
        );
        for (key, value) in row.expected_parameters {
            assert_eq!(
                reported.pointer(&format!("/Parameters/{key}")),
                Some(&Value::String((*value).to_string())),
                "case {case:?}: expected the declared parameter {key:?} to be {value:?}, got \
                 {reported}"
            );
        }
        // The stack is reported complete only once Athena reports the catalog's own creation
        // complete, which for the one type it creates asynchronously is a status of its own.
        assert_eq!(
            reported.get("Status").and_then(Value::as_str),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the catalog's creation to be complete, got {reported}"
        );

        // The tags the declaration asked for are on the resource, beside the stack's own.
        // CloudFormation's three `aws:cloudformation:*` tags are NOT: Athena refuses every key in
        // that namespace on the public calls that take a tag list, so no provisioning call sends
        // one.
        let work_group_arn = athena_arn(region, "workgroup", &work_group);
        let tags = tags_on(&router, region, &work_group_arn, case).await;
        for (key, value) in row.expected_tags {
            assert_eq!(
                tags.get(*key).map(String::as_str),
                Some(*value),
                "case {case:?}: expected tag {key:?} on the workgroup to be {value:?}, got \
                 {tags:?}"
            );
        }
        assert_eq!(
            tags.get("Stack").map(String::as_str),
            Some("athena-suite"),
            "case {case:?}: expected the stack's own propagated tag on the workgroup, got {tags:?}"
        );
        assert!(
            tags.keys().all(|key| !key.starts_with("aws:")),
            "case {case:?}: expected no reserved tag key on the workgroup, got {tags:?}"
        );

        // Deleting the stack releases what Athena holds for it: the next read of either name is
        // refused by the service that no longer holds the resource.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        for (operation, request) in [
            ("GetWorkGroup", json!({ "WorkGroup": work_group })),
            ("GetDataCatalog", json!({ "Name": catalog })),
        ] {
            let (status, gone) = athena(&router, region, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from {operation} once the stack deleted the \
                 resource, got {status} with body {gone}"
            );
            assert_eq!(
                gone.get("__type").and_then(Value::as_str),
                Some("InvalidRequestException"),
                "case {case:?}: expected {operation} to answer InvalidRequestException for a \
                 resource Athena no longer holds, got {gone}"
            );
        }
    }
}

/// A declaration Athena REFUSES does not report a resource nothing created: the stack fails with
/// that service's own refusal, and the name it declared resolves to nothing afterwards.
///
/// The rows differ along WHICH call refuses and why: a workgroup name the account already holds
/// (refused by `CreateWorkGroup`), a `GLUE` catalog whose parameter set does not carry the key
/// its type requires, and a `LAMBDA` catalog carrying both of the mutually exclusive parameter
/// sets. The first row is also what says the rollback takes away only what the stack CREATED:
/// the workgroup the client made under that name is still there afterwards.
#[tokio::test]
async fn a_declaration_athena_refuses_fails_the_stack() {
    let router = build_router();
    let region = "us-east-2";

    // The workgroup a client already holds, which the first row's template re-declares.
    let (status, created) = athena(
        &router,
        region,
        "CreateWorkGroup",
        &json!({ "Name": "already-there" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the client's own CreateWorkGroup to succeed, got {status} with body {created}"
    );

    // (case, the template, the reader of the declared name, that name, the status the reader
    // must answer once the stack has rolled back). The first row's name is the CLIENT's own
    // workgroup, which a stack that never created it must leave exactly where it is — so its
    // read still succeeds, while the names the other two rows declared resolve to nothing.
    let rows: &[(&str, &str, &str, &str, u16)] = &[
        (
            "a workgroup name the account already holds",
            r#"{"Resources": {"Clash": {"Type": "AWS::Athena::WorkGroup",
               "Properties": {"Name": "already-there"}}}}"#,
            "GetWorkGroup",
            "already-there",
            200,
        ),
        (
            "a GLUE catalog whose parameters carry no catalog id",
            r#"{"Resources": {"Catalog": {"Type": "AWS::Athena::DataCatalog",
               "Properties": {"Name": "glue_without_id", "Type": "GLUE",
               "Parameters": {"metadata-function": "arn:aws:lambda:us-east-2:000000000000:function:m"}}}}}"#,
            "GetDataCatalog",
            "glue_without_id",
            400,
        ),
        (
            "a LAMBDA catalog carrying both of the mutually exclusive parameter sets",
            r#"{"Resources": {"Catalog": {"Type": "AWS::Athena::DataCatalog",
               "Properties": {"Name": "lambda_both_sets", "Type": "LAMBDA",
               "Parameters": {"function": "arn:aws:lambda:us-east-2:000000000000:function:f",
               "metadata-function": "arn:aws:lambda:us-east-2:000000000000:function:m",
               "record-function": "arn:aws:lambda:us-east-2:000000000000:function:r"}}}}}"#,
            "GetDataCatalog",
            "lambda_both_sets",
            400,
        ),
    ];

    for (index, (case, template, reader, name, expected_status)) in rows.iter().enumerate() {
        let stack = format!("athena-refusal-{index}");
        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (reported, described) = wait_for_terminal_status(&router, region, &stack).await;
        assert_eq!(
            reported, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the stack to roll back on a declaration Athena refuses, got \
             {reported:?} with answer {described}"
        );

        // Nothing was created under the name the refused declaration asked for — and nothing
        // the stack did not create was taken away either.
        let request = if *reader == "GetWorkGroup" {
            json!({ "WorkGroup": name })
        } else {
            json!({ "Name": name })
        };
        let (status, answered) = athena(&router, region, reader, &request).await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status} from {reader} on the name the failed \
             stack declared, got {status} with body {answered}"
        );
    }
}

/// A stack that CHANGES its workgroup's declaration applies the change through Athena's own
/// update, so the workgroup keeps its name and the configuration a later read reports is the
/// declared one — including a member the new declaration DROPPED, which the update's own
/// `Remove…` flag takes off the workgroup.
#[tokio::test]
async fn a_changed_declaration_is_applied_to_the_workgroup_athena_holds() {
    let router = build_router();
    let region = "ap-southeast-1";
    let stack = "athena-update-stack";

    let before = r#"{
      "Resources": {
        "Analytics": {
          "Type": "AWS::Athena::WorkGroup",
          "Properties": {
            "Name": "updated-workgroup",
            "WorkGroupConfiguration": {
              "EnforceWorkGroupConfiguration": true,
              "BytesScannedCutoffPerQuery": 20000000,
              "ResultConfiguration": {
                "OutputLocation": "s3://before/out/",
                "ExpectedBucketOwner": "000000000000"
              }
            }
          }
        }
      }
    }"#;
    let after = r#"{
      "Resources": {
        "Analytics": {
          "Type": "AWS::Athena::WorkGroup",
          "Properties": {
            "Name": "updated-workgroup",
            "State": "DISABLED",
            "WorkGroupConfiguration": {
              "EnforceWorkGroupConfiguration": false,
              "ResultConfiguration": {"OutputLocation": "s3://after/out/"}
            }
          }
        }
      }
    }"#;

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", before)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    wait_for_stack_status(&router, region, stack, "CREATE_COMPLETE").await;

    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", after)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    wait_for_stack_status(&router, region, stack, "UPDATE_COMPLETE").await;

    let (status, answered) = athena(
        &router,
        region,
        "GetWorkGroup",
        &json!({ "WorkGroup": "updated-workgroup" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetWorkGroup after the update, got {status} with body {answered}"
    );
    let reported = answered.get("WorkGroup").cloned().unwrap_or(Value::Null);
    let cases: &[(&str, &str, Option<Value>)] = &[
        ("the state the update declared", "/State", Some(json!("DISABLED"))),
        (
            "the enforcement switch the update turned off",
            "/Configuration/EnforceWorkGroupConfiguration",
            Some(json!(false)),
        ),
        (
            "the output location the update moved",
            "/Configuration/ResultConfiguration/OutputLocation",
            Some(json!("s3://after/out/")),
        ),
        (
            "the data-usage limit the update dropped",
            "/Configuration/BytesScannedCutoffPerQuery",
            None,
        ),
        (
            "the expected bucket owner the update dropped",
            "/Configuration/ResultConfiguration/ExpectedBucketOwner",
            None,
        ),
    ];
    for (case, path, expected) in cases {
        let actual = reported.pointer(path).cloned();
        assert_eq!(
            actual.as_ref(),
            expected.as_ref(),
            "case {case:?}: expected {expected:?} at {path:?}, got {actual:?} in {reported}"
        );
    }
}
