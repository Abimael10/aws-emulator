// SPDX-License-Identifier: CC0-1.0

//! What `GetBucketTagging` answers for a general purpose bucket a CloudFormation stack
//! provisioned — whichever STACK OPERATION provisioned it — and where the documented
//! `NoSuchTagSet` refusal still belongs.
//!
//! RULE: a bucket's tag set is whatever has been STORED on it, and `GetBucketTagging` answers
//! HTTP 200 with that whole set whenever it holds at least one tag; the documented
//! `NoSuchTagSet` (HTTP 404) is the answer only for a bucket that holds no tag at all. A
//! bucket CloudFormation provisioned always holds some, because every resource it provisions
//! carries the stack-level `Tags` in force plus the three records CloudFormation writes
//! itself — `aws:cloudformation:stack-name`, `aws:cloudformation:stack-id` and
//! `aws:cloudformation:logical-id` — so a stack-created bucket is never reported as untagged,
//! not even when neither the stack operation nor the template declares a single tag. Tagging
//! is part of PROVISIONING rather than of one request, so the rule holds for every stack
//! operation that provisions a resource.
//!
//! That tag set IS the bucket-to-stack reverse lookup a client holding only a bucket name
//! has, and the absence of the three is what tells a hand-made bucket from a stack-managed
//! one — which is why the 404 has to stay reserved for a bucket that really carries nothing:
//! a client treating `NoSuchTagSet` as "this bucket is untagged" otherwise records the wrong
//! fact about a stack's bucket.
//!
//! What `cloudformation_stamps_s3_bucket_stack_provenance.rs` varies — how many stack tags,
//! how many buckets, whether the declaration states tags of its own, whether a key collides,
//! whether a declaration spells one of the three — this suite holds light and varies the
//! OTHER half of the same rule:
//!
//! * the stack operation that provisioned the bucket: `CreateStack`, an executed `CREATE`
//!   change set, and an `UpdateStack` that ADDS a bucket to a stack that already exists;
//! * the Region the stack deploys into, and whether the template spells the bucket's name;
//! * and the boundary the 404 sits on: a bucket a CLIENT created, before any tag write, after
//!   one, and after its tag set has been deleted again.
//!
//! Two crates have to agree for the rule to hold (CloudFormation stamps the tags, Amazon S3
//! stores and reports them), so the suite lives here rather than in either crate. Every
//! expected value is computed here from what the row declares plus the stack id
//! `DescribeStacks` reports — never read back out of the tag answer being checked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{execution_status, status};

// The ONE statement of the tag set this rule decides for a provisioned resource, and the
// request members a stack-level tag list is: both are service-agnostic and already stated
// once for every suite that checks this rule.
use super::cloudformation_stamps_s3vectors_stack_provenance::{
    expected_resource_tags, tag_members, LOGICAL_ID_TAG, STACK_ID_TAG, STACK_NAME_TAG,
};
use super::query_wire::{elements, form_encode, opt_element, physical_ids_of, xml_escape};

/// The change set name every row that deploys through one uses.
const CHANGE_SET: &str = "s3-bucket-provenance-plan";

/// How long a case waits for a stack operation, or a change set, to settle before it gives up.
///
/// The CONDITION is what the loops below wait for, never a span: a stack operation's terminal
/// status is owed a moment after the call that started it, and this deadline is deliberately
/// far past any span the emulator owes, so a loaded or slow machine cannot turn a settled
/// stack into a failed assertion.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll pauses between two reads of the status it is waiting on.
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The suffix of every status CloudFormation reports while an operation is still running.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The error code Amazon S3 answers for a bucket that holds no tag set, and the fixed text it
/// publishes for that code.
///
/// Source: `GetBucketTagging`'s own documentation ("Error code: NoSuchTagSet — There is no tag
/// set associated with the bucket") and the sentence S3's error-responses list carries for it,
/// which this emulator's S3 crate holds as `state::{NO_SUCH_TAG_SET_CODE,
/// NO_SUCH_TAG_SET_MESSAGE}`. Spelled out here so the suite pins the WORDING a client reads
/// rather than whatever the implementation's constant happens to say.
const NO_SUCH_TAG_SET_CODE: &str = "NoSuchTagSet";
const NO_SUCH_TAG_SET_MESSAGE: &str = "The TagSet does not exist";

