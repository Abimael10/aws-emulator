// SPDX-License-Identifier: CC0-1.0

//! What a `sts:GetFederationToken` session minted with the ACCOUNT's own published credential
//! may do in S3, and which document a refusal of one names.
//!
//! RULE: a request signed with a session credential is judged on the identity-based standing of
//! the identity the session STANDS ON — for a federated-user session the IAM user whose
//! long-term credentials minted it, since "the session permissions are the intersection of the
//! IAM user policies and the session policies that you pass" and the federated user carries no
//! document of its own (`GetFederationTokenRequest$Policy`, the STS model). The account's own
//! published credential is bounded by no identity-based policy, so a session it minted is
//! bounded identity-wise by nothing either: what such a session may do is exactly what the
//! scope its mint passed allows — nothing at all for a mint that passed no policy, "if you do
//! not pass any session policies, then the resulting federated user session has no permissions"
//! — and a request outside that scope is refused 403 `AccessDenied` naming the SESSION policy
//! as the document that withheld it, not the identity's own documents.
//!
//! The rows span what the rule generalises over rather than the one recorded call: what the
//! mint passed (no policy at all, a grant naming the action called, a grant naming another
//! action, a grant scoped to another bucket's objects, a grant with an explicit `Deny` over it)
//! and which call the vended credential then makes — the account-wide bucket listing, which
//! addresses no resource of its own and so carries no `on resource:` clause, an object read, a
//! bucket create, and the bucket-policy write, whose lockout exception belongs to the account's
//! own credential ITSELF ("the root principal in a bucket owner's Amazon Web Services account
//! can perform the `GetBucketPolicy`, `PutBucketPolicy`, and `DeleteBucketPolicy` API
//! actions", `PutBucketPolicy`) and not to a session that credential minted, which speaks for
//! a principal of its own.
//!
//! The harness is [`super::federated_session_authorization`]'s: S3 beside IAM and STS over ONE
//! shared engine, with the mint made through STS's own `GetFederationToken` and every S3
//! request presenting the access key id that mint vended.

use super::federated_session_authorization::{
    Answer, Emulator, Session, ACCESS_DENIED, ACCOUNT_KEY, BUCKET, KEY,
};

/// The IAM action the account's own bucket listing asks for ("to grant IAM permission to use
/// this operation, you must add the `s3:ListAllMyBuckets` policy action", `ListBuckets`).
const LIST_ALL_MY_BUCKETS: &str = "s3:ListAllMyBuckets";

/// The IAM action an object read asks for.
const GET_OBJECT: &str = "s3:GetObject";

/// The IAM action a bucket create asks for.
const CREATE_BUCKET: &str = "s3:CreateBucket";

/// The IAM action a bucket-policy write asks for ("the `s3:PutBucketPolicy` permission is
/// required in a policy", `PutBucketPolicy`).
const PUT_BUCKET_POLICY: &str = "s3:PutBucketPolicy";

