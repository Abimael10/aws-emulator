// SPDX-License-Identifier: CC0-1.0

//! The three `aws:cloudformation:*` tags a bucket carries when the stack that provisioned it is
//! itself a NESTED stack, read back through Amazon S3's own `GetBucketTagging`.
//!
//! RULE: a resource CloudFormation provisions carries the stack-level tags in force on the
//! stack that declared it PLUS the three records CloudFormation writes itself —
//! `aws:cloudformation:stack-name`, `aws:cloudformation:stack-id` and
//! `aws:cloudformation:logical-id` — and those three name the stack that DIRECTLY declared the
//! resource together with the logical id it is declared under in THAT stack's template; the
//! resource's own declared tags keep the value they state, except those three, which are
//! CloudFormation's to set. `GetBucketTagging` answers HTTP 200 with that whole set.
//!
//! A nesting hierarchy is the case that tells the directly owning stack apart from the one the
//! client deployed: a stack's tags propagate down into the nested stack its template declares,
//! so the stack-level set that reaches the bucket is every level's tags with the innermost
//! declaration keeping a key two levels state, while the three records name the INNERMOST
//! stack — not the root the client called `CreateStack` on. That is what makes the tag set a
//! usable reverse lookup: the stamped id resolves to the stack that holds the bucket as a
//! resource, and that stack's own `RootId` leads on to the top of the hierarchy.
//!
//! The rows differ along what the rule generalises over: how DEEP the bucket sits (one level of
//! nesting and two), how many stack-level tags the client supplied (none, one, several),
//! whether each nested-stack declaration states tags of its own, whether a key COLLIDES
//! between levels, whether the bucket declares tags of its own, and whether the template spells
//! the bucket's name.
//!
//! Three parts have to agree for the rule to hold — CloudFormation provisions the hierarchy,
//! Amazon S3 holds the child templates the nested-stack resources point at and stores and
//! reports the bucket's tags — so the suite lives here rather than in either crate. Every
//! expected value is computed here from what the row declares plus the ids CloudFormation
//! reports, never read back out of the tag answer being checked.
//!
//! Nothing here sleeps for a span the emulator owes: every wait polls for the CONDITION under a
//! deadline far longer than the spans this emulator gives these transitions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE statement of the tag set this rule decides for a provisioned resource, the request
// members a stack-level tag list is, and the three reserved keys: all four are
// service-agnostic and already stated once for every suite that checks this rule.
use super::cloudformation_stamps_s3vectors_stack_provenance::{
    expected_resource_tags, tag_members, LOGICAL_ID_TAG, STACK_ID_TAG, STACK_NAME_TAG,
};
use super::query_wire::{form_encode, members_of, opt_element, physical_ids_of, tag_set_of};

/// The Region every request here is signed for, and so the Region the hierarchy deploys into.
const REGION: &str = "us-east-1";

/// How long a case waits for a stack operation to settle, and how often it looks again.
///
/// The CONDITION is what the loop waits for, never a span: a stack operation's terminal status
/// is owed a moment after the call that started it, and this deadline is deliberately far past
/// any span the emulator owes, so a loaded or slow machine cannot turn a settled hierarchy into
/// a failed assertion.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The suffix of every status CloudFormation reports while an operation is still running.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The bucket each row stages its child templates in, which is where a nested stack's
/// `TemplateURL` points. It is created by a CLIENT, so it is not part of any hierarchy.
const STAGING_BUCKET: &str = "nested-provenance-templates";

/// The logical ids the nested-stack resources of the chain are declared under, outermost first.
const CHAIN: &[&str] = &["Storage", "Inner"];

/// The emulator as it is ASSEMBLED, on one engine: the nested-stack provider calls
/// CloudFormation in-process and reads its child template out of Amazon S3, so the suite runs
/// the whole router rather than a hand-picked pair of services.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
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
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).into_owned())
}

/// [`cloudformation`] for a call whose members are OWNED, which is the form the rows build.
async fn call(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    cloudformation(router, action, &borrowed).await
}

/// One request against Amazon S3's REST surface, path-style, as a CLIENT makes it.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// `GET /<bucket>?tagging` — `GetBucketTagging`, the read a caller holding only a bucket name
/// makes.
async fn get_bucket_tagging(router: &MultiServiceRouter, bucket: &str) -> (u16, String) {
    s3(router, "GET", &format!("/{bucket}?tagging"), "").await
}

