// SPDX-License-Identifier: CC0-1.0

//! What a client reads back from Amazon S3 Tables and Amazon S3 Vectors through the handle a
//! CloudFormation stack published for a bucket it deployed there — when the template declared
//! no encryption at all, and when the name it declared is one the owning service will not
//! create a bucket under.
//!
//! Two rules, one per half of the suite:
//!
//! 1. **A resource a stack deploys is created in the owning service with exactly the
//!    configuration the template declared, and a configuration the template OMITS is the
//!    owning service's own documented default — so a read addressed by the handle the stack
//!    published answers that default rather than nothing or a refusal.** Both ARN-keyed bucket
//!    variants document the same default: "all new vectors in Amazon S3 vector buckets use
//!    server-side encryption with Amazon S3 managed keys (SSE-S3), specifically `AES256`"
//!    (`EncryptionConfiguration$sseType`, S3 Vectors' model), and a table bucket is always
//!    encrypted, with `AES256` where nothing named an algorithm and the named algorithm and
//!    key otherwise (`EncryptionConfiguration`, S3 Tables' model). The reads driven here are
//!    the ones such a client makes with nothing but the handle: `GetTableBucketEncryption`
//!    and `ListNamespaces` on the table bucket, `GetVectorBucket` on the vector bucket.
//! 2. **A resource whose create the owning service REFUSES fails the stack, and a create
//!    whose default `OnFailure` is `ROLLBACK` ("Roll back the stack creation. This is the
//!    default", `CreateStackInput$OnFailure`) therefore settles at `ROLLBACK_COMPLETE`,
//!    reporting the owning service's own sentence as the resource's failure reason — and the
//!    owning service holds nothing under the name that was asked for.**
//!
//! The rows of the first half differ along what rule 1 generalises over: whether the
//! encryption configuration is omitted, declares S3-managed keys, or declares KMS-managed
//! keys and names the key; whether each bucket is NAMED by the template or left for
//! CloudFormation to generate a name; and the Region deployed in. The expectation of each row
//! is computed from what that row declares, never read out of the answer being checked.
//!
//! The rows of the second half span the ways a declared table bucket name leaves the form S3
//! Tables publishes for one (`CreateTableBucket.name` is a `TableBucketName`:
//! `@length(min: 3, max: 63)`, pattern `^[0-9a-z-]*$`, and that service holds a name to
//! beginning and ending with a letter or a digit): upper case, an underscore, a dot, one
//! character under the minimum, one character past the maximum, and a hyphen at either edge —
//! beside a name inside the form, which must still reach `CREATE_COMPLETE`, so the half cannot
//! pass by refusing everything.
//!
//! Three crates have to agree for either rule to hold (CloudFormation makes the creates, S3
//! Tables and S3 Vectors serve them), so the suite lives here rather than in any one of them.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The four-service router, the two wire callers, the handle writer and the two ARN forms the
// sibling provisioning suite for these types already writes: every suite here drives the SAME
// emulator through the same calls, and computes a handle the same way.
use super::cloudformation_provisions_s3tables_table_buckets::{
    build_router, cloudformation, label, s3tables, s3vectors, table_bucket_arn, vector_bucket_arn,
};
// The XML element readers this crate writes ONCE (`tests/all/query_wire.rs`).
use super::query_wire::{elements, members_of, opt_element, physical_ids_of};

/// How long a poll waits for a stack to settle. The transitions a create owes — its own
/// conclusion, and the rollback that conclusion makes due — each fall due a transition delay
/// after the call that opened them; this bound is generous enough that a loaded machine never
/// reads a status that is merely not due yet.
const SETTLE_WAIT: Duration = Duration::from_secs(30);

/// How often that poll asks, which is how a client watches a stack.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The statuses a `CreateStack` can SETTLE at, as `StackStatus` spells them: every other
/// status a create passes through is an `*_IN_PROGRESS` one.
const SETTLED_STATUSES: &[&str] = &[
    "CREATE_COMPLETE",
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];

/// The resource status an event reports a refused create under.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical ids every template here gives its two ARN-keyed buckets.
const TABLES: &str = "Tables";
const VECTORS: &str = "Vectors";

/// The two values the `SSEAlgorithm` enum of S3 Tables' `EncryptionConfiguration` declares,
/// which are also the two its S3 Vectors counterpart declares for `SseType`.
const S3_MANAGED: &str = "AES256";
const KMS_MANAGED: &str = "aws:kms";

