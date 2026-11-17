// SPDX-License-Identifier: CC0-1.0

//! WHICH requests an S3 ACCESS POINT's policy decides, and over which resource — the data
//! path a caller reaches an access point by, driven over every operation that path serves.
//!
//! The rule under test: **an access point is a named data path onto one bucket, with a policy
//! of its own that is evaluated on the requests made THROUGH the access point and on no
//! others, whatever operation they are. The decision is taken over the resource the access
//! point names for what the request addressed — `<access-point-arn>/object/<key>` for an
//! object-level action and the access point's own ARN for a bucket-level one — so an explicit
//! `Deny` of that action there refuses the request with HTTP 403 `AccessDenied`, naming that
//! resource, while the same call addressed to the bucket BY NAME is unaffected; removing the
//! document with `DeleteAccessPointPolicy` lets the same call through the access point
//! again.**
//!
//! "Access points are named network endpoints that are attached to buckets that you can use
//! to perform S3 object operations", "access point policies … are evaluated in addition to
//! the bucket policy", and "for a bucket-level action the resource is the access point ARN"
//! while an object-level one names `<access-point-arn>/object/<key>` (Amazon S3 User Guide,
//! *Managing data access with Amazon S3 access points* and *Access point policy examples*);
//! "an explicit deny in any policy overrides any allows" (IAM User Guide, *Policy evaluation
//! logic*).
//!
//! Four crates have to agree for that to hold, so the suite lives here: S3 Control creates
//! the access point and holds its policy through its own API, S3 resolves the address and
//! takes the decision, IAM holds the role the calls are granted by, and STS mints the session
//! the calls are signed as — which is the identity an `aws:PrincipalArn` condition selects.
//!
//! The rows span what the rule generalises over rather than the one call a report cites:
//!
//! * the OPERATION, and with it the action the statement denies and the resource the decision
//!   is taken over — the object read (`s3:GetObject`), the HEAD of the same object, the write
//!   (`s3:PutObject`), the delete (`s3:DeleteObject`) and the LISTING of the access point
//!   itself (`s3:ListBucket`), which is the bucket-level row where the resource is the access
//!   point's own ARN;
//! * the ADDRESS the access point is reached by — its ARN as the request's bucket label,
//!   which is what a client puts in the `Bucket` field, and the host every AWS SDK rewrites
//!   that ARN into (`<name>-<account-id>.s3-accesspoint.<region>.amazonaws.com`).
//!
//! Each row asserts all three legs in order: the call through the access point is refused,
//! the same call addressed to the bucket is served, and once the document is deleted the
//! refused call is served through the access point too — and for the write and the delete,
//! that it really acted on the bucket the access point fronts.
//!
//! The suite `core/router/tests/all/s3_access_point_policy_denial.rs` drives the same rule
//! for a Lambda function's own SDK calls; the S3 crate's `access_point_data_plane` suite pins
//! it over the policy shapes and the third address form (the alias).

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request here is signed for and every ARN below carries.
const REGION: &str = "us-east-1";

/// The account this emulator serves: the account the access point belongs to, which its ARN
/// carries and which the S3 Control calls name in `x-amz-account-id`.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The access-key id standing for the account's own published credential, which writes every
/// fixture: no identity-based document bounds it.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// `iam`'s and `sts`' own awsQuery API versions, the `Version` each setup call carries.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The XML namespace S3 Control's model declares.
const S3CONTROL_XMLNS: &str = "http://awss3control.amazonaws.com/doc/2018-08-20/";

/// The bucket the access point fronts, the access point over it, the role every call is
/// signed as a session of, and that session's name.
const BUCKET: &str = "ap-action-scope-bucket";
const ACCESS_POINT: &str = "ap-action-scope";
const ROLE: &str = "ap-action-scope-role";
const SESSION_NAME: &str = "action-scope";

/// The bytes every fixture object holds, and the bytes a write through the access point
/// stores instead.
const BODY: &[u8] = b"the object behind the access point";
const WRITTEN: &[u8] = b"written through the access point";

/// The code and status S3 answers an authorization refusal with (Amazon S3 "Error
/// responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The reason clause IAM publishes for a request a resource-based document explicitly denies.
const RESOURCE_DENY: &str = "with an explicit deny in a resource-based policy";