/// The URL a staged child template is addressed by, path-style.
fn staged_url(key: &str) -> String {
    format!("https://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{key}")
}

/// One row: the stack-level tags the client supplies, the tags each nested-stack declaration of
/// the chain states (outermost first, so its length is how deep the bucket sits), and what the
/// innermost template declares the bucket as.
struct Row {
    case: &'static str,
    stack_tags: &'static [(&'static str, &'static str)],
    nested_tags: &'static [&'static [(&'static str, &'static str)]],
    bucket_logical_id: &'static str,
    /// The name the template spells for the bucket, or `None` to leave the naming to
    /// CloudFormation.
    bucket_name: Option<&'static str>,
    bucket_tags: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a bucket one level down, with no tag declared anywhere",
        stack_tags: &[],
        nested_tags: &[&[]],
        bucket_logical_id: "EventNotificationBucket",
        bucket_name: None,
        bucket_tags: &[],
    },
    Row {
        case: "a bucket one level down, beside a stack tag, a nested declaration's tag and its \
               own",
        stack_tags: &[("Owner", "platform")],
        nested_tags: &[&[("Tier", "prod")]],
        bucket_logical_id: "Assets",
        bucket_name: Some("nested-provenance-assets"),
        bucket_tags: &[("Service", "assets")],
    },
    Row {
        case: "a nested declaration restating a key the client's stack tags state",
        stack_tags: &[("Tier", "root"), ("Owner", "platform")],
        nested_tags: &[&[("Tier", "child")]],
        bucket_logical_id: "Reports",
        bucket_name: None,
        bucket_tags: &[],
    },
    Row {
        case: "a bucket two levels down, each level declaring a tag of its own",
        stack_tags: &[("Owner", "platform")],
        nested_tags: &[&[("Tier", "prod")], &[("Scope", "inner")]],
        bucket_logical_id: "Archive",
        bucket_name: Some("nested-provenance-archive"),
        bucket_tags: &[("Service", "archive")],
    },
    Row {
        case: "a bucket two levels down where every level restates one key",
        stack_tags: &[("Tier", "root")],
        nested_tags: &[&[("Tier", "middle")], &[("Tier", "inner")]],
        bucket_logical_id: "DeepStore",
        bucket_name: None,
        bucket_tags: &[],
    },
];

/// The stack-level tag set IN FORCE on the stack that declares the bucket: every level's tags,
/// innermost first, each key kept at the value the innermost level that states it gives it —
/// the propagation one level makes into the next, applied down the chain.
fn stack_tags_in_force(row: &Row) -> Vec<(&'static str, &'static str)> {
    let mut in_force: Vec<(&'static str, &'static str)> = Vec::new();
    let mut consider = |key: &'static str, value: &'static str| {
        if !in_force.iter().any(|(held, _)| *held == key) {
            in_force.push((key, value));
        }
    };
    for level in row.nested_tags.iter().rev() {
        for (key, value) in level.iter().copied() {
            consider(key, value);
        }
    }
    for (key, value) in row.stack_tags.iter().copied() {
        consider(key, value);
    }
    in_force
}

/// A template's `Tags` property, as a declaration spells it.
fn tag_list(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    )
}

/// The innermost template: the one that declares the bucket.
fn bucket_template(row: &Row) -> String {
    let mut properties = Map::new();
    if let Some(name) = row.bucket_name {
        properties.insert("BucketName".to_string(), json!(name));
    }
    if !row.bucket_tags.is_empty() {
        properties.insert("Tags".to_string(), tag_list(row.bucket_tags));
    }
    let mut resources = Map::new();
    resources.insert(
        row.bucket_logical_id.to_string(),
        json!({"Type": "AWS::S3::Bucket", "Properties": Value::Object(properties)}),
    );
    json!({"Resources": Value::Object(resources)}).to_string()
}

/// The row the reverse-lookup test below drives, named by its `case` rather than by its
/// position, so an insertion into [`ROWS`] cannot change which member of the class it
/// exercises.
const REVERSE_LOOKUP_CASE: &str = "a bucket two levels down, each level declaring a tag of its own";