/// A KMS key ARN in `region`, in the one form both services' `kmsKeyArn` members admit
/// (`^(arn:aws[-a-z0-9]*:kms:[-a-z0-9]*:[0-9]{12}:key/.+)$`), built on the account id this
/// emulator mints. No key is read through KMS here: what the rows assert is that the
/// configuration a template declared is the configuration the owning service then reports.
fn kms_key_arn(region: &str) -> String {
    format!("arn:aws:kms:{region}:000000000000:key/1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d")
}

/// The name a bucket ARN carries: the segment after its `bucket/`, which is how both ARN-keyed
/// services spell the resource part of one.
fn name_in(arn: &str) -> String {
    arn.rsplit_once("bucket/")
        .map(|(_, name)| name.to_string())
        .unwrap_or_default()
}

/// What a row's template declares for one bucket's encryption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encryption {
    /// The template declares no `EncryptionConfiguration` at all, so the bucket is protected
    /// by the owning service's documented default.
    Omitted,
    /// The template declares server-side encryption with S3-managed keys, which takes no key.
    S3Managed,
    /// The template declares server-side encryption with KMS-managed keys and names the key.
    KmsManaged,
}

impl Encryption {
    /// The `EncryptionConfiguration` an `AWS::S3Tables::TableBucket` declares for this
    /// choice — whose members that type spells `SSEAlgorithm` and `KMSKeyArn` — or no
    /// property at all.
    fn table_bucket_property(self, region: &str) -> Option<Value> {
        match self {
            Self::Omitted => None,
            Self::S3Managed => Some(json!({ "SSEAlgorithm": S3_MANAGED })),
            Self::KmsManaged => Some(json!({
                "SSEAlgorithm": KMS_MANAGED,
                "KMSKeyArn": kms_key_arn(region),
            })),
        }
    }

    /// The same for an `AWS::S3Vectors::VectorBucket`, whose members that type spells
    /// `SseType` and `KmsKeyArn`.
    fn vector_bucket_property(self, region: &str) -> Option<Value> {
        match self {
            Self::Omitted => None,
            Self::S3Managed => Some(json!({ "SseType": S3_MANAGED })),
            Self::KmsManaged => Some(json!({
                "SseType": KMS_MANAGED,
                "KmsKeyArn": kms_key_arn(region),
            })),
        }
    }

    /// The algorithm and the key the owning service must then report: what the template
    /// declared, and — for a template that declared nothing — the service's own default,
    /// which is S3-managed keys and no key at all.
    fn reported(self, region: &str) -> (&'static str, Option<String>) {
        match self {
            Self::Omitted | Self::S3Managed => (S3_MANAGED, None),
            Self::KmsManaged => (KMS_MANAGED, Some(kms_key_arn(region))),
        }
    }
}

/// One declared resource: its type, the property its name is declared under, and its
/// encryption configuration. A bucket the row does not name declares neither, which is how a
/// template leaves the name to CloudFormation.
fn declared(
    resource_type: &str,
    name_property: &str,
    name: Option<&str>,
    encryption: Option<Value>,
) -> Value {
    let mut properties = Map::new();
    if let Some(name) = name {
        properties.insert(name_property.to_string(), json!(name));
    }
    if let Some(configuration) = encryption {
        properties.insert("EncryptionConfiguration".to_string(), configuration);
    }
    if properties.is_empty() {
        return json!({ "Type": resource_type });
    }
    json!({ "Type": resource_type, "Properties": Value::Object(properties) })
}

/// One row of the first half: a Region, a name for each of the two ARN-keyed bucket variants,
/// and the encryption each one's declaration states.
struct Row {
    case: &'static str,
    region: &'static str,
    /// The `TableBucketName` the template declares, or `None` for a template that names
    /// nothing.
    tables_name: Option<&'static str>,
    /// The `VectorBucketName`, under the same rule.
    vectors_name: Option<&'static str>,
    tables_encryption: Encryption,
    vectors_encryption: Encryption,
}

