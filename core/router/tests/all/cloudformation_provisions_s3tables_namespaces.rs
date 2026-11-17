// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::S3Tables::Namespace` really creates that
//! namespace in Amazon S3 Tables, inside the table bucket its declaration names.
//!
//! The rule under test: **a resource CloudFormation reports created is created in the service
//! that OWNS it, through that service's own create call — for a CHILD resource, on the parent
//! its declaration names — so every read of the owning service resolves it, and the stack's
//! own deletion removes it there.** A namespace is the child case: Amazon S3 Tables keys one
//! by the table bucket holding it and its own name together, so the pair the declaration
//! states is what `CreateNamespace` is called with and what `ListNamespaces`, `GetNamespace`
//! and `CreateTable` then answer for. Two crates have to agree for that to hold
//! (CloudFormation makes the call, S3 Tables serves it), so the suite lives here rather than
//! in either one.
//!
//! The rows differ along what the rule generalises over: how many namespaces one stack
//! declares, which intrinsic the declaration names the parent bucket with (`Fn::GetAtt`,
//! `Ref`, `Fn::Sub`), whether the namespace's name is a literal or a template PARAMETER, the
//! name's length at both bounds the owning model publishes for one, the characters it is
//! written with, and the Region deployed in.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The XML element readers this crate writes ONCE (`tests/all/query_wire.rs`).
use super::query_wire::{opt_element, outputs_of, physical_ids_of};
// The four-service router, the two wire callers, the handle writer and the table bucket ARN
// form the sibling provisioning suite for these types already writes: all three S3 Tables
// suites drive the SAME emulator through the same calls and compute a handle the same way.
use super::cloudformation_provisions_s3tables_table_buckets::{
    build_router, cloudformation, label, s3tables, table_bucket_arn,
};

/// How long a poll waits for a stack to reach a terminal status, and how long it waits for
/// the resources a deleted stack held to be gone from their owning service. The create's
/// clock-owed span falls due one transition delay after the call that opened it; both bounds
/// are generous enough that a loaded machine never reads a state that is merely not due yet.
const TERMINAL_WAIT: Duration = Duration::from_secs(20);

/// How often those polls ask, which is how a client watches a stack.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The statuses a `CreateStack` can settle at, as `StackStatus` spells them.
const TERMINAL_STATUSES: &[&str] = &[
    "CREATE_COMPLETE",
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];

/// The logical id every template here gives its table bucket, which the namespaces of the
/// same stack name as their parent.
const BUCKET: &str = "Bucket";

/// The output key the bucket's own ARN is published under.
const BUCKET_OUTPUT: &str = "BucketArn";

/// The logical id the refusal table below gives its one namespace, which the stack's failure
/// has to name.
const REFUSED_NAMESPACE: &str = "DeclaredNamespace";

/// The separator CloudFormation joins the parts of a COMPOSITE physical resource id with: a
/// namespace is identified by its table bucket and its own name together, exactly as the
/// other composite-identifier types of this emulator are (an `AWS::EKS::AccessEntry` is
/// `<ClusterName>|<PrincipalArn>`).
const COMPOSITE_SEPARATOR: &str = "|";

/// The table this suite creates IN each declared namespace, and the one open table format
/// `CreateTable.format` admits (`OpenTableFormat`, whose only value is `ICEBERG`). The name
/// is inside the form `TableName` publishes (pattern `^[0-9a-z_]*$`).
const TABLE: &str = "events";
const TABLE_FORMAT: &str = "ICEBERG";

/// The bounds `NamespaceName` publishes (`smithy_query(query="shape",
/// shape="NamespaceName")`: `@length(min: 1, max: 255)`, pattern `^[0-9a-z_]*$`), written
/// here so the rows below and the assertions cannot disagree about them.
const NAMESPACE_NAME_MIN: usize = 1;
const NAMESPACE_NAME_MAX: usize = 255;