/// One level of nesting: a template whose only resource is the stack below it.
fn nesting_template(logical_id: &str, template_url: &str, tags: &[(&str, &str)]) -> String {
    let mut properties = Map::new();
    properties.insert("TemplateURL".to_string(), json!(template_url));
    if !tags.is_empty() {
        properties.insert("Tags".to_string(), tag_list(tags));
    }
    let mut resources = Map::new();
    resources.insert(
        logical_id.to_string(),
        json!({
            "Type": "AWS::CloudFormation::Stack",
            "Properties": Value::Object(properties)
        }),
    );
    json!({"Resources": Value::Object(resources)}).to_string()
}

/// Stage one template in Amazon S3, where a nested-stack declaration's `TemplateURL` reaches it.
async fn stage(router: &MultiServiceRouter, case: &str, key: &str, body: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{STAGING_BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: staging {key:?} must be accepted, got {status} with {answered}"
    );
}

/// Poll one stack until it reports a status that is not `*_IN_PROGRESS`, and answer with that
/// status and the body it was read from.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks({stack}) must answer, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with(IN_PROGRESS) {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last was {reported:?} with {body}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// Every resource one stack reports a FAILED status for, with the reason it gave: what says WHY
/// a hierarchy did not come up.
async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (status, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    if status != 200 {
        return Vec::new();
    }
    members_of(&body)
        .into_iter()
        .filter(|member| {
            opt_element(member, "ResourceStatus").is_some_and(|status| status.contains("FAILED"))
        })
        .map(|member| {
            format!(
                "{}={}",
                opt_element(member, "LogicalResourceId").unwrap_or_default(),
                opt_element(member, "ResourceStatusReason").unwrap_or_default()
            )
        })
        .collect()
}

/// The `PhysicalResourceId` one stack reports for one of its resources — for an
/// `AWS::CloudFormation::Stack` that is the stack below it, by that stack's own id.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResource({stack}, {logical_id}) must answer, got {status} \
         with {body}"
    );
    opt_element(&body, "PhysicalResourceId").unwrap_or_default()
}

/// What one row's deployment published: the root stack's id, the innermost stack's id and name,
/// and the handle the innermost stack reports for its bucket.
struct Deployed {
    root_id: String,
    stack_id: String,
    stack_name: String,
    bucket: String,
}

/// Deploy one row's hierarchy: stage the chain's templates from the innermost outward, create
/// the root stack with the client's own stack tags, and follow the nesting down to the bucket.
async fn deployed(router: &MultiServiceRouter, row: &Row, root_stack: &str) -> Deployed {
    let case = row.case;
    assert!(
        row.nested_tags.len() <= CHAIN.len(),
        "case {case:?}: the chain names {} levels, so a row cannot ask for {}",
        CHAIN.len(),
        row.nested_tags.len()
    );
    let (status, body) = s3(router, "PUT", &format!("/{STAGING_BUCKET}"), "").await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: the staging bucket must be created, got {status} with {body}"
    );

    // The chain, built from the bucket upward: each level is staged and the level above it
    // points at where it was staged.
    let mut template = bucket_template(row);
    for level in (0..row.nested_tags.len()).rev() {
        let key = format!("level-{level}.json");
        stage(router, case, &key, &template).await;
        template = nesting_template(CHAIN[level], &staged_url(&key), row.nested_tags[level]);
    }

    let mut members: Vec<(String, String)> = vec![
        ("StackName".to_string(), root_stack.to_string()),
        ("TemplateBody".to_string(), template),
    ];
    members.extend(tag_members(row.stack_tags));
    let (status, body) = call(router, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must be accepted, got {status} with {body}"
    );
    let (reported, body) = settled(router, case, root_stack).await;
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the root stack to come up, got {reported} with {body}; failures \
         {:?}",
        reported_failures(router, root_stack).await
    );
    let root_id = opt_element(&body, "StackId").unwrap_or_default();

    // Down the hierarchy: each level reports the stack below it by that stack's own id.
    let mut stack_id = root_id.clone();
    for logical_id in &CHAIN[..row.nested_tags.len()] {
        stack_id = physical_id(router, case, &stack_id, logical_id).await;
        assert!(
            stack_id.starts_with("arn:"),
            "case {case:?}: expected {logical_id:?} to report a nested stack's id, got \
             {stack_id:?}"
        );
    }
    let (reported, described) = settled(router, case, &stack_id).await;
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the innermost stack to come up, got {reported} with {described}"
    );
    let stack_name = opt_element(&described, "StackName").unwrap_or_default();

    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", &stack_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources({stack_id}) must answer, got {status} with \
         {resources}"
    );
    let published = physical_ids_of(&resources);
    let bucket = published
        .get(row.bucket_logical_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: no physical id for {:?} in {published:?}",
                row.bucket_logical_id
            )
        });
    Deployed {
        root_id,
        stack_id,
        stack_name,
        bucket,
    }
}