impl Row {
    /// The template this row deploys: the two ARN-keyed bucket variants, each with an output
    /// carrying `Ref` of its logical id — the way a client reads a handle out of a stack
    /// without reading its resource rows.
    fn template(&self) -> String {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Description": "the two ARN-keyed S3 bucket variants",
            "Resources": {
                TABLES: declared(
                    "AWS::S3Tables::TableBucket",
                    "TableBucketName",
                    self.tables_name,
                    self.tables_encryption.table_bucket_property(self.region),
                ),
                VECTORS: declared(
                    "AWS::S3Vectors::VectorBucket",
                    "VectorBucketName",
                    self.vectors_name,
                    self.vectors_encryption.vector_bucket_property(self.region),
                ),
            },
            "Outputs": {
                TABLES: { "Value": { "Ref": TABLES } },
                VECTORS: { "Value": { "Ref": VECTORS } },
            }
        })
        .to_string()
    }
}

const ROWS: &[Row] = &[
    Row {
        // The recorded shape: both buckets named, neither declaring any encryption.
        case: "both buckets named, neither declaring encryption",
        region: "us-west-2",
        tables_name: Some("sim2real-defaults-t"),
        vectors_name: Some("sim2real-defaults-v"),
        tables_encryption: Encryption::Omitted,
        vectors_encryption: Encryption::Omitted,
    },
    Row {
        // The same omission where CloudFormation decides both names, in another Region: the
        // default is the service's, so it cannot depend on who chose the name.
        case: "buckets the template does not name, neither declaring encryption",
        region: "eu-central-1",
        tables_name: None,
        vectors_name: None,
        tables_encryption: Encryption::Omitted,
        vectors_encryption: Encryption::Omitted,
    },
    Row {
        // The declared counterpart of the omission: asking for S3-managed keys reports the
        // same configuration the omission defaults to.
        case: "both buckets declaring S3-managed keys",
        region: "us-east-1",
        tables_name: Some("sim2real-declared-t"),
        vectors_name: Some("sim2real-declared-v"),
        tables_encryption: Encryption::S3Managed,
        vectors_encryption: Encryption::S3Managed,
    },
    Row {
        // A declaration the default is NOT: a configuration naming KMS-managed keys and the
        // key, which both services must report instead of their default.
        case: "both buckets declaring KMS-managed keys",
        region: "ap-northeast-1",
        tables_name: Some("sim2real-kms-t"),
        vectors_name: Some("sim2real-kms-v"),
        tables_encryption: Encryption::KmsManaged,
        vectors_encryption: Encryption::KmsManaged,
    },
    Row {
        // The two halves mixed in one stack, so neither service's answer can be standing in
        // for the other's.
        case: "a table bucket declaring KMS-managed keys beside a vector bucket declaring none",
        region: "eu-west-1",
        tables_name: Some("sim2real-mixed-t"),
        vectors_name: None,
        tables_encryption: Encryption::KmsManaged,
        vectors_encryption: Encryption::Omitted,
    },
];

