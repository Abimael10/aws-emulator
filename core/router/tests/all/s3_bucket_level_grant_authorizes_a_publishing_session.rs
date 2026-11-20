// SPDX-License-Identifier: CC0-1.0

//! A BUCKET-LEVEL Amazon S3 call made by an assumed-role session is decided against the
//! bucket's own ARN — the one with no object suffix — and the identity-based statements the
//! account holds for that role.
//!
//! RULE: a request's authorization is decided over `(the IAM action the operation needs, the
//! ARN the request addressed)`, where a call about the bucket itself (a listing, a bucket
//! head) addresses `arn:<partition>:s3:::<bucket>` and a call about one object addresses
//! `arn:<partition>:s3:::<bucket>/<key>`; a statement applies when one of its `Action`
//! patterns covers that action — IAM compares action names case-insensitively and reads `*`
//! and `?` as wildcards — and one of its `Resource` patterns covers that ARN. So an
//! identity-based grant that names only the bucket's objects (`.../*`) authorizes no
//! bucket-level call, one that names only the bucket authorizes no object call, and one that
//! names both authorizes both; with neither covered the call is refused 403 `AccessDenied`
//! naming the session, the action and the ARN the request addressed.
//!
//! That rule is what an asset-publishing session depends on: a publishing grant states the
//! wildcard action family (`s3:List*`, `s3:GetObject*`, `s3:PutObject*`, …) over the staging
//! bucket AND its objects, and the client's bucket-level preflight — `HeadBucket`,
//! `ListObjectsV2` — has to be served under it before any object is written.
//!
//! Three services have to agree for it to hold — IAM holds the role and the document embedded
//! in it, STS mints the session credential and names the principal it speaks for, S3 decides
//! the call — so this is the suite that can pin it: a crate-local suite can seed one side's
//! state but never drive `CreateRole`/`PutRolePolicy`/`AssumeRole` end to end into another
//! service's answer.
//!
//! The rows differ along what the rule generalises over: the FORM the `Action` element is
//! written in (the wildcard family, the exact action names, the service-wide wildcard, the
//! names in another case, names that cover neither call), and WHICH ARNs the `Resource`
//! element names (the bucket, its objects, both, every resource, another bucket's). Each row
//! is driven through five calls — three bucket-level ones and two object ones — and a served
//! call has to ANSWER as well as pass: the listing enumerates the key, the read answers the
//! bytes last written, and the write is read back through the account's own credential.
//!
//! `cloudformation_asset_bucket_publishing_role.rs` pins the other half of the same chain:
//! that a stack DECLARING such a grant really embeds it in the role.
//!
//! Everything about the WIRE here — the three services on one engine, the awsQuery and S3
//! callers, the role, the session, the bucket and its objects, and the sentence a refusal is
//! worded in — is taken from `sts_session_credential_refusals`, the suite over this same gate
//! that publishes them, so none of it is stated twice.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::sts_session_credential_refusals::{
    assume_role, build_router, create_role, get_object, iam, message_of, no_grant,
    not_authorized, object_arn, put_role_policy, s3, seed_bucket, session_arn, AMBIENT_KEY,
    ASCII_KEY, OBJECT_BODY,
};

/// The bucket every call below addresses: the one the account's own credential creates, which
/// stands for a publishing session's staging bucket.
const BUCKET: &str = "probe-publishing-assets";

/// A bucket no call addresses, which one row's grant names instead of [`BUCKET`].
const OTHER_BUCKET: &str = "probe-other-assets";

/// The key the object calls address — one under a prefix, so the object's ARN is the bucket's
/// with a path and not just a name.
const OBJECT_KEY: &str = ASCII_KEY;

/// What a served `PutObject` writes, which the read then has to answer.
const PUBLISHED_BODY: &str = "published-by-the-session";

/// The action patterns an asset-publishing grant states over its staging bucket: the wildcard
/// FAMILIES, which is the form that makes `s3:ListBucket` — the permission a bucket listing
/// needs — reachable through `s3:List*` rather than by name.
const PUBLISHING_ACTIONS: &[&str] = &[
    "s3:GetObject*",
    "s3:GetBucket*",
    "s3:GetEncryptionConfiguration",
    "s3:List*",
    "s3:DeleteObject*",
    "s3:PutObject*",
    "s3:Abort*",
];