/// The XML namespace a client's XML writer puts on an Amazon S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` and `region`, which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, in `region`.
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
    let http_status = response.status().as_u16();
    (
        http_status,
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// Call CloudFormation with OWNED members, which is the form the rows build.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    cloudformation(router, region, action, &borrowed).await
}

/// One member of a CloudFormation request.
fn member(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One Amazon S3 request, as a CLIENT makes it: path-style, signed for `region`.
async fn s3(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let host = format!("s3.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3", region))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let http_status = response.status().as_u16();
    (
        http_status,
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// `GET /<bucket>?tagging` — `GetBucketTagging`, the read a caller holding only a bucket name
/// makes.
async fn get_bucket_tagging(
    router: &MultiServiceRouter,
    region: &str,
    bucket: &str,
) -> (u16, String) {
    s3(router, region, "GET", &format!("/{bucket}?tagging"), "").await
}

/// `PUT /<bucket>` — `CreateBucket`, as a client with no stack creates one.
///
/// `CreateBucketConfiguration$LocationConstraint` is what decides a bucket's Region and
/// "buckets in Region us-east-1 have a LocationConstraint of null", so the constraint is sent
/// for every other Region and nothing is sent for that one.
async fn create_bucket(router: &MultiServiceRouter, region: &str, bucket: &str) -> (u16, String) {
    let body = if region == "us-east-1" {
        String::new()
    } else {
        format!(
            "<CreateBucketConfiguration xmlns=\"{XMLNS}\"><LocationConstraint>{}\
             </LocationConstraint></CreateBucketConfiguration>",
            xml_escape(region)
        )
    };
    s3(router, region, "PUT", &format!("/{bucket}"), &body).await
}

/// `PUT /<bucket>?tagging` — `PutBucketTagging`, which "will overwrite any current tags the
/// bucket already has".
async fn put_bucket_tagging(
    router: &MultiServiceRouter,
    region: &str,
    bucket: &str,
    tags: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("<Tagging xmlns=\"{XMLNS}\"><TagSet>");
    for (key, value) in tags {
        body.push_str(&format!(
            "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
            xml_escape(key),
            xml_escape(value)
        ));
    }
    body.push_str("</TagSet></Tagging>");
    s3(router, region, "PUT", &format!("/{bucket}?tagging"), &body).await
}

/// `DELETE /<bucket>?tagging` — `DeleteBucketTagging`, which removes the whole tag set.
async fn delete_bucket_tagging(
    router: &MultiServiceRouter,
    region: &str,
    bucket: &str,
) -> (u16, String) {
    s3(router, region, "DELETE", &format!("/{bucket}?tagging"), "").await
}

/// The tag set a `GetBucketTagging` body reports, as the key-to-value map a client reads it
/// as.
///
/// Each pair is read from the `<Tag>` that carries BOTH halves, rather than by zipping two
/// document-ordered lists, so a member that ever omitted one half misaligns nothing.
fn tag_set_of(xml: &str) -> BTreeMap<String, String> {
    let mut held: BTreeMap<String, String> = BTreeMap::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Tag>") {
        let after = &rest[start + "<Tag>".len()..];
        let Some(end) = after.find("</Tag>") else {
            break;
        };
        let tag = &after[..end];
        if let (Some(key), Some(value)) = (
            elements(tag, "Key").into_iter().next(),
            elements(tag, "Value").into_iter().next(),
        ) {
            held.insert(key, value);
        }
        rest = &after[end + "</Tag>".len()..];
    }
    held
}

/// The `Code` and `Message` of an Amazon S3 error body, as a client reads them.
fn refusal_of(xml: &str) -> (String, String) {
    (
        opt_element(xml, "Code").unwrap_or_default(),
        opt_element(xml, "Message").unwrap_or_default(),
    )
}

/// Which stack operation provisioned the bucket a row reads back.
#[derive(Debug, Clone, Copy)]
enum DeployedBy {
    /// `CreateStack` with the row's template.
    CreateStack,
    /// `CreateChangeSet` with `ChangeSetType=CREATE`, then `ExecuteChangeSet` — the plan path,
    /// which brings the stack into existence in review and then deploys it.
    ExecutedChangeSet,
    /// `CreateStack` with the row's first template, then `UpdateStack` with its second, which
    /// ADDS a bucket to a stack that already exists.
    CreateThenUpdate,
}

/// One bucket a row reads back: its logical id in the template and the `Tags` that declaration
/// asks for.
struct Declared {
    logical_id: &'static str,
    declared: &'static [(&'static str, &'static str)],
}

/// One row: the deployment path, the Region, the stack-level tags in force, the template(s) the
/// path deploys and the buckets whose tag sets the rule decides.
struct Row {
    case: &'static str,
    region: &'static str,
    deployed_by: DeployedBy,
    /// The `Tags` the stack operation supplies, propagated to every resource.
    stack_tags: &'static [(&'static str, &'static str)],
    /// The template the path deploys first.
    template: &'static str,
    /// The template an `UpdateStack` deploys afterwards, for the row whose path is an update.
    updated_template: Option<&'static str>,
    resources: &'static [Declared],
}

/// A named bucket declaring nothing at all beside its name — the shape the three reserved
/// records are the WHOLE tag set of.
const PLAIN_BUCKET_TEMPLATE: &str = r#"{"Resources":{
     "Reports":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"paths-plain"}}}}"#;