/// Every row: the bucket a nested stack provisioned carries the tag set in force on THAT stack
/// plus the three records naming it — the stack the client deployed is not what the three name.
#[tokio::test]
async fn a_nested_stack_stamps_its_own_provenance_onto_the_bucket_it_creates() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One router per row, so a bucket name a row spells cannot collide with another's.
        let router = build_router();
        let root_stack = format!("nested-provenance-root-{index}");
        let published = deployed(&router, row, &root_stack).await;

        let (status, body) = get_bucket_tagging(&router, &published.bucket).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetBucketTagging({}), got {status} with {body}",
            published.bucket
        );
        let held = tag_set_of(&body);
        let expected = expected_resource_tags(
            row.bucket_tags,
            &stack_tags_in_force(row),
            &published.stack_name,
            &published.stack_id,
            row.bucket_logical_id,
        );
        assert_eq!(
            held, expected,
            "case {case:?}: expected the bucket {} to carry {expected:?}, got {held:?}",
            published.bucket
        );
        // The three name the stack that DECLARED the bucket, which is a stack below the one the
        // client deployed: a hierarchy whose bucket was stamped with the root's identity would
        // report the root's name and id here.
        assert_ne!(
            held.get(STACK_NAME_TAG),
            Some(&root_stack),
            "case {case:?}: expected the stamped stack name to be the nested stack's, not the \
             root stack's, got {held:?}"
        );
        assert_ne!(
            held.get(STACK_ID_TAG),
            Some(&published.root_id),
            "case {case:?}: expected the stamped stack id to be the nested stack's, not the \
             root stack's, got {held:?}"
        );
    }
}

/// The three keys are the bucket-to-stack REVERSE LOOKUP inside a hierarchy: everything the
/// client knows is what the tag read answered, and from it the stack holding the bucket, its
/// place in that stack's template and the top of the hierarchy all resolve.
#[tokio::test]
async fn the_stamped_tags_resolve_to_the_nested_stack_and_on_to_its_root() {
    let router = build_router();
    let row = ROWS
        .iter()
        .find(|row| row.case == REVERSE_LOOKUP_CASE)
        .unwrap_or_else(|| panic!("the row {REVERSE_LOOKUP_CASE:?} must be one of the rows"));
    let case = row.case;
    let root_stack = "nested-provenance-reverse-lookup";
    let published = deployed(&router, row, root_stack).await;

    // From here on, the only thing the client holds is the bucket name.
    let (status, body) = get_bucket_tagging(&router, &published.bucket).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetBucketTagging({}), got {status} with {body}",
        published.bucket
    );
    let held = tag_set_of(&body);
    let stamped_id = held.get(STACK_ID_TAG).cloned().unwrap_or_default();
    let stamped_name = held.get(STACK_NAME_TAG).cloned().unwrap_or_default();
    let stamped_logical_id = held.get(LOGICAL_ID_TAG).cloned().unwrap_or_default();

    let (status, described) =
        cloudformation(&router, "DescribeStacks", &[("StackName", &stamped_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the stamped stack id {stamped_id:?} to resolve, got {status} \
         with {described}"
    );
    assert_eq!(
        opt_element(&described, "StackName"),
        Some(stamped_name.clone()),
        "case {case:?}: expected the stack the stamped id resolves to to be {stamped_name:?}, \
         got {described}"
    );
    // The stack the stamped id resolves to really holds the bucket, under the stamped logical id.
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", &stamped_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the stamped stack id to resolve for its resources, got {status} \
         with {resources}"
    );
    assert_eq!(
        physical_ids_of(&resources).get(&stamped_logical_id),
        Some(&published.bucket),
        "case {case:?}: expected the resource at the stamped logical id {stamped_logical_id:?} to \
         be the bucket {}, got {resources}",
        published.bucket
    );
    // ...and that stack is a nested one, so the hierarchy's top is reachable from it: the stack
    // the client deployed.
    assert_eq!(
        opt_element(&described, "RootId"),
        Some(published.root_id.clone()),
        "case {case:?}: expected the stamped stack to report the root {:?} the client deployed, \
         got {described}",
        published.root_id
    );
}