/// WHAT one call asks of the bucket, which decides both the ARN it is judged over and what a
/// served answer has to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    /// `HeadBucket`: about the bucket itself, and its answer carries no body.
    BucketHead,
    /// A listing of the bucket's objects: about the bucket itself, and a served one
    /// enumerates the key the bucket holds.
    Listing,
    /// `GetObject`: about one object, and a served one answers the bytes last written.
    Read,
    /// `PutObject`: about one object, and a served one leaves those bytes behind.
    Write,
}

impl What {
    /// Whether the decision is taken over the BUCKET's own ARN rather than over one object's.
    fn bucket_level(self) -> bool {
        matches!(self, Self::BucketHead | Self::Listing)
    }
}

/// One call the session makes over [`BUCKET`].
struct Call {
    /// What to call this call in a failure.
    case: &'static str,
    /// The HTTP method the operation is addressed with.
    method: &'static str,
    /// The query string that addresses this operation, for a call about the BUCKET; an object
    /// call addresses [`OBJECT_KEY`] and carries none.
    query: &'static str,
    /// The body the request carries.
    body: &'static str,
    /// The IAM action the operation needs, which is what a refusal names.
    action: &'static str,
    /// What it asks of the bucket.
    what: What,
}

/// The five calls every row is driven through: three about the bucket itself — the preflight
/// an asset publication makes, in both listing forms — and two about one object.
const CALLS: &[Call] = &[
    Call {
        case: "HeadBucket",
        method: "HEAD",
        query: "",
        body: "",
        action: "s3:ListBucket",
        what: What::BucketHead,
    },
    Call {
        case: "ListObjectsV2",
        method: "GET",
        query: "?list-type=2",
        body: "",
        action: "s3:ListBucket",
        what: What::Listing,
    },
    Call {
        case: "ListObjects",
        method: "GET",
        query: "",
        body: "",
        action: "s3:ListBucket",
        what: What::Listing,
    },
    Call {
        case: "GetObject",
        method: "GET",
        query: "",
        body: "",
        action: "s3:GetObject",
        what: What::Read,
    },
    Call {
        case: "PutObject",
        method: "PUT",
        query: "",
        body: PUBLISHED_BODY,
        action: "s3:PutObject",
        what: What::Write,
    },
];

impl Call {
    /// Make the call as `access_key_id`.
    async fn drive(&self, router: &MultiServiceRouter, access_key_id: &str) -> (u16, String) {
        let path = if self.what.bucket_level() {
            format!("/{BUCKET}{}", self.query)
        } else {
            format!("/{BUCKET}/{OBJECT_KEY}")
        };
        s3(router, access_key_id, self.method, &path, self.body).await
    }

    /// The ARN the decision about this call is taken over, and which a refusal names.
    fn resource(&self) -> String {
        if self.what.bucket_level() {
            format!("arn:aws:s3:::{BUCKET}")
        } else {
            object_arn(BUCKET, OBJECT_KEY)
        }
    }

    /// The sentence S3 refuses this call with when no identity-based document allows it, as
    /// the sibling suite's composers word it.
    fn refusal(&self, role_name: &str) -> String {
        not_authorized(
            &session_arn(role_name),
            self.action,
            &self.resource(),
            &no_grant(self.action),
        )
    }
}

/// One row's statement: the `Action` patterns it names and the `Resource` ARNs it names, with
/// `{bucket}` standing for the bucket every call addresses and `{other}` for one none does.
struct Grant {
    /// What to call this row in a failure.
    case: &'static str,
    /// The `Action` element of the row's one statement.
    actions: &'static [&'static str],
    /// The `Resource` element of it.
    resources: &'static [&'static str],
    /// Whether the three bucket-level calls are served under it.
    bucket_level: bool,
    /// Whether the two object calls are served under it.
    object_level: bool,
}