/// The four services that have to agree about the decision, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope, which
    // is where a service reads the Region and the caller's access key id.
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
    router.register_service(
        service_com_amazonaws_s3control_awss3controlservicev20180820::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key a service reads the caller's identity from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One answer: the status and the body as text.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The error code of a refusal, or the empty string for an answer carrying none.
    fn code(&self) -> String {
        opt_element(&self.body, "Code").unwrap_or_default()
    }

    /// The refusal's sentence, with the serializer's XML escaping undone — the resource it
    /// names travels in quotes, which the wire carries as `&quot;`.
    fn message(&self) -> String {
        elements(&self.body, "Message").into_iter().next().unwrap_or_default()
    }
}

/// One request to S3 (or, by its path, to S3 Control — both sign as `s3`), sent to `host` and
/// signed with `access_key_id`.
async fn s3(
    router: &MultiServiceRouter,
    access_key_id: &str,
    host: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(access_key_id, "s3"))
        .header("x-amz-account-id", ACCOUNT)
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    Answer {
        status,
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// The REGION's path-style S3 endpoint, whose own host names no bucket.
fn path_style_host() -> String {
    format!("s3.{REGION}.amazonaws.com")
}

/// One awsQuery call to IAM or STS, as the account's own credential.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, signing_name))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// The ARN the account holds the role under, which is the value `aws:PrincipalArn` answers
/// for a session of it.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}")
}

/// The credential one session was minted under: the access key id the rows sign with, and the
/// assumed-role session ARN `sts:AssumeRole` reports for it — the principal a refusal blames.
struct Session {
    access_key_id: String,
    arn: String,
}

/// Create the bucket the access point fronts.
async fn create_bucket(router: &MultiServiceRouter) {
    let created = s3(
        router,
        ACCOUNT_KEY,
        &path_style_host(),
        "PUT",
        &format!("/{BUCKET}"),
        b"",
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {}",
        created.status, created.body
    );
}

/// Store `key` with the bytes every row starts from, as the account.
async fn write_fixture(router: &MultiServiceRouter, key: &str) {
    let written = s3(
        router,
        ACCOUNT_KEY,
        &path_style_host(),
        "PUT",
        &format!("/{BUCKET}/{key}"),
        BODY,
    )
    .await;
    assert_eq!(
        written.status, 200,
        "setup: PutObject({key:?}) must succeed, got {} with {}",
        written.status, written.body
    );
}

