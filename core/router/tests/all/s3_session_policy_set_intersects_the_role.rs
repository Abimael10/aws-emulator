// SPDX-License-Identifier: CC0-1.0

//! The session policy documents ONE mint passed form one SET, and that set intersects the
//! role's own policy — decided by the service the scoped credential then calls.
//!
//! RULE: the `Allow`s of the session policies a mint passes are UNIONED into one set, and the
//! permissions of the credential it vends are that set INTERSECTED with the identity-based
//! policy of the role. "You can pass a single JSON policy document to use as an inline
//! session policy. You can also specify up to 10 managed policy Amazon Resource Names (ARNs)
//! to use as managed session policies… The resulting session's permissions are the
//! intersection of the role's identity-based policy and the session policies. You cannot use
//! session policies to grant more permissions than those allowed by the identity-based policy
//! of the role that is being assumed" (`AssumeRole`, the STS model). So a call only ONE
//! document of the set names is still allowed — the set is a union, not a chain of
//! intersections between its own documents — while a call either side withholds is refused,
//! and each refusal carries the diagnosis of the side that withheld it: "because no
//! identity-based policy allows the <action> action" where the role is the side that did not
//! name it, and "because no session policy allows the <action> action" where the set is.
//!
//! `PackedPolicySize` is "the percentage of the maximum size limit… that was used"
//! (`AssumeRoleResponse$PackedPolicySize`), so a mint that passed documents reports a
//! percentage inside 1..100, and it GROWS with the documents passed.
//!
//! Three services have to agree for this to hold — STS mints the session and records the set
//! it was minted with, IAM holds the managed policy the ARN names and the role's own
//! documents, and Amazon S3 is the gate that decides the data-plane call against them — so
//! this is the suite that can pin it end to end.
//!
//! The rows span what the rule generalises over: WHICH part of the set names the action (both
//! documents, the managed document alone, neither) against whether the ROLE names it, which
//! is the intersection's own table; the LEVEL the call is made at (a bucket listing, a bucket
//! tagging read, an object read and an object write); and the two controls that show neither
//! side is simply stuck — the same role with NO session policy may make the call the set
//! withheld, and widening the ROLE makes the call only the set allowed succeed.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder a form body needs and the reader of one element of an XML answer, both
// the test crate's single copy (`query_wire`): a suite that writes its own drifts from the
// others, which is what that module exists to prevent.
use super::query_wire::{form_encode, opt_element};

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own administrative
/// credential: it holds no IAM policies, so the fixtures it writes are not what any row is
/// about.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The role every session below is minted from.
const ROLE_NAME: &str = "SessionSetRole";

/// The bucket and the object every row addresses, and the bytes a write presents.
const BUCKET: &str = "session-policy-set-bucket";
const OBJECT_KEY: &str = "readable";
const BODY: &[u8] = b"session-policy-set";

/// The four IAM actions the documents are written over.
const LIST_BUCKET: &str = "s3:ListBucket";
const GET_OBJECT: &str = "s3:GetObject";
const PUT_OBJECT: &str = "s3:PutObject";
const GET_BUCKET_TAGGING: &str = "s3:GetBucketTagging";

/// A trust policy granting the account root permission to assume a role.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// Amazon S3 beside the two services a decision about a scoped session reads from, over ONE
/// engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key each service reads the caller's identity from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Post one awsQuery form body to `signing_name`, presenting `access_key_id`.
async fn query(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    body: String,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(access_key_id, signing_name))
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One IAM action, presenting `access_key_id`.
async fn iam(router: &MultiServiceRouter, access_key_id: &str, body: String) -> (u16, String) {
    query(
        router,
        access_key_id,
        "iam",
        format!("Version=2010-05-08&{body}"),
    )
    .await
}