/// The rows: one statement each, spanning the forms an `Action` element is written in and the
/// ARNs a `Resource` element names.
const GRANTS: &[Grant] = &[
    Grant {
        case: "the wildcard action family over the bucket and its objects",
        actions: PUBLISHING_ACTIONS,
        resources: &["arn:aws:s3:::{bucket}", "arn:aws:s3:::{bucket}/*"],
        bucket_level: true,
        object_level: true,
    },
    Grant {
        case: "the exact action names over the bucket and its objects",
        actions: &["s3:ListBucket", "s3:GetObject", "s3:PutObject"],
        resources: &["arn:aws:s3:::{bucket}", "arn:aws:s3:::{bucket}/*"],
        bucket_level: true,
        object_level: true,
    },
    Grant {
        case: "the service-wide wildcard over every resource",
        actions: &["s3:*"],
        resources: &["*"],
        bucket_level: true,
        object_level: true,
    },
    Grant {
        case: "the action names written in another case",
        actions: &["S3:LISTBUCKET", "s3:getobject", "s3:PUTobject"],
        resources: &["arn:aws:s3:::{bucket}", "arn:aws:s3:::{bucket}/*"],
        bucket_level: true,
        object_level: true,
    },
    Grant {
        case: "only the objects of the bucket",
        actions: PUBLISHING_ACTIONS,
        resources: &["arn:aws:s3:::{bucket}/*"],
        bucket_level: false,
        object_level: true,
    },
    Grant {
        case: "only the bucket itself",
        actions: PUBLISHING_ACTIONS,
        resources: &["arn:aws:s3:::{bucket}"],
        bucket_level: true,
        object_level: false,
    },
    Grant {
        case: "action names that cover neither call",
        actions: &[
            "s3:ListAllMyBuckets",
            "s3:ListBucketVersions",
            "s3:GetBucketPolicy",
        ],
        resources: &["arn:aws:s3:::{bucket}", "arn:aws:s3:::{bucket}/*"],
        bucket_level: false,
        object_level: false,
    },
    Grant {
        case: "the whole family over another bucket",
        actions: PUBLISHING_ACTIONS,
        resources: &["arn:aws:s3:::{other}", "arn:aws:s3:::{other}/*"],
        bucket_level: false,
        object_level: false,
    },
];

impl Grant {
    /// The document this row embeds in its role.
    fn document(&self) -> String {
        let resources: Vec<String> = self
            .resources
            .iter()
            .map(|arn| {
                arn.replace("{bucket}", BUCKET)
                    .replace("{other}", OTHER_BUCKET)
            })
            .collect();
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": self.actions,
                "Resource": resources,
            }],
        })
        .to_string()
    }

    /// Whether `call` is served under this row's statement.
    fn allows(&self, call: &Call) -> bool {
        if call.what.bucket_level() {
            self.bucket_level
        } else {
            self.object_level
        }
    }
}

/// Drive `call` as `session`, judge it against what `grant` covers, and answer the bytes the
/// object holds afterwards.
///
/// `stored` is what it held before. A SERVED call has to answer as well as pass — the listing
/// enumerates the key, the read answers `stored`, and the write's bytes are read back through
/// the account's own credential — and a refused one has to carry AWS's whole sentence, which
/// is the one thing a client reads to learn which action on which ARN was withheld.
async fn drive_and_judge(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    grant: &Grant,
    call: &Call,
    session: &str,
    stored: &str,
) -> String {
    let (status, body) = call.drive(router, session).await;
    let expected = if grant.allows(call) { 200 } else { 403 };
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected} for the role session, got {status}: {body}"
    );
    if expected == 403 {
        // A refusal is worded from the request, so it states which action on which ARN was
        // withheld, and by what. A HEAD answers no body on AWS — "the HEAD request returns a
        // generic 400 Bad Request, 403 Forbidden, or 404 Not Found HTTP status code"
        // (`HeadBucket`) — so for that one the status is the whole of the refusal.
        if call.what != What::BucketHead {
            let message = message_of(&body);
            assert_eq!(
                message,
                call.refusal(role_name),
                "case {case:?}: expected {:?}, got {message:?}",
                call.refusal(role_name)
            );
        }
        return stored.to_string();
    }
    match call.what {
        // The status is the whole answer: a bucket head carries no body.
        What::BucketHead => stored.to_string(),
        What::Listing => {
            let entry = format!("<Key>{OBJECT_KEY}</Key>");
            assert!(
                body.contains(&entry),
                "case {case:?}: the listing must enumerate {OBJECT_KEY:?}, got {body}"
            );
            stored.to_string()
        }
        What::Read => {
            assert_eq!(
                body, stored,
                "case {case:?}: the read must answer the bytes the object holds, got {body:?}"
            );
            stored.to_string()
        }
        What::Write => {
            // What the session wrote is what the bucket holds: read back through the
            // account's own credential, which no identity-based document bounds, so the
            // round trip is judged on the store and not on the grant.
            let (status, read) = get_object(router, AMBIENT_KEY, BUCKET, OBJECT_KEY).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the written object must read back, got {status}: {read}"
            );
            assert_eq!(
                read, PUBLISHED_BODY,
                "case {case:?}: expected the bytes the session wrote, got {read:?}"
            );
            PUBLISHED_BODY.to_string()
        }
    }
}