/// A bucket the template does not name, so CloudFormation generates the name, and that also
/// configures its versioning — so the tags are not the only thing the create carries.
const GENERATED_NAME_TEMPLATE: &str = r#"{"Resources":{
     "Archive":{"Type":"AWS::S3::Bucket","Properties":{
       "VersioningConfiguration":{"Status":"Enabled"}}}}}"#;

/// The stack an update then ADDS a second bucket to.
const ONE_BUCKET_TEMPLATE: &str = r#"{"Resources":{
     "Existing":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"paths-existing"}}}}"#;

/// That stack's next revision: the bucket it already had, plus one the update provisions.
const ADDED_BUCKET_TEMPLATE: &str = r#"{"Resources":{
     "Existing":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"paths-existing"}},
     "Added":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"paths-added",
       "Tags":[{"Key":"Service","Value":"added"}]}}}}"#;

const ROWS: &[Row] = &[
    Row {
        case: "CreateStack provisions a bucket with no stack tags and no declared tags",
        region: "us-east-1",
        deployed_by: DeployedBy::CreateStack,
        stack_tags: &[],
        template: PLAIN_BUCKET_TEMPLATE,
        updated_template: None,
        resources: &[Declared {
            logical_id: "Reports",
            declared: &[],
        }],
    },
    Row {
        case: "an executed CREATE change set provisions a bucket CloudFormation names itself",
        region: "us-west-2",
        deployed_by: DeployedBy::ExecutedChangeSet,
        stack_tags: &[],
        template: GENERATED_NAME_TEMPLATE,
        updated_template: None,
        resources: &[Declared {
            logical_id: "Archive",
            declared: &[],
        }],
    },
    Row {
        case: "an executed CREATE change set with stack tags beside the three records",
        region: "eu-west-1",
        deployed_by: DeployedBy::ExecutedChangeSet,
        stack_tags: &[("Owner", "platform"), ("Tier", "prod")],
        template: PLAIN_BUCKET_TEMPLATE,
        updated_template: None,
        resources: &[Declared {
            logical_id: "Reports",
            declared: &[],
        }],
    },
    Row {
        case: "an UpdateStack that adds a bucket to a stack that already has one",
        region: "ap-northeast-1",
        deployed_by: DeployedBy::CreateThenUpdate,
        stack_tags: &[("Tier", "prod")],
        template: ONE_BUCKET_TEMPLATE,
        updated_template: Some(ADDED_BUCKET_TEMPLATE),
        resources: &[
            Declared {
                logical_id: "Existing",
                declared: &[],
            },
            Declared {
                logical_id: "Added",
                declared: &[("Service", "added")],
            },
        ],
    },
];