/// `PUT /v20180820/accesspoint/<name>` — S3 Control's `CreateAccessPoint`, answering the ARN
/// it minted.
async fn create_access_point(router: &MultiServiceRouter) -> String {
    let created = s3(
        router,
        ACCOUNT_KEY,
        &path_style_host(),
        "PUT",
        &format!("/v20180820/accesspoint/{ACCESS_POINT}"),
        format!(
            "<CreateAccessPointRequest xmlns=\"{S3CONTROL_XMLNS}\">\
             <Bucket>{BUCKET}</Bucket></CreateAccessPointRequest>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateAccessPoint({ACCESS_POINT:?}) must succeed, got {} with {}",
        created.status, created.body
    );
    let arn = element(&created.body, "AccessPointArn");
    assert!(
        arn.ends_with(&format!(":accesspoint/{ACCESS_POINT}")),
        "setup: CreateAccessPoint must answer the access point's own ARN, got {arn:?}"
    );
    arn
}

/// `PUT /v20180820/accesspoint/<name>/policy` — S3 Control's `PutAccessPointPolicy`.
async fn put_access_point_policy(router: &MultiServiceRouter, case: &str, document: &str) {
    let stored = s3(
        router,
        ACCOUNT_KEY,
        &path_style_host(),
        "PUT",
        &format!("/v20180820/accesspoint/{ACCESS_POINT}/policy"),
        format!(
            "<PutAccessPointPolicyRequest xmlns=\"{S3CONTROL_XMLNS}\">\
             <Policy>{document}</Policy></PutAccessPointPolicyRequest>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        stored.status, 204,
        "case {case:?}: PutAccessPointPolicy must succeed, got {} with {}",
        stored.status, stored.body
    );
}

/// `DELETE /v20180820/accesspoint/<name>/policy` — S3 Control's `DeleteAccessPointPolicy`.
async fn delete_access_point_policy(router: &MultiServiceRouter, case: &str) {
    let removed = s3(
        router,
        ACCOUNT_KEY,
        &path_style_host(),
        "DELETE",
        &format!("/v20180820/accesspoint/{ACCESS_POINT}/policy"),
        b"",
    )
    .await;
    assert_eq!(
        removed.status, 204,
        "case {case:?}: DeleteAccessPointPolicy must succeed, got {} with {}",
        removed.status, removed.body
    );
}

/// Hold the role, grant it every action the rows ask for on BOTH the bucket's resources and
/// the access point's, and mint the session the rows call as.
///
/// The grant covers both arms of every row, so a refusal below can only come from the
/// document the row installed on the access point.
async fn granted_session(router: &MultiServiceRouter, access_point_arn: &str) -> Session {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &AssumeRolePolicyDocument={}",
            form_encode(&trust)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({ROLE:?}) must succeed, got {status} with {created}"
    );

    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:ListBucket"],
            "Resource": [
                format!("arn:aws:s3:::{BUCKET}"),
                format!("arn:aws:s3:::{BUCKET}/*"),
                access_point_arn.to_string(),
                format!("{access_point_arn}/object/*"),
            ],
        }],
    })
    .to_string();
    let (status, attached) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version={IAM_API_VERSION}&RoleName={ROLE}&PolicyName=inline\
             &PolicyDocument={}",
            form_encode(&document)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({ROLE:?}) must succeed, got {status} with {attached}"
    );

    let (status, assumed) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={}&RoleSessionName={SESSION_NAME}",
            form_encode(&role_arn())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole({ROLE:?}) must succeed, got {status} with {assumed}"
    );
    Session {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// One operation an access point's data plane serves, which is what decides the action the
/// statement denies and the resource the decision is taken over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    /// A read of one object through the access point.
    Read,
    /// A HEAD of that same object.
    Head,
    /// A write of one object through the access point.
    Write,
    /// A delete of one object through the access point.
    Remove,
    /// A listing of the access point itself — the BUCKET-level row.
    List,
}

impl Call {
    /// The IAM action this call stands on: "you must have the `s3:PutObject` permission" to
    /// write an object, "`s3:DeleteObject`" to delete one, `s3:GetObject` for the read and the
    /// HEAD of an object, and "READ access to the bucket" — `s3:ListBucket` — to list its
    /// objects (each operation's own documented general purpose bucket permissions).
    fn action(self) -> &'static str {
        match self {
            Self::Read | Self::Head => "s3:GetObject",
            Self::Write => "s3:PutObject",
            Self::Remove => "s3:DeleteObject",
            Self::List => "s3:ListBucket",
        }
    }

    /// The HTTP method the call is made with.
    fn method(self) -> &'static str {
        match self {
            Self::Read | Self::List => "GET",
            Self::Head => "HEAD",
            Self::Write => "PUT",
            Self::Remove => "DELETE",
        }
    }

    /// The status the call answers when it is SERVED: a delete reports no content, everything
    /// else answers 200.
    fn served(self) -> u16 {
        match self {
            Self::Remove => 204,
            _ => 200,
        }
    }

    /// Whether the decision is taken over the access point's OBJECT resource
    /// (`<arn>/object/<key>`) rather than over the access point itself, which is what tells an
    /// object-level action from a bucket-level one.
    fn names_an_object(self) -> bool {
        self != Self::List
    }

    /// The payload the call carries: the bytes a write stores, nothing for every other.
    fn payload(self) -> &'static [u8] {
        match self {
            Self::Write => WRITTEN,
            _ => b"",
        }
    }

    /// The slug this call's own fixture key is held under, so every row addresses its own
    /// object and no row depends on what another one left behind.
    fn slug(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Head => "head",
            Self::Write => "write",
            Self::Remove => "remove",
            Self::List => "list",
        }
    }

    /// How a row names itself in every assertion.
    fn case(self) -> &'static str {
        match self {
            Self::Read => "a read of an object",
            Self::Head => "a HEAD of an object",
            Self::Write => "a write of an object",
            Self::Remove => "a delete of an object",
            Self::List => "a listing of the access point",
        }
    }
}

