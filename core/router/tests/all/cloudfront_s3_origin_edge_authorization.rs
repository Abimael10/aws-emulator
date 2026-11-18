// SPDX-License-Identifier: CC0-1.0

//! WHO a CloudFront origin fetch of a private Amazon S3 bucket is made as, WHO decides it, and
//! WHICH object the root of a distribution asks for — across the two services on one engine.
//!
//! Two rules are under test.
//!
//! * An origin fetch under an origin access mechanism is made AS the principal that mechanism
//!   names — the `cloudfront.amazonaws.com` service principal for an origin access control, the
//!   identity itself for a legacy origin access identity — acting for the distribution it is
//!   made for, and the ORIGIN decides whether it serves it, from its own resource policy and
//!   for that principal. So a fetch the bucket's document grants is answered with the object,
//!   one it does not grant is refused BY THE BUCKET in its own vocabulary, and a key the bucket
//!   does not hold is answered by what that principal may learn about the bucket's contents at
//!   all: `403 AccessDenied` while it holds no `s3:ListBucket`, and `404 NoSuchKey` once it
//!   does.
//! * `DistributionConfig$DefaultRootObject` is "the object that you want CloudFront to request
//!   from your origin … when a viewer requests the root URL for your distribution", and "a
//!   default root object avoids exposing the contents of your distribution": the root URL is an
//!   origin request for THAT object, every path below the root is asked for as the viewer
//!   spelled it, and a distribution naming no root object exposes none of the bucket at its
//!   root either.
//!
//! Only a cross-service suite can state either: the distribution, the access mechanism and the
//! root object are CloudFront's records, the document that grants the fetch and the object it
//! guards are Amazon S3's, and both rules are about what the one decides for the other.
//!
//! Every call here goes through ONE router with request authentication OFF, which is how the
//! assembled emulator serves a viewer by default. That is the point: what must make the bucket
//! refuse an ungranted fetch is the IDENTITY the fetch carries, resolved from the credential
//! CloudFront minted for that one call, and never the absence of a credential — a request
//! carrying none speaks for the account, whose own store serves a private bucket in full. The
//! sibling `cloudfront_oac_s3_origin_edge` and `cloudfront_oai_s3_origin_edge` suites drive the
//! same mechanisms through an authenticating viewer plane; this one is the half that cannot be
//! passed by a front door refusing an unsigned request.
//!
//! The rows span what each rule generalises over: the mechanism (an origin access control, a
//! legacy identity named by either spelling AWS publishes for it), the document (a grant to
//! this distribution, a grant naming another distribution's ARN, no policy at all, a grant that
//! adds the bucket listing), whether the key exists, and the form of the root object (a name at
//! the bucket root, a nested one, none) against the paths a viewer asks for (the root itself, an
//! object below it, a directory-style path).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_oac_s3_origin_edge::{
    assert_deployed, call, create_bucket, create_origin_access_control, emulator, origin_domain,
    put_bucket_policy, put_object, view, Answer, API_VERSION, CLOUDFRONT_HOST,
    CLOUDFRONT_PRINCIPAL,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row's answer is the origin's own rather
/// than another row's held at the edge.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The code Amazon S3 refuses a read nothing granted with, and the message it words it in
/// (Amazon S3 "Error responses").
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// The code Amazon S3 answers a read of a key it does not hold with, to a caller that holds
/// the bucket's listing.
const NO_SUCH_KEY: &str = "NoSuchKey";

/// What the edge reports as the outcome of an answer the ORIGIN produced for this request.
const MISS: &str = "Miss from cloudfront";