/// A namespace name of exactly `length` characters, inside the published character set.
fn namespace_name_of(length: usize) -> String {
    "n".repeat(length)
}

/// How a namespace's declaration names the table bucket it lives in: the three intrinsics a
/// template writes the parent's ARN with. `Ref` of an `AWS::S3Tables::TableBucket` returns
/// that ARN, and `TableBucketARN` is the one attribute the type publishes, so all three
/// resolve to the same handle.
#[derive(Clone, Copy, Debug)]
enum Parent {
    GetAtt,
    Ref,
    Sub,
}

impl Parent {
    /// The property value one of them is written as.
    fn declaration(self) -> Value {
        match self {
            Self::GetAtt => json!({ "Fn::GetAtt": [BUCKET, "TableBucketARN"] }),
            Self::Ref => json!({ "Ref": BUCKET }),
            Self::Sub => json!({ "Fn::Sub": format!("${{{BUCKET}.TableBucketARN}}") }),
        }
    }
}

/// One row of the table: a stack, and what the rule says Amazon S3 Tables must then hold.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the bucket and its namespaces live
    /// in and the one the bucket's ARN carries.
    region: &'static str,
    /// The `TableBucketName` the template declares for the parent bucket.
    bucket: &'static str,
    /// The namespaces the stack declares, as `(logical id, namespace name)` in template
    /// order.
    namespaces: fn() -> Vec<(&'static str, String)>,
    /// Which intrinsic the namespaces name their parent bucket with.
    parent: Parent,
    /// The template PARAMETER the namespace name is stated as a `Ref` to, for the row that
    /// deploys it the way a parameterised estate template does; `None` for a row that states
    /// the name literally.
    parameter: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "one namespace naming its bucket with Fn::GetAtt",
        region: "us-east-1",
        bucket: "ns-stack-getatt",
        namespaces: || vec![("Analytics", "analytics".to_string())],
        parent: Parent::GetAtt,
        parameter: None,
    },
    Row {
        case: "two namespaces in one stack, naming their bucket with Ref",
        region: "us-west-2",
        bucket: "ns-stack-ref",
        namespaces: || {
            vec![
                ("Raw", "raw_events".to_string()),
                ("Curated", "curated0".to_string()),
            ]
        },
        parent: Parent::Ref,
        parameter: None,
    },
    Row {
        case: "a namespace whose name comes from a template parameter, naming its bucket \
               with Fn::Sub",
        region: "eu-west-1",
        bucket: "ns-stack-sub",
        namespaces: || vec![("Warehouse", "warehouse_2026".to_string())],
        parent: Parent::Sub,
        parameter: Some("NamespaceName"),
    },
    Row {
        case: "namespace names at the shortest and longest the published form admits",
        region: "ap-southeast-2",
        bucket: "ns-stack-bounds",
        namespaces: || {
            vec![
                ("Shortest", namespace_name_of(NAMESPACE_NAME_MIN)),
                ("Longest", namespace_name_of(NAMESPACE_NAME_MAX)),
            ]
        },
        parent: Parent::GetAtt,
        parameter: None,
    },
];