/// Read the stack until it reports a status that is not `*_IN_PROGRESS`, and answer with that
/// status and the body it was read from.
async fn settled(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let started = Instant::now();
    loop {
        let (http_status, body) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: DescribeStacks({stack}) must answer 200, got {http_status} {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with(IN_PROGRESS) {
            return (reported, body);
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the stack {stack} is still {reported:?} after waiting for it to \
             settle: {body}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// Read the change set until it reports itself executable (`ExecutionStatus: AVAILABLE`), which
/// is what says the plan may be executed.
async fn executable(case: &str, router: &MultiServiceRouter, region: &str, stack: &str) {
    let started = Instant::now();
    loop {
        let (http_status, body) = cloudformation(
            router,
            region,
            "DescribeChangeSet",
            &[("ChangeSetName", CHANGE_SET), ("StackName", stack)],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: DescribeChangeSet({CHANGE_SET}) must answer 200, got {http_status} \
             {body}"
        );
        let reported = opt_element(&body, "ExecutionStatus").unwrap_or_default();
        if reported == execution_status::AVAILABLE {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the change set {CHANGE_SET} is still {reported:?} after waiting for \
             it to become executable: {body}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// Deploy one row's stack by the operation the row names, and answer with the stack id
/// `DescribeStacks` reports for it.
async fn deployed(router: &MultiServiceRouter, row: &Row, stack: &str) -> String {
    let case = row.case;
    let region = row.region;
    let tags = tag_members(row.stack_tags);
    match row.deployed_by {
        DeployedBy::CreateStack | DeployedBy::CreateThenUpdate => {
            let mut members = vec![
                member("StackName", stack),
                member("TemplateBody", row.template),
            ];
            members.extend(tags.clone());
            let (http_status, body) = call(router, region, "CreateStack", &members).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: CreateStack must be accepted, got {http_status} {body}"
            );
        }
        DeployedBy::ExecutedChangeSet => {
            let mut members = vec![
                member("StackName", stack),
                member("ChangeSetName", CHANGE_SET),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", row.template),
            ];
            members.extend(tags.clone());
            let (http_status, body) = call(router, region, "CreateChangeSet", &members).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: CreateChangeSet must be accepted, got {http_status} {body}"
            );
            // A change set is executable once its own creation has completed; until it is
            // executed the stack it created rests in review, with no deployment of its own.
            executable(case, router, region, stack).await;
            let (http_status, reviewed) =
                cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
            assert_eq!(
                (http_status, opt_element(&reviewed, "StackStatus").as_deref()),
                (200, Some(status::REVIEW_IN_PROGRESS)),
                "case {case:?}: a change set that has not been executed leaves the stack in \
                 review, got {http_status} {reviewed}"
            );
            let (http_status, body) = call(
                router,
                region,
                "ExecuteChangeSet",
                &[
                    member("ChangeSetName", CHANGE_SET),
                    member("StackName", stack),
                ],
            )
            .await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: ExecuteChangeSet must be accepted, got {http_status} {body}"
            );
        }
    }
    let (reported, body) = settled(case, router, region, stack).await;
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {reported:?} {body}"
    );

    if let Some(updated) = row.updated_template {
        let mut members = vec![member("StackName", stack), member("TemplateBody", updated)];
        members.extend(tags);
        let (http_status, body) = call(router, region, "UpdateStack", &members).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: UpdateStack must be accepted, got {http_status} {body}"
        );
        let (reported, body) = settled(case, router, region, stack).await;
        assert_eq!(
            reported,
            status::UPDATE_COMPLETE,
            "case {case:?}: expected the update to complete, got {reported:?} {body}"
        );
    }

    let (http_status, described) =
        cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: the deployed stack must be readable, got {http_status} {described}"
    );
    opt_element(&described, "StackId").unwrap_or_else(|| {
        panic!("case {case:?}: the stack must report a StackId, got {described}")
    })
}

/// The handles the stack publishes for its resources, as the logical-id to physical-id map.
async fn published_handles(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (http_status, resources) = cloudformation(
        router,
        region,
        "DescribeStackResources",
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: DescribeStackResources({stack}) must answer 200, got {http_status} \
         {resources}"
    );
    physical_ids_of(&resources)
}

/// The tag set the rule says the bucket `logical_id` of this row ends up carrying, in the stack
/// `stack` whose id is `stack_id` — stated once for every suite that checks this rule
/// (`expected_resource_tags`), with this row's declaration read out of it.
fn expected_tags(
    row: &Row,
    logical_id: &str,
    stack: &str,
    stack_id: &str,
) -> BTreeMap<String, String> {
    let declared: Vec<(&str, &str)> = row
        .resources
        .iter()
        .filter(|resource| resource.logical_id == logical_id)
        .flat_map(|resource| resource.declared.iter().copied())
        .collect();
    expected_resource_tags(&declared, row.stack_tags, stack, stack_id, logical_id)
}

/// Every row: `GetBucketTagging` on the bucket the stack published answers HTTP 200 with the
/// stack's own tags, the declaration's own tags and the three tags CloudFormation records —
/// whichever stack operation provisioned the bucket, so a bucket that declares no tag at all is
/// never reported as having no tag set.
#[tokio::test]
async fn every_provisioning_path_serves_the_stack_provenance_through_get_bucket_tagging() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        // One router per row, so a bucket name one row spells cannot collide with another's.
        let router = build_router();
        let stack = format!("s3-bucket-paths-{index}");
        let stack_id = deployed(&router, row, &stack).await;
        let handles = published_handles(case, &router, region, &stack).await;

        for resource in row.resources {
            let logical_id = resource.logical_id;
            // The handle the stack publishes for the resource, which for this type IS the
            // bucket name ("Ref returns the bucket name").
            let bucket = handles.get(logical_id).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: the stack must publish a physical id for {logical_id:?}")
            });

            let (http_status, body) = get_bucket_tagging(&router, region, &bucket).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: GetBucketTagging({bucket}) must answer 200, got {http_status} \
                 with body {body}"
            );
            let held = tag_set_of(&body);
            let expected = expected_tags(row, logical_id, &stack, &stack_id);
            assert_eq!(
                held, expected,
                "case {case:?}: expected {logical_id} to carry {expected:?}, got {held:?}"
            );
            for reserved in [STACK_NAME_TAG, STACK_ID_TAG, LOGICAL_ID_TAG] {
                assert!(
                    held.contains_key(reserved),
                    "case {case:?}: expected {reserved:?} on a bucket the stack provisioned, got \
                     {held:?}"
                );
            }
        }
    }
}