/// How long the suite waits for the clock to deploy a distribution, and how often it looks.
/// The wait is on the CONDITION — the status CloudFront reports — with a deadline far past the
/// propagation any configuration is owed.
const POLL: Duration = Duration::from_millis(25);
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// Poll `GetDistribution` until CloudFront first reports `id` deployed, which is the state a
/// viewer request is served in.
async fn awaited_deployment(router: &MultiServiceRouter, case: &str, id: &str) {
    let polls = POLL_DEADLINE.as_millis() / POLL.as_millis().max(1);
    for _ in 0..polls {
        let read = call(
            router,
            "cloudfront",
            CLOUDFRONT_HOST,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            &[],
            Bytes::new(),
        )
        .await;
        if read.element("Status") == "Deployed" {
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
    panic!(
        "case {case:?}: CloudFront never reported the distribution {id} deployed within \
         {POLL_DEADLINE:?}"
    );
}

// ---------------------------------------------------------------------------
// The documents a bucket carries
// ---------------------------------------------------------------------------

/// One statement of a bucket policy: the principal, under the kind a bucket policy names it by
/// (`Service` for a service principal, `AWS` for an IAM principal, `CanonicalUser` for a
/// canonical user id), the actions it grants, the resource they are granted on and the
/// `aws:SourceArn` the grant is scoped by, when it is scoped at all.
struct Statement<'s> {
    kind: &'s str,
    principal: &'s str,
    actions: &'s [&'s str],
    resource: String,
    source_arn: Option<&'s str>,
}

impl Statement<'_> {
    fn render(&self) -> String {
        let actions = self
            .actions
            .iter()
            .map(|action| format!("\"{action}\""))
            .collect::<Vec<String>>()
            .join(",");
        let condition = self
            .source_arn
            .map(|arn| {
                format!(",\"Condition\":{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}")
            })
            .unwrap_or_default();
        format!(
            "{{\"Effect\":\"Allow\",\"Principal\":{{\"{kind}\":\"{principal}\"}},\
             \"Action\":[{actions}],\"Resource\":\"{resource}\"{condition}}}",
            kind = self.kind,
            principal = self.principal,
            resource = self.resource,
        )
    }
}

/// A bucket policy carrying `statements`.
fn document(statements: &[Statement<'_>]) -> String {
    let statements = statements
        .iter()
        .map(Statement::render)
        .collect::<Vec<String>>()
        .join(",");
    format!("{{\"Version\":\"2012-10-17\",\"Statement\":[{statements}]}}")
}

/// The IAM principal a legacy origin access identity is named by in a bucket policy, as the
/// Amazon S3 User Guide writes it.
fn identity_principal(id: &str) -> String {
    format!("arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity {id}")
}

// ---------------------------------------------------------------------------
// Staging the distribution
// ---------------------------------------------------------------------------

/// WHICH origin access mechanism fronts a row's origin.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Access {
    /// An origin access control, whose id the origin carries.
    Control(String),
    /// A legacy origin access identity, whose documented reference the `S3OriginConfig`
    /// carries.
    Identity(String),
}

/// `cloudfront:CreateCloudFrontOriginAccessIdentity`, answering the identity's
/// `(Id, S3CanonicalUserId)`.
async fn create_origin_access_identity(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
) -> (String, String) {
    let body = format!(
        "<CloudFrontOriginAccessIdentityConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Comment>an identity a private bucket grants</Comment>\
         </CloudFrontOriginAccessIdentityConfig>"
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/origin-access-identity/cloudfront"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateCloudFrontOriginAccessIdentity must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, canonical) = (
        created.element("Id"),
        created.element("S3CanonicalUserId"),
    );
    assert!(
        !id.is_empty() && !canonical.is_empty(),
        "case {case:?}: the created identity reports no Id or S3CanonicalUserId: {:?}",
        created.text()
    );
    (id, canonical)
}

/// `cloudfront:CreateDistribution` over the S3 origin `bucket`, fronted by `access`, with
/// `root_object` as its `DefaultRootObject` when it names one; answers the distribution's
/// `(id, DomainName, ARN)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    access: &Access,
    root_object: Option<&str>,
) -> (String, String, String) {
    let (identity, control) = match access {
        Access::Control(id) => (String::new(), format!("<OriginAccessControlId>{id}</OriginAccessControlId>")),
        Access::Identity(id) => (format!("origin-access-identity/cloudfront/{id}"), String::new()),
    };
    let root = root_object
        .map(|object| format!("<DefaultRootObject>{object}</DefaultRootObject>"))
        .unwrap_or_default();
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         {root}\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id><DomainName>{domain}</DomainName>\
         <S3OriginConfig><OriginAccessIdentity>{identity}</OriginAccessIdentity></S3OriginConfig>\
         {control}\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>{caller_reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        domain = origin_domain(bucket)
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain, arn) = (
        created.element("Id"),
        created.element("DomainName"),
        created.element("ARN"),
    );
    for (member, value) in [("Id", &id), ("DomainName", &domain), ("ARN", &arn)] {
        assert!(
            !value.is_empty(),
            "case {case:?}: the created distribution reports no {member}: {:?}",
            created.text()
        );
    }
    (id, domain, arn)
}