/// The bucket policy a caller writes, naming in its single `Deny` statement WHICH caller wrote
/// it — so a later read says whose write was stored.
///
/// A `Deny` is used because a wildcard-principal `Allow` is public access, which Block Public
/// Access refuses by default; neither action it names is one any row calls, so the stored
/// document decides nothing else here.
///
/// The statement names the bucket AND its objects, because the two rows deny a bucket action
/// and an object action respectively: S3 refuses a document whose action is performed on no
/// resource type any resource the statement names is of, so a statement naming only the
/// bucket could carry the bucket-scoped row's action alone.
fn written_policy(denied_action: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny","Principal":"*","Action":"{denied_action}","Resource":["arn:aws:s3:::{BUCKET}","arn:aws:s3:::{BUCKET}/*"]}}]}}"#
    )
}

/// The action the MINTING credential's own bucket-policy write names.
const MINTERS_DENIED_ACTION: &str = "s3:DeleteBucket";

/// The action the SESSION's bucket-policy write names.
const SESSIONS_DENIED_ACTION: &str = "s3:DeleteObject";

/// A session policy allowing exactly `action` on every resource.
fn allowing(action: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"*"}}]}}"#
    )
}

/// WHICH call a row makes with the vended credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// `ListBuckets` — the account's own listing, which addresses no resource of its own.
    ListBuckets,
    /// `GetObject` on the seeded key.
    GetObject,
    /// `CreateBucket` on a name no row shares.
    CreateBucket,
    /// `PutBucketPolicy` on the seeded bucket — one of the three operations AWS never refuses
    /// the account's root principal, which a session is not.
    PutBucketPolicy,
}

/// The bucket a `CreateBucket` row addresses: one per row, so a served create and a refused one
/// never address the same name.
fn created_bucket(index: usize) -> String {
    format!("account-minted-federation-{index}")
}

impl Call {
    /// The IAM action this call asks for.
    fn action(self) -> &'static str {
        match self {
            Self::ListBuckets => LIST_ALL_MY_BUCKETS,
            Self::GetObject => GET_OBJECT,
            Self::CreateBucket => CREATE_BUCKET,
            Self::PutBucketPolicy => PUT_BUCKET_POLICY,
        }
    }

    /// The resource a refusal of this call quotes, or `None` for an action that addresses none.
    fn resource(self, index: usize) -> Option<String> {
        match self {
            Self::ListBuckets => None,
            Self::GetObject => Some(format!("arn:aws:s3:::{BUCKET}/{KEY}")),
            Self::CreateBucket => Some(format!("arn:aws:s3:::{}", created_bucket(index))),
            Self::PutBucketPolicy => Some(format!("arn:aws:s3:::{BUCKET}")),
        }
    }

    /// The status this call answers when the policies in force authorize it: `PutBucketPolicy`
    /// declares an empty 204 response, every other call here a 200.
    fn served_status(self) -> u16 {
        match self {
            Self::PutBucketPolicy => 204,
            _ => 200,
        }
    }

    /// Make the call as `access_key_id`.
    async fn drive(self, emulator: &Emulator, access_key_id: &str, index: usize) -> Answer {
        match self {
            Self::ListBuckets => {
                emulator
                    .s3(access_key_id, "GET", "/?x-id=ListBuckets", b"")
                    .await
            }
            Self::GetObject => {
                emulator
                    .s3(access_key_id, "GET", &format!("/{BUCKET}/{KEY}"), b"")
                    .await
            }
            Self::CreateBucket => {
                emulator
                    .s3(access_key_id, "PUT", &format!("/{}", created_bucket(index)), b"")
                    .await
            }
            Self::PutBucketPolicy => {
                // Each caller writes its own document, so the read below says whose write was
                // stored.
                let denied = match access_key_id {
                    ACCOUNT_KEY => MINTERS_DENIED_ACTION,
                    _ => SESSIONS_DENIED_ACTION,
                };
                emulator
                    .s3(
                        access_key_id,
                        "PUT",
                        &format!("/{BUCKET}?policy"),
                        written_policy(denied).as_bytes(),
                    )
                    .await
            }
        }
    }
}

/// What the scope in force answers one call: served, or refused with the clause naming the
/// document that decided it. The clauses are IAM's own published ones, written out here rather
/// than built from the service's own helper so the wording has something to be wrong about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Served: the scope the mint passed allows the call, and nothing identity-based bounds the
    /// identity the session stands on.
    Served,
    /// Refused because the session policies the credential was minted with do not allow the
    /// action — the clause the recorded federation refusal carries.
    NoSessionGrant,
    /// Refused by an explicit `Deny` in the session policy.
    SessionPolicyDeny,
}

impl Verdict {
    /// The reason clause S3's refusal sentence carries for this verdict.
    fn clause(self, action: &str) -> Option<String> {
        match self {
            Self::Served => None,
            Self::NoSessionGrant => Some(format!(
                "because no session policy allows the {action} action"
            )),
            Self::SessionPolicyDeny => {
                Some("with an explicit deny in a session policy".to_string())
            }
        }
    }
}

/// One row: what the mint passed as its session policy, the call made with the vended
/// credential, and what the rule says that call is answered.
struct Row {
    /// What to call the case in a failure.
    case: &'static str,
    /// The inline session policy the mint passed, or `None` for a mint that passed none.
    scope: Option<String>,
    /// The call the row makes with the vended credential.
    call: Call,
    /// What the rule says that call is answered.
    verdict: Verdict,
}

/// The table: every row mints with [`ACCOUNT_KEY`], the account's own published credential.
fn rows() -> Vec<Row> {
    let deny_over_a_broad_grant = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:*","Resource":"*"}},{{"Effect":"Deny","Action":"{LIST_ALL_MY_BUCKETS}","Resource":"*"}}]}}"#
    );
    vec![
        Row {
            // The recorded case: a mint that passed no policy, whose session has no
            // permissions, refused the account's own listing.
            case: "no session policy at all, on the account-wide listing",
            scope: None,
            call: Call::ListBuckets,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "no session policy at all, on an object read",
            scope: None,
            call: Call::GetObject,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "no session policy at all, on a bucket create",
            scope: None,
            call: Call::CreateBucket,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "a session policy naming the listing, on the listing",
            scope: Some(allowing(LIST_ALL_MY_BUCKETS)),
            call: Call::ListBuckets,
            verdict: Verdict::Served,
        },
        Row {
            case: "a session policy naming the listing, on an object read",
            scope: Some(allowing(LIST_ALL_MY_BUCKETS)),
            call: Call::GetObject,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "a session policy naming the object read, on the read",
            scope: Some(allowing(GET_OBJECT)),
            call: Call::GetObject,
            verdict: Verdict::Served,
        },
        Row {
            case: "a session policy naming the bucket create, on the create",
            scope: Some(allowing(CREATE_BUCKET)),
            call: Call::CreateBucket,
            verdict: Verdict::Served,
        },
        Row {
            case: "a session policy naming the create, on the listing",
            scope: Some(allowing(CREATE_BUCKET)),
            call: Call::ListBuckets,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "a session policy scoped to another bucket's objects, on the read",
            scope: Some(
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"arn:aws:s3:::somewhere-else/*"}]}"#
                    .to_string(),
            ),
            call: Call::GetObject,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "a session policy denying the listing above its own broad grant",
            scope: Some(deny_over_a_broad_grant),
            call: Call::ListBuckets,
            verdict: Verdict::SessionPolicyDeny,
        },
        Row {
            // The lockout exception is the account's own credential's, not its sessions':
            // a session that passed no policy may not write the bucket's policy either.
            case: "no session policy at all, on the bucket-policy write",
            scope: None,
            call: Call::PutBucketPolicy,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "a session policy naming the listing, on the bucket-policy write",
            scope: Some(allowing(LIST_ALL_MY_BUCKETS)),
            call: Call::PutBucketPolicy,
            verdict: Verdict::NoSessionGrant,
        },
        Row {
            case: "a session policy naming the bucket-policy write, on that write",
            scope: Some(allowing(PUT_BUCKET_POLICY)),
            call: Call::PutBucketPolicy,
            verdict: Verdict::Served,
        },
    ]
}

/// RULE: a federation session minted with the account's own published credential may do exactly
/// what its session policy allows — and a call outside that scope is refused 403 `AccessDenied`
/// naming the SESSION policy, with the federated-user ARN the mint answered with as the
/// principal and no `on resource:` clause for an action that addresses no resource.
#[tokio::test]
async fn a_session_the_account_credential_minted_is_bounded_by_its_session_policy_alone() {
    for (index, row) in rows().iter().enumerate() {
        let emulator = Emulator::new();
        emulator.seeded_bucket().await;
        // The positive control: the credential that MINTS the session may make every call the
        // rows make, so a refusal below is attributable to the session's scope and not to the
        // minting identity's own documents.
        let control = row.call.drive(&emulator, ACCOUNT_KEY, index).await;
        assert_eq!(
            control.status,
            row.call.served_status(),
            "case {:?}: the minting credential's own {:?} must be served, got {} with {}",
            row.case,
            row.call,
            control.status,
            control.body
        );
        let session: Session = emulator
            .minted_federation_session(
                ACCOUNT_KEY,
                &format!("accountminted{index}"),
                row.scope.as_deref(),
            )
            .await;
        // The create the control just made holds the name, so the row's own create addresses a
        // fresh one.
        let index = index + rows().len();
        let answer = row.call.drive(&emulator, &session.access_key_id, index).await;
        match row.verdict.clause(row.call.action()) {
            None => assert_eq!(
                answer.status,
                row.call.served_status(),
                "case {:?} ({:?}): expected the call to be authorized, got {} with {}",
                row.case,
                row.call,
                answer.status,
                answer.body
            ),
            Some(clause) => {
                assert_eq!(
                    answer.status, 403,
                    "case {:?} ({:?}): expected HTTP 403, got {} with {}",
                    row.case, row.call, answer.status, answer.body
                );
                let code = answer.error_code();
                assert_eq!(
                    code, ACCESS_DENIED,
                    "case {:?} ({:?}): expected {ACCESS_DENIED}, got {code:?}: {}",
                    row.case, row.call, answer.body
                );
                let expected = match row.call.resource(index) {
                    Some(resource) => format!(
                        "User: {} is not authorized to perform: {} on resource: \"{resource}\" \
                         {clause}",
                        session.federated_user_arn,
                        row.call.action()
                    ),
                    None => format!(
                        "User: {} is not authorized to perform: {} {clause}",
                        session.federated_user_arn,
                        row.call.action()
                    ),
                };
                let message = answer.error_message();
                assert_eq!(
                    message, expected,
                    "case {:?} ({:?}): expected {expected:?}, got {message:?}",
                    row.case, row.call
                );
            }
        }
        // A create the scope refuses must leave nothing behind, and one it allows must leave the
        // bucket the account can list.
        if row.call == Call::CreateBucket {
            let name = created_bucket(index);
            let listed = emulator.account_buckets().await;
            let holds = listed.contains(&format!("<Name>{name}</Name>"));
            assert_eq!(
                holds,
                row.verdict == Verdict::Served,
                "case {:?}: expected the account's listing to name {name:?} only where the \
                 create was served, got {listed}",
                row.case
            );
        }
        // A refused bucket-policy write must leave the document the minting credential wrote,
        // and a served one must replace it: the account's own read says which.
        if row.call == Call::PutBucketPolicy {
            let read = emulator
                .s3(ACCOUNT_KEY, "GET", &format!("/{BUCKET}?policy"), b"")
                .await;
            assert_eq!(
                read.status, 200,
                "case {:?}: the account's own policy read must be served, got {} with {}",
                row.case, read.status, read.body
            );
            let expected = match row.verdict {
                Verdict::Served => SESSIONS_DENIED_ACTION,
                _ => MINTERS_DENIED_ACTION,
            };
            assert!(
                read.body.contains(expected),
                "case {:?}: expected the stored policy to name {expected:?}, got {}",
                row.case,
                read.body
            );
        }
    }
}