/// One S3 request to `uri`, presenting `access_key_id`.
async fn s3(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization(access_key_id, "s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    // S3 escapes the quotes its messages carry, so the text is read back as it was worded.
    let text = String::from_utf8_lossy(response.body())
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    (status, text)
}

/// One policy document allowing `actions` over the bucket and everything in it.
fn allowing(actions: &[&str]) -> String {
    let actions = actions
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":[{actions}],"Resource":["arn:aws:s3:::{BUCKET}","arn:aws:s3:::{BUCKET}/*"]}}]}}"#
    )
}

/// Create the role, holding `policy` as its only inline document.
async fn given_role(router: &MultiServiceRouter, case: &str, policy: &str) {
    let (status, body) = iam(
        router,
        AMBIENT_KEY,
        format!(
            "Action=CreateRole&RoleName={ROLE_NAME}&AssumeRolePolicyDocument={}",
            form_encode(TRUST_POLICY)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole expected 200, got {status}: {body}"
    );
    put_role_policy(router, case, policy).await;
}

/// Embed `policy` in the role as its only inline document — also how a row WIDENS the role.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, policy: &str) {
    let (status, body) = iam(
        router,
        AMBIENT_KEY,
        format!(
            "Action=PutRolePolicy&RoleName={ROLE_NAME}&PolicyName=inline&PolicyDocument={}",
            form_encode(policy)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy expected 200, got {status}: {body}"
    );
}

/// Create a customer managed policy holding `document`, attached to nothing, and answer its
/// ARN: a managed SESSION policy is named by ARN at mint time, which is a different thing
/// from attaching it to the role.
async fn given_managed_policy(
    router: &MultiServiceRouter,
    case: &str,
    policy_name: &str,
    document: &str,
) -> String {
    let (status, body) = iam(
        router,
        AMBIENT_KEY,
        format!(
            "Action=CreatePolicy&PolicyName={policy_name}&PolicyDocument={}",
            form_encode(document)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePolicy expected 200, got {status}: {body}"
    );
    opt_element(&body, "Arn").unwrap_or_else(|| panic!("case {case:?}: no <Arn> in {body}"))
}

/// What one `AssumeRole` reported: the session's access key id, the session ARN a refusal
/// names, and the `PackedPolicySize` it measured the session state at.
struct Minted {
    access_key_id: String,
    session_arn: String,
    packed_policy_size: i64,
}

/// Assume the role under `session_name`, passing `inline` as the inline session policy and
/// `managed` as the managed session policy ARNs.
async fn assume_role(
    router: &MultiServiceRouter,
    case: &str,
    session_name: &str,
    inline: Option<&str>,
    managed: &[String],
) -> Minted {
    let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}");
    let mut body = format!(
        "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={session_name}",
        form_encode(&role_arn)
    );
    if let Some(policy) = inline {
        body.push_str(&format!("&Policy={}", form_encode(policy)));
    }
    for (index, arn) in managed.iter().enumerate() {
        body.push_str(&format!(
            "&PolicyArns.member.{}.arn={}",
            index + 1,
            form_encode(arn)
        ));
    }
    let (status, body) = query(router, AMBIENT_KEY, "sts", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole expected 200, got {status}: {body}"
    );
    Minted {
        access_key_id: opt_element(&body, "AccessKeyId")
            .unwrap_or_else(|| panic!("case {case:?}: no <AccessKeyId> in {body}")),
        session_arn: opt_element(&body, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: no <Arn> in {body}")),
        packed_policy_size: opt_element(&body, "PackedPolicySize")
            .unwrap_or_else(|| panic!("case {case:?}: no <PackedPolicySize> in {body}"))
            .parse()
            .unwrap_or_else(|why| {
                panic!("case {case:?}: PackedPolicySize is not an integer: {why}")
            }),
    }
}

/// Which side of the intersection withheld the grant, and so which diagnosis the refusal
/// carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Both sides name the action, so the call is served.
    Served,
    /// The session set names it and the ROLE does not.
    RefusedByTheRole,
    /// The role names it and no document of the SET does.
    RefusedByTheSet,
}

/// One row: the call the scoped session makes, the action it is authorized under, the
/// resource a refusal names, and which side withheld it.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    action: &'static str,
    resource: String,
    expect: Expect,
}

/// Assert an S3 answer is the refusal AWS words for `action` on `resource`, closed by the
/// diagnosis of the side that withheld the grant.
fn assert_refused(
    case: &str,
    status: u16,
    text: &str,
    session_arn: &str,
    action: &str,
    resource: &str,
    diagnosis: &str,
) {
    assert_eq!(
        status, 403,
        "case {case:?}: an authorization refusal is HTTP 403, got {status}: {text}"
    );
    let code = opt_element(text, "Code").unwrap_or_default();
    assert_eq!(
        code, "AccessDenied",
        "case {case:?}: expected AccessDenied, got {code:?}: {text}"
    );
    let expected = format!(
        "User: {session_arn} is not authorized to perform: {action} on resource: \
         \"{resource}\" because {diagnosis}"
    );
    assert!(
        text.contains(&expected),
        "case {case:?}: expected the message {expected:?}, got {text}"
    );
}

/// RULE: the session policy documents one mint passes are unioned into one set that
/// intersects the role's own policy, and each refusal names the side that withheld the grant.
#[tokio::test]
async fn a_managed_and_an_inline_session_policy_union_and_then_cap_the_role() {
    let router = build_router();
    let case = "a session minted with both a managed and an inline session policy";

    // The fixtures, written as the account: the bucket, one object to read, and the bucket's
    // tag set, so the control row that widens the role has something to read back.
    let (status, text) = s3(&router, AMBIENT_KEY, "PUT", &format!("/{BUCKET}"), b"").await;
    assert!(
        (200..300).contains(&status),
        "the bucket must be created first, got {status}: {text}"
    );
    let (status, text) = s3(
        &router,
        AMBIENT_KEY,
        "PUT",
        &format!("/{BUCKET}/{OBJECT_KEY}"),
        BODY,
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "the object must be stored first, got {status}: {text}"
    );
    let tagging = "<Tagging><TagSet><Tag><Key>stage</Key><Value>test</Value></Tag></TagSet>\
                   </Tagging>";
    let (status, text) = s3(
        &router,
        AMBIENT_KEY,
        "PUT",
        &format!("/{BUCKET}?tagging"),
        tagging.as_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "tagging the bucket must succeed, got {status}: {text}"
    );

    // The role names the listing, the object read and the object write and NOT the tagging
    // read; the managed session policy names the listing, the tagging read and the object
    // read; the inline session policy names the tagging read and the object read. Each call
    // below therefore sits in a different cell of the intersection.
    given_role(&router, case, &allowing(&[LIST_BUCKET, GET_OBJECT, PUT_OBJECT])).await;
    let managed = given_managed_policy(
        &router,
        case,
        "SessionSetManaged",
        &allowing(&[LIST_BUCKET, GET_BUCKET_TAGGING, GET_OBJECT]),
    )
    .await;
    let inline = allowing(&[GET_BUCKET_TAGGING, GET_OBJECT]);

    let scoped = assume_role(
        &router,
        case,
        "set",
        Some(&inline),
        std::slice::from_ref(&managed),
    )
    .await;
    assert!(
        (1..=100).contains(&scoped.packed_policy_size),
        "case {case:?}: PackedPolicySize is a percentage of the one packed budget, so a mint \
         that passed session policies reports it inside 1..100, got {}",
        scoped.packed_policy_size
    );

    let bucket_arn = format!("arn:aws:s3:::{BUCKET}");
    let object_arn = format!("arn:aws:s3:::{BUCKET}/{OBJECT_KEY}");
    let rows = vec![
        Row {
            case: "the object read, which both session documents and the role allow",
            method: "GET",
            uri: format!("/{BUCKET}/{OBJECT_KEY}"),
            action: GET_OBJECT,
            resource: object_arn.clone(),
            expect: Expect::Served,
        },
        Row {
            case: "the listing, which only the managed session document allows",
            method: "GET",
            uri: format!("/{BUCKET}?list-type=2"),
            action: LIST_BUCKET,
            resource: bucket_arn.clone(),
            expect: Expect::Served,
        },
        Row {
            case: "the tagging read, which both session documents allow and the role does not",
            method: "GET",
            uri: format!("/{BUCKET}?tagging"),
            action: GET_BUCKET_TAGGING,
            resource: bucket_arn.clone(),
            expect: Expect::RefusedByTheRole,
        },
        Row {
            case: "the object write, which the role allows and no session document does",
            method: "PUT",
            uri: format!("/{BUCKET}/{OBJECT_KEY}"),
            action: PUT_OBJECT,
            resource: object_arn.clone(),
            expect: Expect::RefusedByTheSet,
        },
    ];

    for Row {
        case,
        method,
        uri,
        action,
        resource,
        expect,
    } in rows
    {
        let (status, text) = s3(&router, &scoped.access_key_id, method, &uri, BODY).await;
        match expect {
            Expect::Served => assert!(
                (200..300).contains(&status),
                "case {case:?}: the union of the session documents must leave this call to the \
                 session, got {status}: {text}"
            ),
            Expect::RefusedByTheRole => assert_refused(
                case,
                status,
                &text,
                &scoped.session_arn,
                action,
                &resource,
                &format!("no identity-based policy allows the {action} action"),
            ),
            Expect::RefusedByTheSet => assert_refused(
                case,
                status,
                &text,
                &scoped.session_arn,
                action,
                &resource,
                &format!("no session policy allows the {action} action"),
            ),
        }
    }

    // Control 1: the same role with NO session policy is capped by nothing, so the write the
    // set withheld is served — the refusal above was the SET's and not the role's. And the
    // packed size grows with the documents passed.
    let unscoped = assume_role(
        &router,
        "the same role with no session policy",
        "plain",
        None,
        &[],
    )
    .await;
    let (status, text) = s3(
        &router,
        &unscoped.access_key_id,
        "PUT",
        &format!("/{BUCKET}/{OBJECT_KEY}"),
        BODY,
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "a session the mint scoped with nothing must be allowed the write, got {status}: {text}"
    );
    assert!(
        unscoped.packed_policy_size < scoped.packed_policy_size,
        "PackedPolicySize must grow with the documents passed: a mint passing none reported \
         {} and one passing two reported {}",
        unscoped.packed_policy_size,
        scoped.packed_policy_size
    );

    // Control 2: widening the ROLE to name the tagging read makes the call the session set
    // already allowed succeed — the refusal above was the role's and not the set's.
    put_role_policy(
        &router,
        "the widened role",
        &allowing(&[LIST_BUCKET, GET_OBJECT, PUT_OBJECT, GET_BUCKET_TAGGING]),
    )
    .await;
    let widened = assume_role(
        &router,
        "the widened role under the same session policies",
        "widened",
        Some(&inline),
        std::slice::from_ref(&managed),
    )
    .await;
    let (status, text) = s3(
        &router,
        &widened.access_key_id,
        "GET",
        &format!("/{BUCKET}?tagging"),
        b"",
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "the widened role must make the tagging read the session set allows succeed, got \
         {status}: {text}"
    );
}