// ---------------------------------------------------------------------------
// WHO the fetch is made as, and WHO decides it
// ---------------------------------------------------------------------------

/// WHICH mechanism a row's origin is fronted by, and — for a legacy identity — which spelling
/// its bucket policy names it by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mechanism {
    /// An origin access control, whose principal is the CloudFront service principal.
    OriginAccessControl,
    /// A legacy origin access identity, granted by its `S3CanonicalUserId`.
    IdentityByCanonicalUser,
    /// The same identity, granted by the IAM principal of CloudFront's own account.
    IdentityByIamPrincipal,
}

/// WHAT the bucket's own document grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The read of every object, to this origin's own principal.
    TheRead,
    /// The read of every object AND the bucket's listing, to that principal.
    TheReadAndTheListing,
    /// The read, scoped by `aws:SourceArn` to another distribution's ARN.
    AnotherDistribution,
    /// No policy at all.
    Nothing,
}

/// WHAT the viewer must be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The object's own bytes, as the origin answered them.
    TheObject,
    /// The bucket's own refusal, which says nothing about whether the key exists.
    AccessDenied,
    /// The bucket's own report that the key does not exist, which only a caller holding the
    /// listing is given.
    NoSuchKey,
}

/// One row: a private bucket holding one object, a distribution over it fronted by one
/// mechanism, the document the bucket carries, the path a viewer asks for — and what the
/// viewer is served.
struct FetchRow {
    case: &'static str,
    /// This row's own bucket, so one row's grant cannot admit another's read.
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    mechanism: Mechanism,
    grant: Grant,
    /// The viewer path, which names the row's key or one the bucket does not hold.
    path: &'static str,
    expected: Expected,
}

const FETCH_ROWS: &[FetchRow] = &[
    FetchRow {
        case: "an origin access control the bucket grants this distribution",
        bucket: "edge-auth-oac-granted",
        key: "assets/app.js",
        body: "console.log('the granted object');",
        mechanism: Mechanism::OriginAccessControl,
        grant: Grant::TheRead,
        path: "/assets/app.js",
        expected: Expected::TheObject,
    },
    FetchRow {
        case: "an origin access control whose grant names another distribution's ARN",
        bucket: "edge-auth-oac-other-arn",
        key: "plain.txt",
        body: "not for this distribution",
        mechanism: Mechanism::OriginAccessControl,
        grant: Grant::AnotherDistribution,
        path: "/plain.txt",
        expected: Expected::AccessDenied,
    },
    FetchRow {
        case: "an origin access control over a bucket carrying no policy at all",
        bucket: "edge-auth-oac-no-policy",
        key: "plain.txt",
        body: "nothing granted",
        mechanism: Mechanism::OriginAccessControl,
        grant: Grant::Nothing,
        path: "/plain.txt",
        expected: Expected::AccessDenied,
    },
    FetchRow {
        case: "a key the bucket does not hold, to a grant of the read alone",
        bucket: "edge-auth-oac-missing-key",
        key: "held.txt",
        body: "the object that is there",
        mechanism: Mechanism::OriginAccessControl,
        grant: Grant::TheRead,
        path: "/tenant/",
        expected: Expected::AccessDenied,
    },
    FetchRow {
        case: "the same missing key once the bucket's listing is granted too",
        bucket: "edge-auth-oac-missing-key-listed",
        key: "held.txt",
        body: "the object that is there",
        mechanism: Mechanism::OriginAccessControl,
        grant: Grant::TheReadAndTheListing,
        path: "/tenant/",
        expected: Expected::NoSuchKey,
    },
    FetchRow {
        case: "a legacy identity the bucket grants by its canonical user id",
        bucket: "edge-auth-oai-canonical",
        key: "site/index.html",
        body: "<p>behind the identity</p>",
        mechanism: Mechanism::IdentityByCanonicalUser,
        grant: Grant::TheRead,
        path: "/site/index.html",
        expected: Expected::TheObject,
    },
    FetchRow {
        case: "a legacy identity the bucket grants by its IAM principal",
        bucket: "edge-auth-oai-iam",
        key: "plain.txt",
        body: "granted to the identity's arn",
        mechanism: Mechanism::IdentityByIamPrincipal,
        grant: Grant::TheRead,
        path: "/plain.txt",
        expected: Expected::TheObject,
    },
    FetchRow {
        case: "a key the bucket does not hold, behind a legacy identity",
        bucket: "edge-auth-oai-missing-key",
        key: "held.txt",
        body: "the object that is there",
        mechanism: Mechanism::IdentityByCanonicalUser,
        grant: Grant::TheRead,
        path: "/tenant/",
        expected: Expected::AccessDenied,
    },
    FetchRow {
        case: "a legacy identity over a bucket carrying no policy at all",
        bucket: "edge-auth-oai-no-policy",
        key: "plain.txt",
        body: "nothing granted to the identity",
        mechanism: Mechanism::IdentityByCanonicalUser,
        grant: Grant::Nothing,
        path: "/plain.txt",
        expected: Expected::AccessDenied,
    },
];

