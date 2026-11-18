// SPDX-License-Identifier: CC0-1.0

//! What a media read owes a caller whose identity-based policies CHANGE while it holds a
//! session credential, driven end to end over the wire against the services that own the
//! identity (STS) and the policies in force on it (IAM).
//!
//! The rule under test, one level above the recorded case (one role, one allow, one deny
//! attached after a served call):
//!
//! > A media data-plane read is authorized on EVERY request against the identity-based
//! > policies in force on the principal the signing credential speaks for AT THAT MOMENT,
//! > with an explicit `Deny` beating every `Allow`: a `Deny` covering the request's action
//! > and resource that is attached after the session credential was minted refuses the next
//! > call with `AccessDeniedException` (HTTP 403), naming the signing credential's
//! > assumed-role session ARN, the action and the stream as the request addressed it, and
//! > removing that `Deny` lets the same credential through again — no re-authentication and
//! > no new session.
//!
//! The rows span what the rule generalises over: the statements a `Deny` can be written as
//! (the exact action, the family's wildcard action, the action among a list, scoped to every
//! resource or to this stream's ARN) and the statements that do NOT cover the call (a `Deny`
//! of another action of the family, a `Deny` scoped to another stream), both identifier forms
//! a request addresses a stream by, and a distinct role and session name per row — so the
//! principal the refusal names is the ROW's session and not one spelling the suite shares.
//!
//! The harness, the role setup and the response readers are
//! [`crate::media_plane_authorization`]'s: that suite fixes the policy before the session is
//! minted and varies WHICH statement the principal holds, while this one holds the statement
//! fixed and varies WHEN it is attached.

use serde_json::{json, Value};

use crate::media_plane_authorization::{
    error_code, message, policy, producer_start, status, xml_element, Harness, ACCOUNT_KEY,
};

/// The code and status AWS refuses a call IAM does not permit with.
const DENIED_CODE: &str = "AccessDeniedException";
const DENIED_STATUS: u16 = 403;

/// The stream every row reads, and another one a statement can be scoped to instead.
const STREAM: &str = "policy-change-stream";
const OTHER_STREAM: &str = "policy-change-other-stream";

/// The two inline policies a role carries in turn: the `Allow` that is in force from the
/// start, and the `Deny` attached under a session that is already being served. Two
/// documents on one role, as the recorded sequence attaches them.
const ALLOW_POLICY_NAME: &str = "allow-get-media";
const DENY_POLICY_NAME: &str = "deny-get-media";

/// The tag name every chunk `GetMedia` serves carries in its Matroska `Tags` block, so a
/// body holding none of it served no chunk.
const MKV_TAG_MARKER: &[u8] = b"AWS_KINESISVIDEO_";

/// A session of a role: the access key id its media calls are signed with, and the session
/// ARN STS reports for it — the principal a refusal names.
struct Session {
    access_key_id: String,
    arn: String,
}

/// Create `role` with `ALLOW_POLICY_NAME` granting `kinesisvideo:GetMedia`, assume it under
/// `session_name`, and answer that session's credential and ARN.
async fn allowed_session(harness: &Harness, role: &str, session_name: &str) -> Session {
    allowing_role(harness, role).await;
    assume_session(harness, role, session_name).await
}