/// Poll `DescribeStacks` until the stack SETTLES, and answer the status it settled at beside
/// the answer it was read from — which is how a client waits for a stack.
async fn settled_stack(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> (String, String) {
    let deadline = Instant::now() + SETTLE_WAIT;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks on {stack_name:?}, got \
             {status} with body {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if SETTLED_STATUSES.contains(&reached.as_str()) {
            return (reached, described);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack_name:?} to settle within {SETTLE_WAIT:?}, it is \
             still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The physical resource id each logical id of a stack reports, as `DescribeStackResources`
/// answers them.
async fn reported_rows(
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

/// The row one logical id reports, or a panic quoting the rows that were read.
fn row_of<'a>(case: &str, rows: &'a BTreeMap<String, String>, logical_id: &str) -> &'a String {
    rows.get(logical_id).unwrap_or_else(|| {
        panic!("case {case:?}: expected a physical id for {logical_id:?}, got the rows {rows:?}")
    })
}

/// The `CREATE_FAILED` events a stack recorded for its RESOURCES, each as its logical id and
/// the reason it carries — the stack's own failure event (whose logical id is the stack) left
/// out, since it reports the deployment rather than a resource.
async fn refused_resources(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> Vec<(String, String)> {
    let (status, events) =
        cloudformation(router, region, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
         {events}"
    );
    members_of(&events)
        .into_iter()
        .filter(|event| {
            elements(event, "ResourceStatus")
                .first()
                .is_some_and(|status| status == CREATE_FAILED)
        })
        .map(|event| {
            (
                elements(event, "LogicalResourceId")
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                elements(event, "ResourceStatusReason")
                    .first()
                    .cloned()
                    .unwrap_or_default(),
            )
        })
        .filter(|(logical_id, _)| logical_id != stack_name)
        .collect()
}

/// The names of the table buckets Amazon S3 Tables holds in `region`, as `ListTableBuckets`
/// reports them.
async fn listed_table_buckets(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
) -> Vec<String> {
    let (status, listed) = s3tables(router, region, "GET", "/buckets", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTableBuckets, got {status} with body {listed}"
    );
    listed
        .get("tableBuckets")
        .and_then(Value::as_array)
        .map(|buckets| {
            buckets
                .iter()
                .filter_map(|bucket| bucket.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Rule 1: each row's buckets are created in their owning services with the encryption its
/// template declared — the service's own default where the template declared none — and both
/// are readable through the handle the stack published.
#[tokio::test]
async fn an_omitted_encryption_configuration_reports_each_services_documented_default() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("s3-bucket-encryption-{index}");

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &row.template()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (reached, described) = settled_stack(&router, case, region, &stack_name).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {reached:?} in \
             {described}"
        );
        let rows = reported_rows(&router, case, region, &stack_name).await;

        // The table bucket is published under its own ARN, whatever decided its name.
        let tables = row_of(case, &rows, TABLES).clone();
        let tables_name = row.tables_name.map_or_else(|| name_in(&tables), str::to_string);
        let tables_handle = table_bucket_arn(region, &tables_name);
        assert_eq!(
            tables, tables_handle,
            "case {case:?}: expected the table bucket's handle to be the ARN \
             {tables_handle:?}, got {tables:?}"
        );

        // ...and the encryption that service reports for it is the one the template declared,
        // or its own default where the template declared none.
        let (algorithm, key) = row.tables_encryption.reported(region);
        let (status, answered) = s3tables(
            &router,
            region,
            "GET",
            &format!("/buckets/{}/encryption", label(&tables_handle)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetTableBucketEncryption on the handle the \
             stack published, got {status} with body {answered}"
        );
        let configuration = answered
            .get("encryptionConfiguration")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            configuration.get("sseAlgorithm").and_then(Value::as_str),
            Some(algorithm),
            "case {case:?}: expected the table bucket's sseAlgorithm to be {algorithm:?}, got \
             {configuration}"
        );
        assert_eq!(
            configuration
                .get("kmsKeyArn")
                .and_then(Value::as_str)
                .map(str::to_string),
            key,
            "case {case:?}: expected the table bucket's kmsKeyArn to be {key:?}, got \
             {configuration}"
        );

        // The other read a client holding only that handle makes: the bucket's namespaces,
        // which is an empty list for a bucket the stack created nothing inside — never a
        // refusal of the handle.
        let (status, namespaces) = s3tables(
            &router,
            region,
            "GET",
            &format!("/namespaces/{}", label(&tables_handle)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListNamespaces on the handle the stack \
             published, got {status} with body {namespaces}"
        );
        let listed = namespaces
            .get("namespaces")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the table bucket to hold no namespaces, got {listed:?}"
        );

        // The vector bucket beside it: published under its own ARN too, and the encryption S3
        // Vectors reports in its record follows the same rule.
        let vectors = row_of(case, &rows, VECTORS).clone();
        let vectors_name = row
            .vectors_name
            .map_or_else(|| name_in(&vectors), str::to_string);
        let vectors_handle = vector_bucket_arn(region, &vectors_name);
        assert_eq!(
            vectors, vectors_handle,
            "case {case:?}: expected the vector bucket's handle to be the ARN \
             {vectors_handle:?}, got {vectors:?}"
        );
        let (algorithm, key) = row.vectors_encryption.reported(region);
        let (status, answered) = s3vectors(
            &router,
            region,
            "GetVectorBucket",
            &json!({ "vectorBucketArn": vectors_handle }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetVectorBucket on the handle the stack \
             published, got {status} with body {answered}"
        );
        let configuration = answered
            .get("vectorBucket")
            .and_then(|bucket| bucket.get("encryptionConfiguration"))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            configuration.get("sseType").and_then(Value::as_str),
            Some(algorithm),
            "case {case:?}: expected the vector bucket's sseType to be {algorithm:?}, got \
             {configuration}"
        );
        assert_eq!(
            configuration
                .get("kmsKeyArn")
                .and_then(Value::as_str)
                .map(str::to_string),
            key,
            "case {case:?}: expected the vector bucket's kmsKeyArn to be {key:?}, got \
             {configuration}"
        );
    }
}

/// The sentence Amazon S3 Tables states a table bucket name it will not accept with, as the
/// recorded refusal of one words it. CloudFormation reports a resource provider's answer
/// quoted inside an envelope of its own, so what a row holds the reason to is that it CARRIES
/// this sentence.
const REFUSED_NAME_SENTENCE: &str =
    "The specified bucket name isn't valid. Specify a different bucket name, and then try again.";

/// A name inside the form S3 Tables publishes, whose stack must still complete — so the half
/// below cannot pass by refusing every name.
const ACCEPTED_NAME: &str = "sim2real-refusals-ok";

/// The declared names the second half deploys: each case, the name, and whether S3 Tables
/// accepts it.
fn name_rows() -> Vec<(&'static str, String, bool)> {
    vec![
        // The recorded instance: upper case and an underscore, neither in the published
        // character set.
        (
            "an upper-case, underscored name",
            "Upper_Case_Tab".to_string(),
            false,
        ),
        ("an upper-case name", "SimToRealTables".to_string(), false),
        ("an underscored name", "under_scored_name".to_string(), false),
        ("a name carrying a dot", "dots.not.allowed".to_string(), false),
        (
            "a name one character under the published minimum",
            "ab".to_string(),
            false,
        ),
        (
            "a name one character past the published maximum",
            "a".repeat(64),
            false,
        ),
        (
            "a name that opens with a hyphen",
            "-opens-with-a-hyphen".to_string(),
            false,
        ),
        (
            "a name that closes with a hyphen",
            "closes-with-a-hyphen-".to_string(),
            false,
        ),
        // ...and the name inside the form, at the same length as the refused ones around it.
        ("a name inside the published form", ACCEPTED_NAME.to_string(), true),
    ]
}

/// Rule 2: a table bucket name the owning service refuses fails the resource and rolls the
/// whole create back to `ROLLBACK_COMPLETE`, reporting that service's own sentence — while a
/// name inside the published form completes and is held by that service.
#[tokio::test]
async fn a_table_bucket_name_s3_tables_refuses_rolls_the_stack_back() {
    let router = build_router();
    let region = "us-east-2";

    for (index, (case, declared_name, accepted)) in name_rows().into_iter().enumerate() {
        let stack_name = format!("s3tables-name-refusal-{index}");
        let template = json!({
            "Resources": {
                TABLES: {
                    "Type": "AWS::S3Tables::TableBucket",
                    "Properties": { "TableBucketName": declared_name }
                }
            }
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — the template declares a type \
             CloudFormation publishes — got {status} with body {body}"
        );

        let (reached, described) = settled_stack(&router, case, region, &stack_name).await;
        let refused = refused_resources(&router, case, region, &stack_name).await;
        let listed = listed_table_buckets(&router, case, region).await;

        if accepted {
            assert_eq!(
                reached, "CREATE_COMPLETE",
                "case {case:?}: expected a name inside the published form to complete, got \
                 {reached:?} in {described}"
            );
            assert!(
                refused.is_empty(),
                "case {case:?}: expected no refused resource, got {refused:?}"
            );
            assert!(
                listed.contains(&declared_name),
                "case {case:?}: expected S3 Tables to hold {declared_name:?}, it lists {listed:?}"
            );
            continue;
        }

        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the refused create to roll back to ROLLBACK_COMPLETE, got \
             {reached:?} in {described}"
        );
        let reasons: Vec<&String> = refused
            .iter()
            .filter(|(logical_id, _)| logical_id == TABLES)
            .map(|(_, reason)| reason)
            .collect();
        assert!(
            !reasons.is_empty(),
            "case {case:?}: expected a CREATE_FAILED event for {TABLES:?}, got {refused:?}"
        );
        assert!(
            reasons
                .iter()
                .all(|reason| reason.contains(REFUSED_NAME_SENTENCE)),
            "case {case:?}: expected every failure reason to carry \
             {REFUSED_NAME_SENTENCE:?}, got {reasons:?}"
        );
        assert!(
            !listed.contains(&declared_name),
            "case {case:?}: expected S3 Tables to hold no bucket named {declared_name:?}, it \
             lists {listed:?}"
        );
    }
}