/// What one row left behind for the viewer phase.
struct Prepared {
    case: &'static str,
    body: &'static str,
    path: &'static str,
    expected: Expected,
    domain: String,
    id: String,
}

#[tokio::test]
async fn the_origin_decides_every_fetch_for_the_principal_the_mechanism_names() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // ONE plane, authentication OFF: the viewer presents no credential and neither does any
    // setup call, so nothing a front door does can stand in for the bucket's own decision.
    let router = emulator(&engine, false);

    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, row) in FETCH_ROWS.iter().enumerate() {
        let case = row.case;
        create_bucket(&router, case, row.bucket).await;
        put_object(&router, case, row.bucket, row.key, row.body, &[]).await;
        let (access, principal_kind, principal) = match row.mechanism {
            Mechanism::OriginAccessControl => {
                let control = create_origin_access_control(
                    &router,
                    case,
                    &format!("edge-auth-control-{index}"),
                    "always",
                )
                .await;
                (
                    Access::Control(control),
                    "Service",
                    CLOUDFRONT_PRINCIPAL.to_string(),
                )
            }
            Mechanism::IdentityByCanonicalUser => {
                let (id, canonical) = create_origin_access_identity(
                    &router,
                    case,
                    &format!("edge-auth-identity-{index}"),
                )
                .await;
                (Access::Identity(id), "CanonicalUser", canonical)
            }
            Mechanism::IdentityByIamPrincipal => {
                let (id, _canonical) = create_origin_access_identity(
                    &router,
                    case,
                    &format!("edge-auth-identity-{index}"),
                )
                .await;
                let named = identity_principal(&id);
                (Access::Identity(id), "AWS", named)
            }
        };
        let (id, domain, arn) = create_distribution(
            &router,
            case,
            &format!("edge-auth-reference-{index}"),
            row.bucket,
            &access,
            None,
        )
        .await;
        // The document is written AFTER the distribution exists, because a grant to the service
        // principal is scoped to that distribution's own ARN — the order a client has to follow.
        let objects = format!("arn:aws:s3:::{}/*", row.bucket);
        let whole_bucket = format!("arn:aws:s3:::{}", row.bucket);
        let another = "arn:aws:cloudfront::000000000000:distribution/E2NOTTHISONE";
        let scoped = (row.mechanism == Mechanism::OriginAccessControl).then_some(arn.as_str());
        let statements = match row.grant {
            Grant::TheRead => vec![Statement {
                kind: principal_kind,
                principal: &principal,
                actions: &["s3:GetObject"],
                resource: objects,
                source_arn: scoped,
            }],
            Grant::TheReadAndTheListing => vec![
                Statement {
                    kind: principal_kind,
                    principal: &principal,
                    actions: &["s3:GetObject"],
                    resource: objects,
                    source_arn: scoped,
                },
                Statement {
                    kind: principal_kind,
                    principal: &principal,
                    actions: &["s3:ListBucket"],
                    resource: whole_bucket,
                    source_arn: scoped,
                },
            ],
            Grant::AnotherDistribution => vec![Statement {
                kind: principal_kind,
                principal: &principal,
                actions: &["s3:GetObject"],
                resource: objects,
                source_arn: Some(another),
            }],
            Grant::Nothing => Vec::new(),
        };
        if !statements.is_empty() {
            put_bucket_policy(&router, case, row.bucket, &document(&statements)).await;
        }
        prepared.push(Prepared {
            case,
            body: row.body,
            path: row.path,
            expected: row.expected,
            domain,
            id,
        });
    }

    for row in &prepared {
        awaited_deployment(&router, row.case, &row.id).await;
        let viewed = view(&router, &row.domain, row.path).await;
        assert_served(&viewed, row.case, row.expected, row.body, row.path);
    }
}