/// The `NoSuchTagSet` refusal is about the tag set being EMPTY, nothing else: a bucket a client
/// created carries no tag until one is written, answers 200 with whatever was written, and
/// answers the refusal again once the set is deleted — while the stack-provisioned bucket in
/// the same emulator, which declared no tag of its own either, answers 200 all along because
/// CloudFormation's three records are on it.
///
/// The rows differ along what has been written to the bucket's tag set and along how many keys
/// it holds, which is what the rule generalises over.
#[tokio::test]
async fn the_no_such_tag_set_refusal_is_reserved_for_a_bucket_holding_no_tags() {
    let region = "us-east-2";
    let router = build_router();

    // The stack-provisioned bucket, which declares no tag of its own.
    let row = &Row {
        case: "a stack-provisioned bucket declaring no tag of its own",
        region,
        deployed_by: DeployedBy::CreateStack,
        stack_tags: &[],
        template: PLAIN_BUCKET_TEMPLATE,
        updated_template: None,
        resources: &[Declared {
            logical_id: "Reports",
            declared: &[],
        }],
    };
    let stack = "s3-bucket-tag-set-boundary";
    let stack_id = deployed(&router, row, stack).await;
    let handles = published_handles(row.case, &router, region, stack).await;
    let stack_bucket = handles
        .get("Reports")
        .cloned()
        .unwrap_or_else(|| panic!("the stack must publish a physical id, got {handles:?}"));

    // The hand-made bucket beside it, in the same emulator and the same Region.
    let client_bucket = "boundary-hand-made";
    let (http_status, body) = create_bucket(&router, region, client_bucket).await;
    assert_eq!(
        http_status, 200,
        "CreateBucket({client_bucket}) must answer 200, got {http_status} {body}"
    );

    // Each step writes the client bucket's tag set and states what the rule then decides for
    // it: `None` is the refusal, `Some(set)` is the 200 and the whole set it reports.
    #[allow(clippy::type_complexity)]
    let steps: &[(&str, Option<&[(&str, &str)]>)] = &[
        ("a bucket nothing has ever tagged", None),
        ("a bucket holding one tag", Some(&[("Service", "reports")])),
        (
            "a bucket holding several tags",
            Some(&[("Service", "reports"), ("Tier", "prod"), ("Owner", "data")]),
        ),
        (
            "a bucket whose only tag names the reserved prefix of another namespace",
            Some(&[("not-aws:cloudformation:stack-id", "forged")]),
        ),
        ("a bucket whose tag set has been deleted again", None),
    ];

    for (case, written) in steps {
        match written {
            Some(tags) => {
                let (http_status, body) =
                    put_bucket_tagging(&router, region, client_bucket, tags).await;
                assert_eq!(
                    http_status, 204,
                    "case {case:?}: PutBucketTagging must answer 204, got {http_status} {body}"
                );
            }
            None => {
                let (http_status, body) =
                    delete_bucket_tagging(&router, region, client_bucket).await;
                assert_eq!(
                    http_status, 204,
                    "case {case:?}: DeleteBucketTagging must answer 204, got {http_status} {body}"
                );
            }
        }

        let (http_status, body) = get_bucket_tagging(&router, region, client_bucket).await;
        match written {
            Some(tags) => {
                assert_eq!(
                    http_status, 200,
                    "case {case:?}: a bucket that holds tags must answer 200, got {http_status} \
                     {body}"
                );
                let expected: BTreeMap<String, String> = tags
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect();
                let held = tag_set_of(&body);
                assert_eq!(
                    held, expected,
                    "case {case:?}: expected the written set {expected:?}, got {held:?}"
                );
                for reserved in [STACK_NAME_TAG, STACK_ID_TAG, LOGICAL_ID_TAG] {
                    assert!(
                        !held.contains_key(reserved),
                        "case {case:?}: expected no {reserved:?} on a bucket no stack created, \
                         got {held:?}"
                    );
                }
            }
            None => {
                assert_eq!(
                    http_status, 404,
                    "case {case:?}: a bucket holding no tag must be refused with 404, got \
                     {http_status} {body}"
                );
                let (code, message) = refusal_of(&body);
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    (NO_SUCH_TAG_SET_CODE, NO_SUCH_TAG_SET_MESSAGE),
                    "case {case:?}: expected the documented refusal, got {body}"
                );
            }
        }

        // Whatever the client did to its own bucket, the stack's bucket still answers with the
        // provenance it was provisioned with.
        let (http_status, body) = get_bucket_tagging(&router, region, &stack_bucket).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: GetBucketTagging({stack_bucket}) must answer 200, got {http_status} \
             {body}"
        );
        let held = tag_set_of(&body);
        let expected = expected_tags(row, "Reports", stack, &stack_id);
        assert_eq!(
            held, expected,
            "case {case:?}: expected the stack's bucket to carry {expected:?}, got {held:?}"
        );
    }
}