/// One ADDRESS an access point is reached by.
#[derive(Clone, Copy, Debug)]
enum Addressed {
    /// Its ARN as the request's bucket label — what a client puts in the `Bucket` field, sent
    /// to the Region's path-style endpoint.
    Arn,
    /// The host an AWS SDK rewrites that ARN into:
    /// `<name>-<account-id>.s3-accesspoint.<region>.amazonaws.com`, whose path is the key
    /// alone.
    EndpointHost,
}

impl Addressed {
    /// How a row names this address form.
    fn case(self) -> &'static str {
        match self {
            Self::Arn => "the access point ARN as the bucket label",
            Self::EndpointHost => "the access point's own endpoint host",
        }
    }

    /// The slug this address form's own fixture keys are held under, so a row addressed one
    /// way never reads what a row addressed the other way left behind.
    fn slug(self) -> &'static str {
        match self {
            Self::Arn => "by-arn",
            Self::EndpointHost => "by-host",
        }
    }

    /// The host and path one `call` about `key` arrives on through this address form.
    fn request(self, access_point_arn: &str, call: Call, key: &str) -> (String, String) {
        match self {
            Self::Arn => {
                let label = form_encode(access_point_arn);
                let path = match call {
                    Call::List => format!("/{label}?list-type=2"),
                    _ => format!("/{label}/{key}"),
                };
                (path_style_host(), path)
            }
            Self::EndpointHost => {
                let name = access_point_arn
                    .rsplit('/')
                    .next()
                    .unwrap_or(access_point_arn);
                let host = format!("{name}-{ACCOUNT}.s3-accesspoint.{REGION}.amazonaws.com");
                let path = match call {
                    Call::List => "/?list-type=2".to_string(),
                    _ => format!("/{key}"),
                };
                (host, path)
            }
        }
    }
}

/// The host and path the same `call` arrives on when it addresses the bucket BY NAME — the
/// path the access point's document does not govern.
fn addressed_to_the_bucket(call: Call, key: &str) -> (String, String) {
    let path = match call {
        Call::List => format!("/{BUCKET}?list-type=2"),
        _ => format!("/{BUCKET}/{key}"),
    };
    (path_style_host(), path)
}

/// The access point policy one row installs: an explicit `Deny` of that row's action on the
/// resource the access point names, selected by the calling role through `aws:PrincipalArn`.
fn denying_document(call: Call, access_point_arn: &str) -> String {
    let resource = if call.names_an_object() {
        format!("{access_point_arn}/object/*")
    } else {
        access_point_arn.to_string()
    };
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": "*",
            "Action": call.action(),
            "Resource": resource,
            "Condition": {"ArnEquals": {"aws:PrincipalArn": role_arn()}},
        }],
    })
    .to_string()
}

/// The resource a refusal of `call` names: the access point's own resource for the object
/// (`<arn>/object/<key>`), and the access point itself for the bucket-level listing.
fn refused_resource(call: Call, access_point_arn: &str, key: &str) -> String {
    if call.names_an_object() {
        format!("{access_point_arn}/object/{key}")
    } else {
        access_point_arn.to_string()
    }
}

/// The sentence S3 refuses a call an access point policy explicitly denies with: who was
/// refused, the action, the resource the decision was taken over, and the clause naming the
/// document that decided it. Written out here rather than built from the service's own helper
/// so the wording has something to be wrong about.
fn explicit_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: \
         \"{resource}\" {RESOURCE_DENY}"
    )
}

/// Read `key` back through the BUCKET's own address, as the account — the reading that says
/// what a call through the access point really did to the bucket it fronts.
async fn read_back(router: &MultiServiceRouter, key: &str) -> Answer {
    s3(
        router,
        ACCOUNT_KEY,
        &path_style_host(),
        "GET",
        &format!("/{BUCKET}/{key}"),
        b"",
    )
    .await
}