/// The template one row deploys: the table bucket, one `AWS::S3Tables::Namespace` per
/// declared namespace, and an output per resource so the handles are readable both ways.
fn template(row: &Row, namespaces: &[(&'static str, String)]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        BUCKET.to_string(),
        json!({
            "Type": "AWS::S3Tables::TableBucket",
            "Properties": { "TableBucketName": row.bucket },
        }),
    );
    let mut outputs = serde_json::Map::new();
    outputs.insert(
        BUCKET_OUTPUT.to_string(),
        json!({ "Value": { "Fn::GetAtt": [BUCKET, "TableBucketARN"] } }),
    );
    for (logical_id, name) in namespaces {
        let declared_name = match row.parameter {
            Some(parameter) => json!({ "Ref": parameter }),
            None => Value::String(name.clone()),
        };
        resources.insert(
            (*logical_id).to_string(),
            json!({
                "Type": "AWS::S3Tables::Namespace",
                "Properties": {
                    "TableBucketARN": row.parent.declaration(),
                    "Namespace": declared_name,
                },
            }),
        );
        outputs.insert(
            (*logical_id).to_string(),
            json!({ "Value": { "Ref": *logical_id } }),
        );
    }
    let mut body = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an S3 Tables estate: a table bucket and the namespaces in it",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    });
    if let Some(parameter) = row.parameter {
        body.as_object_mut().expect("a template object").insert(
            "Parameters".to_string(),
            json!({ parameter: { "Type": "String" } }),
        );
    }
    body.to_string()
}

/// Poll `DescribeStacks` until the stack SETTLES, and answer the terminal status it settled
/// at beside the answer it was read from — which is how a client waits for a stack.
async fn settled_stack(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> (String, String) {
    let deadline = Instant::now() + TERMINAL_WAIT;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks on {stack_name:?}, got \
             {status} with body {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if TERMINAL_STATUSES.contains(&reached.as_str()) {
            return (reached, described);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack_name:?} to settle at a terminal status within \
             {TERMINAL_WAIT:?}, it is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The physical id each logical resource of a settled stack reports.
async fn reported_ids(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> BTreeMap<String, String> {
    let (status, resources) = cloudformation(
        router,
        region,
        "DescribeStackResources",
        &[("StackName", stack_name)],
    )
    .await;

    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {resources}"
    );
    physical_ids_of(&resources)
}

/// `ListNamespaces` on a table bucket's ARN — the read a client waiting for a deployed
/// namespace makes (`GET /namespaces/{tableBucketARN}`).
async fn list_namespaces(
    router: &MultiServiceRouter,
    region: &str,
    table_bucket_arn: &str,
) -> (u16, Value) {
    s3tables(
        router,
        region,
        "GET",
        &format!("/namespaces/{}", label(table_bucket_arn)),
        None,
    )
    .await
}

/// `GetNamespace` on the pair S3 Tables keys one by
/// (`GET /namespaces/{tableBucketARN}/{namespace}`).
async fn get_namespace(
    router: &MultiServiceRouter,
    region: &str,
    table_bucket_arn: &str,
    namespace: &str,
) -> (u16, Value) {
    s3tables(
        router,
        region,
        "GET",
        &format!(
            "/namespaces/{}/{}",
            label(table_bucket_arn),
            label(namespace)
        ),
        None,
    )
    .await
}

/// The namespace names one `ListNamespaces` answer reports, each read off its summary's
/// `namespace` list, sorted so a row compares against the set it declared.
fn listed_namespaces(answered: &Value) -> Vec<String> {
    let mut listed: Vec<String> = answered
        .get("namespaces")
        .and_then(Value::as_array)
        .map(|summaries| {
            summaries
                .iter()
                .filter_map(|summary| {
                    summary
                        .get("namespace")
                        .and_then(Value::as_array)
                        .and_then(|names| names.first())
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    listed.sort();
    listed
}

/// The single name a `namespace` member carries, which is how both the create and the reads
/// answer one (`NamespaceList`, `@length(min: 1, max: 1)` on the create).
fn answered_namespace(answered: &Value) -> Option<String> {
    answered
        .get("namespace")
        .and_then(Value::as_array)
        .and_then(|names| match names.as_slice() {
            [only] => only.as_str().map(str::to_string),
            _ => None,
        })
}

/// Every row: the namespaces a stack declares exist in Amazon S3 Tables under the bucket the
/// declaration named — listed, readable, and usable as the namespace of a `CreateTable` —
/// and deleting the stack removes them there.
#[tokio::test]
async fn a_declared_namespace_is_created_in_s3tables_under_its_declared_bucket() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let namespaces = (row.namespaces)();
        let stack_name = format!("s3tables-namespaces-{index}");
        let body = template(row, &namespaces);
        let mut members: Vec<(&str, &str)> = vec![
            ("StackName", stack_name.as_str()),
            ("TemplateBody", body.as_str()),
        ];
        if let Some(parameter) = row.parameter {
            members.push(("Parameters.member.1.ParameterKey", parameter));
            members.push((
                "Parameters.member.1.ParameterValue",
                namespaces[0].1.as_str(),
            ));
        }
        let (status, created) = cloudformation(&router, region, "CreateStack", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );

        let (reached, described) = settled_stack(&router, case, region, &stack_name).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {reached:?} in \
             {described}"
        );

        let arn = table_bucket_arn(region, row.bucket);
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get(BUCKET_OUTPUT),
            Some(&arn),
            "case {case:?}: expected output {BUCKET_OUTPUT:?} to be the table bucket ARN \
             {arn:?}, got {:?}",
            outputs.get(BUCKET_OUTPUT)
        );

        // The stack reports each namespace under the pair that identifies one, and the same
        // value through its `Ref`.
        let reported = reported_ids(&router, case, region, &stack_name).await;
        for (logical_id, name) in &namespaces {
            let identity = format!("{arn}{COMPOSITE_SEPARATOR}{name}");
            assert_eq!(
                reported.get(*logical_id),
                Some(&identity),
                "case {case:?}: expected the physical id of {logical_id:?} to be {identity:?}, \
                 got {:?}",
                reported.get(*logical_id)
            );
            assert_eq!(
                outputs.get(*logical_id),
                Some(&identity),
                "case {case:?}: expected output {logical_id:?} to be {identity:?}, got {:?}",
                outputs.get(*logical_id)
            );
        }

        // And Amazon S3 Tables itself holds exactly those namespaces under that bucket.
        let (status, listed) = list_namespaces(&router, region, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListNamespaces on the ARN the stack \
             published, got {status} with body {listed}"
        );
        let mut declared: Vec<String> =
            namespaces.iter().map(|(_, name)| name.clone()).collect();
        declared.sort();
        let found = listed_namespaces(&listed);
        assert_eq!(
            found, declared,
            "case {case:?}: expected ListNamespaces to report the declared namespaces \
             {declared:?}, got {found:?} in {listed}"
        );

        for (_, name) in &namespaces {
            let (status, answered) = get_namespace(&router, region, &arn, name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetNamespace on {name:?}, got {status} \
                 with body {answered}"
            );
            assert_eq!(
                answered_namespace(&answered).as_deref(),
                Some(name.as_str()),
                "case {case:?}: expected GetNamespace to report {name:?}, got {answered}"
            );

            // The read a client makes of a deployed namespace is a table created IN it: the
            // namespace the stack declared is a namespace `CreateTable` resolves.
            let (status, table) = s3tables(
                &router,
                region,
                "PUT",
                &format!("/tables/{}/{}", label(&arn), label(name)),
                Some(&json!({ "name": TABLE, "format": TABLE_FORMAT })),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from CreateTable in the deployed namespace \
                 {name:?}, got {status} with body {table}"
            );
            let table_prefix = format!("{arn}/table/");
            let table_arn = table
                .get("tableARN")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                table_arn.starts_with(&table_prefix),
                "case {case:?}: expected a table ARN under {table_prefix:?}, got \
                 {table_arn:?}"
            );

            let (status, read) = s3tables(
                &router,
                region,
                "GET",
                &format!(
                    "/get-table?tableBucketARN={}&namespace={}&name={}",
                    label(&arn),
                    label(name),
                    label(TABLE)
                ),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetTable on the table created in \
                 {name:?}, got {status} with body {read}"
            );
            assert_eq!(
                answered_namespace(&read).as_deref(),
                Some(name.as_str()),
                "case {case:?}: expected GetTable to report the namespace {name:?}, got {read}"
            );
            assert_eq!(
                read.get("name").and_then(Value::as_str),
                Some(TABLE),
                "case {case:?}: expected GetTable to report the table name {TABLE:?}, got \
                 {read}"
            );

            // The table is the caller's, not the stack's: it is removed by the caller, so
            // what the stack's own deletion removes is the namespace it created.
            let (status, dropped) = s3tables(
                &router,
                region,
                "DELETE",
                &format!(
                    "/tables/{}/{}/{}",
                    label(&arn),
                    label(name),
                    label(TABLE)
                ),
                None,
            )
            .await;
            assert_eq!(
                status, 204,
                "case {case:?}: expected HTTP 204 from DeleteTable in {name:?}, got {status} \
                 with body {dropped}"
            );
        }

        // Deleting the stack removes what it created in that service, namespaces included.
        let (status, deleted) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)])
                .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body \
             {deleted}"
        );
        for (_, name) in &namespaces {
            let deadline = Instant::now() + TERMINAL_WAIT;
            loop {
                let (status, answered) = get_namespace(&router, region, &arn, name).await;
                if status == 404 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the namespace {name:?} to be gone from S3 Tables \
                     within {TERMINAL_WAIT:?} of the stack's deletion, GetNamespace answers \
                     {status} with body {answered}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

/// A stack whose template declares a namespace Amazon S3 Tables REFUSES does not report a
/// resource nothing created: the stack fails naming that resource, and no namespace of that
/// name exists in the bucket.
///
/// The rows are the ways a declared name leaves the form that service publishes for one
/// (`NamespaceName`: pattern `^[0-9a-z_]*$`, 1 to 255 characters): upper case, a character
/// the set excludes, and one character past the published maximum.
#[tokio::test]
async fn a_namespace_the_owning_service_refuses_fails_the_stack() {
    let router = build_router();
    let region = "us-east-2";
    let past_longest = namespace_name_of(NAMESPACE_NAME_MAX + 1);

    for (index, (case, declared)) in [
        ("an upper-case namespace name", "Analytics".to_string()),
        (
            "a namespace name carrying a character the published set excludes",
            "raw-events".to_string(),
        ),
        (
            "a namespace name one character past the published maximum",
            past_longest,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let bucket = format!("ns-refused-{index}");
        let body = json!({
            "Resources": {
                BUCKET: {
                    "Type": "AWS::S3Tables::TableBucket",
                    "Properties": { "TableBucketName": bucket },
                },
                REFUSED_NAMESPACE: {
                    "Type": "AWS::S3Tables::Namespace",
                    "Properties": {
                        "TableBucketARN": { "Fn::GetAtt": [BUCKET, "TableBucketARN"] },
                        "Namespace": declared,
                    },
                },
            }
        })
        .to_string();
        let stack_name = format!("s3tables-namespace-refused-{index}");
        let (status, created) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", stack_name.as_str()),
                ("TemplateBody", body.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — the template declares a type \
             CloudFormation publishes — got {status} with body {created}"
        );

        let (reached, described) = settled_stack(&router, case, region, &stack_name).await;
        assert!(
            reached.starts_with("CREATE_FAILED") || reached.starts_with("ROLLBACK"),
            "case {case:?}: expected the stack not to report a created namespace, got \
             {reached:?} in {described}"
        );
        let reason = opt_element(&described, "StackStatusReason").unwrap_or_default();
        assert!(
            reason.contains(REFUSED_NAMESPACE),
            "case {case:?}: expected the failure to name the resource, got {reason:?}"
        );

        // And nothing exists under the pair that declaration named.
        let arn = table_bucket_arn(region, &bucket);
        let (status, answered) = get_namespace(&router, region, &arn, &declared).await;
        assert_ne!(
            status, 200,
            "case {case:?}: expected no namespace named {declared:?}, GetNamespace answered \
             {status} with body {answered}"
        );
    }
}