/// Assert that `viewed` is what the row expected, naming the case and what came back.
fn assert_served(
    viewed: &Answer,
    case: &str,
    expected: Expected,
    body: &str,
    path: &str,
) {
    match expected {
        Expected::TheObject => {
            assert_eq!(
                (viewed.status, viewed.text().as_str()),
                (200, body),
                "case {case:?}: expected the origin's own bytes through the edge, got {} with \
                 {:?}",
                viewed.status,
                viewed.text()
            );
            assert_eq!(
                viewed.header("x-cache"),
                MISS,
                "case {case:?}: expected the answer to be the origin's, got X-Cache {:?}",
                viewed.header("x-cache")
            );
        }
        Expected::AccessDenied => {
            assert_eq!(
                viewed.status, 403,
                "case {case:?}: expected the bucket's own refusal through the edge, got {} with \
                 {:?}",
                viewed.status,
                viewed.text()
            );
            assert_eq!(
                (
                    viewed.element("Code").as_str(),
                    viewed.element("Message").as_str()
                ),
                (ACCESS_DENIED, ACCESS_DENIED_MESSAGE),
                "case {case:?}: expected the {ACCESS_DENIED} document, got {:?}",
                viewed.text()
            );
            assert!(
                !viewed.text().contains(NO_SUCH_KEY) && !viewed.text().contains(path),
                "case {case:?}: a refusal must say nothing about whether the key exists, got \
                 {:?}",
                viewed.text()
            );
            assert!(
                !viewed.text().contains(body),
                "case {case:?}: a refused read must carry none of the object, got {:?}",
                viewed.text()
            );
        }
        Expected::NoSuchKey => {
            assert_eq!(
                viewed.status, 404,
                "case {case:?}: expected the bucket to report the key absent, got {} with {:?}",
                viewed.status,
                viewed.text()
            );
            assert_eq!(
                (
                    viewed.element("Code").as_str(),
                    viewed.element("Key").as_str()
                ),
                (NO_SUCH_KEY, path.trim_start_matches('/')),
                "case {case:?}: expected {NO_SUCH_KEY} naming the key asked for, got {:?}",
                viewed.text()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// WHICH object the ROOT of a distribution asks for
// ---------------------------------------------------------------------------

/// The objects every bucket below holds: one at the bucket's root, one under a prefix, and one
/// a nested default root object names.
const ROOT_PAGE: (&str, &str) = ("index.html", "<p>the bucket root index</p>");
const NESTED_PAGE: (&str, &str) = ("sub/index.html", "<p>the nested index</p>");
const START_PAGE: (&str, &str) = ("home/start.html", "<p>the nested start page</p>");

/// One root-object configuration, and what each viewer path under it is served.
struct RootCase {
    case: &'static str,
    bucket: &'static str,
    /// `DefaultRootObject`, or `None` for a distribution that names none.
    root_object: Option<&'static str>,
    /// The viewer paths this configuration is asked for: the case, the path, and the object's
    /// bytes the viewer must be served — `None` where the viewer is refused instead, the
    /// origin having been asked for something it grants nobody.
    paths: &'static [(&'static str, &'static str, Option<&'static str>)],
}

const ROOT_CASES: &[RootCase] = &[
    RootCase {
        case: "a distribution naming an object at the bucket root",
        bucket: "edge-root-object-named",
        root_object: Some(ROOT_PAGE.0),
        paths: &[
            ("the root URL", "/", Some(ROOT_PAGE.1)),
            ("the root object by name", "/index.html", Some(ROOT_PAGE.1)),
            ("an object below the root", "/sub/index.html", Some(NESTED_PAGE.1)),
            ("a directory below the root", "/sub/", None),
        ],
    },
    RootCase {
        case: "a distribution naming a nested object",
        bucket: "edge-root-object-nested",
        root_object: Some(START_PAGE.0),
        paths: &[
            ("the root URL", "/", Some(START_PAGE.1)),
            ("the root object by name", "/home/start.html", Some(START_PAGE.1)),
        ],
    },
    RootCase {
        case: "a distribution naming no root object",
        bucket: "edge-root-object-absent",
        root_object: None,
        paths: &[("the root URL", "/", None)],
    },
];

#[tokio::test]
async fn the_distribution_root_asks_the_origin_for_its_default_root_object() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine, false);

    let mut staged: Vec<(&RootCase, String, String)> = Vec::new();
    for (index, case) in ROOT_CASES.iter().enumerate() {
        create_bucket(&router, case.case, case.bucket).await;
        for (key, body) in [ROOT_PAGE, NESTED_PAGE, START_PAGE] {
            put_object(&router, case.case, case.bucket, key, body, &[]).await;
        }
        let control = create_origin_access_control(
            &router,
            case.case,
            &format!("edge-root-control-{index}"),
            "always",
        )
        .await;
        let (id, domain, arn) = create_distribution(
            &router,
            case.case,
            &format!("edge-root-reference-{index}"),
            case.bucket,
            &Access::Control(control),
            case.root_object,
        )
        .await;
        // The READ of every object and nothing else: the bucket's own contents stay unlistable,
        // which is what a root object is for.
        put_bucket_policy(
            &router,
            case.case,
            case.bucket,
            &document(&[Statement {
                kind: "Service",
                principal: CLOUDFRONT_PRINCIPAL,
                actions: &["s3:GetObject"],
                resource: format!("arn:aws:s3:::{}/*", case.bucket),
                source_arn: Some(&arn),
            }]),
        )
        .await;
        staged.push((case, id, domain));
    }

    for (root, id, domain) in &staged {
        awaited_deployment(&router, root.case, id).await;
        for (path_case, path, expected) in root.paths {
            let case = format!("{} / {path_case}", root.case);
            let viewed = view(&router, domain, path).await;
            match expected {
                Some(body) => {
                    assert_eq!(
                        (viewed.status, viewed.text().as_str()),
                        (200, *body),
                        "case {case:?}: expected the object's own bytes, got {} with {:?}",
                        viewed.status,
                        viewed.text()
                    );
                }
                None => {
                    assert_eq!(
                        viewed.status, 403,
                        "case {case:?}: expected the bucket's own refusal, got {} with {:?}",
                        viewed.status,
                        viewed.text()
                    );
                    for (_key, body) in [ROOT_PAGE, NESTED_PAGE, START_PAGE] {
                        assert!(
                            !viewed.text().contains(body),
                            "case {case:?}: the answer must carry none of the bucket's objects, \
                             got {:?}",
                            viewed.text()
                        );
                    }
                    // What a root object is for: the viewer learns nothing about what the
                    // bucket holds.
                    assert!(
                        !viewed.text().contains("ListBucketResult"),
                        "case {case:?}: the viewer must never be relayed a listing of the \
                         bucket, got {:?}",
                        viewed.text()
                    );
                }
            }
        }
    }
    // The staged distributions are read back once more through the control plane, so the suite
    // states that each one really is the configuration it drove.
    for (root, id, _domain) in &staged {
        let read = call(
            &router,
            "cloudfront",
            CLOUDFRONT_HOST,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            &[],
            Bytes::new(),
        )
        .await;
        assert_deployed(&router, root.case, id).await;
        assert_eq!(
            read.element("DefaultRootObject"),
            root.root_object.unwrap_or_default(),
            "case {:?}: expected the stored DefaultRootObject to read back, got {:?}",
            root.case,
            read.text()
        );
    }
}