/// The rule, over every operation the access point's data plane serves and both addresses it
/// is reached by.
#[tokio::test]
async fn an_access_point_policy_decides_every_call_made_through_that_access_point() {
    let router = build_router();
    create_bucket(&router).await;
    let access_point_arn = create_access_point(&router).await;
    let session = granted_session(&router, &access_point_arn).await;

    for addressed in [Addressed::Arn, Addressed::EndpointHost] {
        for call in [Call::Read, Call::Head, Call::Write, Call::Remove, Call::List] {
            let case = format!("{} through {}", call.case(), addressed.case());
            let key = format!("{}/{}/report.txt", addressed.slug(), call.slug());
            write_fixture(&router, &key).await;
            put_access_point_policy(&router, &case, &denying_document(call, &access_point_arn))
                .await;
            let (host, path) = addressed.request(&access_point_arn, call, &key);

            // The call THROUGH the access point: refused for the document the row installed,
            // over the resource the access point names.
            let refused = s3(
                &router,
                &session.access_key_id,
                &host,
                call.method(),
                &path,
                call.payload(),
            )
            .await;
            assert_eq!(
                refused.status, ACCESS_DENIED_STATUS,
                "case {case:?}: the call through the access point must be refused {ACCESS_DENIED_STATUS}, got {} with {}",
                refused.status, refused.body
            );
            // A HEAD carries no body at all, so there is no code or sentence to read on that
            // row — its status is the whole answer.
            if call != Call::Head {
                assert_eq!(
                    refused.code(),
                    ACCESS_DENIED,
                    "case {case:?}: the refusal must carry the code {ACCESS_DENIED}, got {}",
                    refused.body
                );
                assert_eq!(
                    refused.message(),
                    explicit_deny_message(
                        &session.arn,
                        call.action(),
                        &refused_resource(call, &access_point_arn, &key)
                    ),
                    "case {case:?}: the refusal must name the principal, the action, the \
                     access point's own resource and the document that decided it, got {}",
                    refused.body
                );
            }

            // The SAME call addressed to the bucket by name, which that document does not
            // govern at all.
            let (direct_host, direct_path) = addressed_to_the_bucket(call, &key);
            let direct = s3(
                &router,
                &session.access_key_id,
                &direct_host,
                call.method(),
                &direct_path,
                call.payload(),
            )
            .await;
            assert_eq!(
                direct.status,
                call.served(),
                "case {case:?}: the same call addressed to the bucket must be served {}, got {} with {}",
                call.served(),
                direct.status,
                direct.body
            );

            // The document's REMOVAL is live: with the fixture back as it started, the call
            // the access point had been refusing is served through it.
            write_fixture(&router, &key).await;
            delete_access_point_policy(&router, &case).await;
            let served = s3(
                &router,
                &session.access_key_id,
                &host,
                call.method(),
                &path,
                call.payload(),
            )
            .await;
            assert_eq!(
                served.status,
                call.served(),
                "case {case:?}: once the document is removed, the call through the access \
                 point must be served {}, got {} with {}",
                call.served(),
                served.status,
                served.body
            );

            // And what that served call did is the bucket's: the read and the listing report
            // what the bucket holds, the write stored its bytes there and the delete took the
            // object away.
            match call {
                Call::Read => assert_eq!(
                    served.body.as_bytes(),
                    BODY,
                    "case {case:?}: the read through the access point must serve the object's \
                     bytes, got {}",
                    served.body
                ),
                Call::List => assert!(
                    served.body.contains(&key),
                    "case {case:?}: the listing through the access point must report the key \
                     {key:?}, got {}",
                    served.body
                ),
                Call::Write => {
                    let back = read_back(&router, &key).await;
                    assert_eq!(
                        (back.status, back.body.as_bytes()),
                        (200, WRITTEN),
                        "case {case:?}: the bucket's own address must read the bytes the write \
                         through the access point stored, got {} with {}",
                        back.status,
                        back.body
                    );
                }
                Call::Remove => {
                    let back = read_back(&router, &key).await;
                    assert_eq!(
                        (back.status, back.code().as_str()),
                        (404, "NoSuchKey"),
                        "case {case:?}: the bucket must hold nothing at the key the delete \
                         through the access point took away, got {} with {}",
                        back.status,
                        back.body
                    );
                }
                Call::Head => {}
            }
        }
    }
}