/// RULE: a bucket-level call is decided against the bucket's OWN ARN and an object call
/// against the object's, so a role's identity-based grant serves exactly the calls whose
/// action and ARN its statement covers — and refuses the rest 403 `AccessDenied` in AWS's
/// sentence, naming the session, that action and the ARN the request addressed.
#[tokio::test]
async fn a_roles_grant_serves_exactly_the_calls_its_statement_covers() {
    let router = build_router();
    seed_bucket(&router, "the fixture", BUCKET).await;
    let mut stored = OBJECT_BODY.to_string();

    for (index, grant) in GRANTS.iter().enumerate() {
        let role_name = format!("publishing-role-{index}");
        create_role(&router, grant.case, &role_name).await;
        put_role_policy(
            &router,
            grant.case,
            &role_name,
            "bucket-access",
            &grant.document(),
        )
        .await;
        let session = assume_role(&router, grant.case, &role_name, None).await;

        for call in CALLS {
            let case = format!("{} / {}", grant.case, call.case);
            stored = drive_and_judge(
                &router, &case, &role_name, grant, call, &session, &stored,
            )
            .await;
        }
    }
}

/// RULE applied to the DOCUMENTS IN FORCE: the decision is taken from the policies the
/// account holds for the role at the instant of the call, so removing the grant refuses the
/// very calls it was serving — to the same session, whose credential never changed.
#[tokio::test]
async fn removing_the_grant_refuses_the_calls_it_was_serving() {
    let router = build_router();
    seed_bucket(&router, "the fixture", BUCKET).await;
    let mut stored = OBJECT_BODY.to_string();

    // The row that serves every call, stated once above.
    let granted = &GRANTS[0];
    let role_name = "publishing-role-withdrawn";
    create_role(&router, granted.case, role_name).await;
    put_role_policy(
        &router,
        granted.case,
        role_name,
        "bucket-access",
        &granted.document(),
    )
    .await;
    let session = assume_role(&router, granted.case, role_name, None).await;

    for call in CALLS {
        let case = format!("{} while the grant is in force", call.case);
        stored = drive_and_judge(
            &router, &case, role_name, granted, call, &session, &stored,
        )
        .await;
    }

    let (status, body) = iam(
        &router,
        "DeleteRolePolicy",
        &[("RoleName", role_name), ("PolicyName", "bucket-access")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"withdrawing the grant\": DeleteRolePolicy expected 200, got {status}: {body}"
    );

    // The same statement with nothing in force behind it: every call this row's grant was
    // serving is now refused, which is what says the decision is read from the documents.
    let withdrawn = Grant {
        case: granted.case,
        actions: granted.actions,
        resources: granted.resources,
        bucket_level: false,
        object_level: false,
    };
    for call in CALLS {
        let case = format!("{} after the grant is withdrawn", call.case);
        stored = drive_and_judge(
            &router,
            &case,
            role_name,
            &withdrawn,
            call,
            &session,
            &stored,
        )
        .await;
    }
}