/// Create `role`, assumable by this account, carrying `ALLOW_POLICY_NAME` as its inline
/// policy granting `kinesisvideo:GetMedia` on every resource.
async fn allowing_role(harness: &Harness, role: &str) {
    let account = engine::EMULATOR_ACCOUNT_ID;
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (code, body) = harness
        .query(
            "iam",
            "2010-05-08",
            "CreateRole",
            &[
                ("RoleName", role.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
    assert_eq!(
        code, 200,
        "setup: creating the role {role:?} must succeed, got {code} {body}"
    );
    put_role_policy(
        harness,
        role,
        ALLOW_POLICY_NAME,
        &policy("Allow", &["kinesisvideo:GetMedia"], &["*"]),
    )
    .await;
}

/// Assume the existing role `role` under `session_name` and answer that session's
/// credential and the session ARN STS reports for it.
async fn assume_session(harness: &Harness, role: &str, session_name: &str) -> Session {
    let account = engine::EMULATOR_ACCOUNT_ID;
    let (code, body) = harness
        .query(
            "sts",
            "2011-06-15",
            "AssumeRole",
            &[
                ("RoleArn", format!("arn:aws:iam::{account}:role/{role}")),
                ("RoleSessionName", session_name.to_string()),
            ],
        )
        .await;
    assert_eq!(
        code, 200,
        "setup: assuming {role:?} as {session_name:?} must succeed, got {code} {body}"
    );
    let session = Session {
        access_key_id: xml_element(&body, "AccessKeyId")
            .unwrap_or_else(|| panic!("setup: AssumeRole must answer an AccessKeyId, got {body}")),
        arn: xml_element(&body, "Arn")
            .unwrap_or_else(|| panic!("setup: AssumeRole must answer the session Arn, got {body}")),
    };
    // The spelling of an assumed-role session ARN: the account, the role's name and the
    // RoleSessionName the caller asked for. This is the principal a refusal has to name.
    assert_eq!(
        session.arn,
        format!("arn:aws:sts::{account}:assumed-role/{role}/{session_name}"),
        "setup: expected the session of {role:?} to be reported as an assumed-role session \
         ARN, got {:?}",
        session.arn
    );
    session
}

/// Attach `document` to `role` as the inline policy `name`.
async fn put_role_policy(harness: &Harness, role: &str, name: &str, document: &str) {
    let (code, body) = harness
        .query(
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", role.to_string()),
                ("PolicyName", name.to_string()),
                ("PolicyDocument", document.to_string()),
            ],
        )
        .await;
    assert_eq!(
        code, 200,
        "setup: putting {name:?} on {role:?} must succeed, got {code} {body}"
    );
}

/// Remove the inline policy `name` from `role`.
async fn delete_role_policy(harness: &Harness, role: &str, name: &str) {
    let (code, body) = harness
        .query(
            "iam",
            "2010-05-08",
            "DeleteRolePolicy",
            &[
                ("RoleName", role.to_string()),
                ("PolicyName", name.to_string()),
            ],
        )
        .await;
    assert_eq!(
        code, 200,
        "setup: deleting {name:?} from {role:?} must succeed, got {code} {body}"
    );
}

/// The sentence this media plane refuses an unauthorized read with, for a principal and the
/// stream as the request addressed it.
fn expected_refusal(principal: &str, addressed_as: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: kinesisvideo:GetMedia on resource: \
         {addressed_as}"
    )
}

/// Whether a response carries a chunk of the stream's media.
fn serves_a_chunk(response: &http::Response<bytes::Bytes>) -> bool {
    response
        .body()
        .windows(MKV_TAG_MARKER.len())
        .any(|window| window == MKV_TAG_MARKER)
}

/// One row: the `Deny` attached under an already-served session, and whether it covers the
/// read the row makes.
struct Row {
    case: &'static str,
    role: &'static str,
    session_name: &'static str,
    /// The document attached as `DENY_POLICY_NAME` while the session is being served.
    deny: String,
    /// How the request addresses the stream.
    addressing: Value,
    /// The stream as that request addresses it — what a refusal names.
    addressed_as: String,
    /// Whether the attached statement covers this read.
    refused: bool,
}

/// RULE: a media read is decided on every request against the policies in force at that
/// moment, so a `Deny` attached after a session credential was minted refuses the next call
/// and removing it lets the same credential through again.
#[tokio::test]
async fn a_deny_attached_under_a_served_session_refuses_its_next_read() {
    let harness = Harness::new();
    let arn = harness.create_stream(STREAM).await;
    let other_arn = harness.create_stream(OTHER_STREAM).await;
    // The stream holds media, so a served read really answers a chunk and a refused one is
    // never an empty payload in disguise.
    let seeded = harness
        .put_media(ACCOUNT_KEY, STREAM, producer_start(0))
        .await;
    assert_eq!(
        status(&seeded),
        200,
        "setup: seeding {STREAM:?} must succeed, got {} ({})",
        status(&seeded),
        error_code(&seeded)
    );

    let rows: Vec<Row> = vec![
        Row {
            case: "a deny of the exact action on every resource",
            role: "change-exact-action",
            session_name: "probe-exact",
            deny: policy("Deny", &["kinesisvideo:GetMedia"], &["*"]),
            addressing: json!({ "StreamName": STREAM }),
            addressed_as: STREAM.to_string(),
            refused: true,
        },
        Row {
            case: "a deny of every action of the family on every resource",
            role: "change-wildcard-action",
            session_name: "probe-wildcard",
            deny: policy("Deny", &["kinesisvideo:*"], &["*"]),
            addressing: json!({ "StreamName": STREAM }),
            addressed_as: STREAM.to_string(),
            refused: true,
        },
        Row {
            case: "a deny carrying the action among others",
            role: "change-action-list",
            session_name: "probe-list",
            deny: policy(
                "Deny",
                &["kinesisvideo:PutMedia", "kinesisvideo:GetMedia"],
                &["*"],
            ),
            addressing: json!({ "StreamName": STREAM }),
            addressed_as: STREAM.to_string(),
            refused: true,
        },
        Row {
            case: "a deny scoped to this stream's ARN",
            role: "change-this-stream",
            session_name: "probe-this-stream",
            deny: policy("Deny", &["kinesisvideo:GetMedia"], &[&arn]),
            addressing: json!({ "StreamName": STREAM }),
            addressed_as: STREAM.to_string(),
            refused: true,
        },
        Row {
            case: "a deny met by a request addressing the stream by ARN",
            role: "change-addressed-by-arn",
            session_name: "probe-by-arn",
            deny: policy("Deny", &["kinesisvideo:GetMedia"], &["*"]),
            addressing: json!({ "StreamARN": arn.clone() }),
            addressed_as: arn.clone(),
            refused: true,
        },
        Row {
            case: "a deny of another action of the family",
            role: "change-other-action",
            session_name: "probe-other-action",
            deny: policy("Deny", &["kinesisvideo:PutMedia"], &["*"]),
            addressing: json!({ "StreamName": STREAM }),
            addressed_as: STREAM.to_string(),
            refused: false,
        },
        Row {
            case: "a deny scoped to another stream",
            role: "change-other-stream",
            session_name: "probe-other-stream",
            deny: policy("Deny", &["kinesisvideo:GetMedia"], &[&other_arn]),
            addressing: json!({ "StreamName": STREAM }),
            addressed_as: STREAM.to_string(),
            refused: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let session = allowed_session(&harness, row.role, row.session_name).await;

        // The allow alone is in force: the read is served. This is the positive control the
        // recorded sequence starts from.
        let served = harness
            .get_media(&session.access_key_id, &row.addressing)
            .await;
        assert_eq!(
            status(&served),
            200,
            "case {case:?}: with only the allow in force, GetMedia expected HTTP 200, got {} \
             ({}) {:?}",
            status(&served),
            error_code(&served),
            message(&served)
        );
        assert!(
            serves_a_chunk(&served),
            "case {case:?}: with only the allow in force, GetMedia must answer the stream's \
             media, got a body carrying no chunk framing"
        );

        // The deny is attached under the same, already-served session credential.
        put_role_policy(&harness, row.role, DENY_POLICY_NAME, &row.deny).await;
        let after_attach = harness
            .get_media(&session.access_key_id, &row.addressing)
            .await;
        if row.refused {
            assert_eq!(
                status(&after_attach),
                DENIED_STATUS,
                "case {case:?}: after the deny is attached, GetMedia expected HTTP \
                 {DENIED_STATUS}, got {} ({})",
                status(&after_attach),
                error_code(&after_attach)
            );
            assert_eq!(
                error_code(&after_attach),
                DENIED_CODE,
                "case {case:?}: expected {DENIED_CODE}, got {:?}",
                error_code(&after_attach)
            );
            let answered = message(&after_attach);
            assert_eq!(
                answered,
                expected_refusal(&session.arn, &row.addressed_as),
                "case {case:?}: expected the refusal to name this session and the stream as \
                 the request addressed it, got {answered:?}"
            );
            assert!(
                !serves_a_chunk(&after_attach),
                "case {case:?}: a refused read must serve no chunk, but its body carries the \
                 chunk framing"
            );
        } else {
            assert_eq!(
                status(&after_attach),
                200,
                "case {case:?}: a deny that does not cover this read must leave it served, \
                 got HTTP {} ({}) {:?}",
                status(&after_attach),
                error_code(&after_attach),
                message(&after_attach)
            );
            assert!(
                serves_a_chunk(&after_attach),
                "case {case:?}: a deny that does not cover this read must leave the stream's \
                 media served, got a body carrying no chunk framing"
            );
        }

        // And removing it lifts the refusal for the SAME credential: nothing was re-signed
        // and no new session was minted.
        delete_role_policy(&harness, row.role, DENY_POLICY_NAME).await;
        let after_delete = harness
            .get_media(&session.access_key_id, &row.addressing)
            .await;
        assert_eq!(
            status(&after_delete),
            200,
            "case {case:?}: once the deny is removed, the same credential expected HTTP 200, \
             got {} ({}) {:?}",
            status(&after_delete),
            error_code(&after_delete),
            message(&after_delete)
        );
        assert!(
            serves_a_chunk(&after_delete),
            "case {case:?}: once the deny is removed, the read must answer the stream's media \
             again, got a body carrying no chunk framing"
        );
    }
}

/// RULE: the policies the decision reads are the PRINCIPAL's, so a deny attached to a role
/// covers every session of it — the one minted before the attach and the one minted after —
/// and each refusal names the session that made the call.
#[tokio::test]
async fn a_deny_on_a_role_covers_every_session_of_it() {
    let harness = Harness::new();
    let role = "every-session";
    harness.create_stream(STREAM).await;
    let seeded = harness
        .put_media(ACCOUNT_KEY, STREAM, producer_start(0))
        .await;
    assert_eq!(
        status(&seeded),
        200,
        "setup: seeding {STREAM:?} must succeed, got {} ({})",
        status(&seeded),
        error_code(&seeded)
    );

    let before_attach = {
        allowing_role(&harness, role).await;
        assume_session(&harness, role, "minted-before").await
    };
    let addressing = json!({ "StreamName": STREAM });
    let served = harness
        .get_media(&before_attach.access_key_id, &addressing)
        .await;
    assert_eq!(
        status(&served),
        200,
        "case \"the session minted before the deny, while only the allow is in force\": \
         expected HTTP 200, got {} ({})",
        status(&served),
        error_code(&served)
    );

    put_role_policy(
        &harness,
        role,
        DENY_POLICY_NAME,
        &policy("Deny", &["kinesisvideo:GetMedia"], &["*"]),
    )
    .await;
    // A second session of the SAME role, minted while the deny is in force.
    let after_attach = assume_session(&harness, role, "minted-after").await;

    for (case, session) in [
        ("the session minted before the deny", &before_attach),
        ("the session minted after it", &after_attach),
    ] {
        let response = harness.get_media(&session.access_key_id, &addressing).await;
        assert_eq!(
            status(&response),
            DENIED_STATUS,
            "case {case:?}: expected HTTP {DENIED_STATUS}, got {} ({})",
            status(&response),
            error_code(&response)
        );
        assert_eq!(
            error_code(&response),
            DENIED_CODE,
            "case {case:?}: expected {DENIED_CODE}, got {:?}",
            error_code(&response)
        );
        let answered = message(&response);
        assert_eq!(
            answered,
            expected_refusal(&session.arn, STREAM),
            "case {case:?}: expected the refusal to name the session that made the call, got \
             {answered:?}"
        );
    }

    delete_role_policy(&harness, role, DENY_POLICY_NAME).await;
    for (case, session) in [
        ("the session minted before the deny", &before_attach),
        ("the session minted after it", &after_attach),
    ] {
        let response = harness.get_media(&session.access_key_id, &addressing).await;
        assert_eq!(
            status(&response),
            200,
            "case {case:?}: once the deny is removed, expected HTTP 200, got {} ({}) {:?}",
            status(&response),
            error_code(&response),
            message(&response)
        );
    }
}
