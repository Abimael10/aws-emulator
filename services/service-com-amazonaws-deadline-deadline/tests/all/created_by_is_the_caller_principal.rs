// SPDX-License-Identifier: CC0-1.0

//! WHICH PRINCIPAL this service records as a resource's author, over the wire.
//!
//! The rule under test: an audit member naming who created or last updated a resource —
//! `createdBy` and `updatedBy`, "the user or system that created this resource"
//! (`smithy_query(query="shape", shape="CreatedBy")`) — reports the IDENTITY the request's
//! credential speaks for, the one `sts:GetCallerIdentity` answers for that same request,
//! never the credential material the request was signed with. So:
//!
//! * a key that names a live session speaks for that session's principal — for an assumed
//!   role the `assumed-role` ARN, for an `sts:GetSessionToken` session the IAM user that
//!   asked for it;
//! * a key that names no session is a long-term key and speaks for the IAM user the account
//!   holds it for;
//! * a key no store of the account holds at all — the position of the long-term key this
//!   emulator publishes at start — speaks for the account's own published identity;
//! * a session whose expiry the clock has passed resolves to that long-term answer.
//!
//! Two consequences the rows drive end to end: the value a caller reads is one it can
//! compare with its own identity (so it never contains the access key id of the signing
//! credential), and it does not move when the same principal signs with another credential,
//! while two different principals ARE told apart — an update by a second credential records
//! that second principal in `updatedBy` and leaves `createdBy` alone.
//!
//! The rows span what the rule generalises over: the kind of credential a call is signed
//! with, the resource type the author is recorded on (farm, queue, queue environment,
//! fleet), and the reader it is reported through. The sessions are written into the account
//! registries the way `sts:AssumeRole`, `sts:GetSessionToken` and `iam:CreateAccessKey`
//! record one, because what is under test is what THIS service does with the principal a
//! credential names, not how the credential was minted.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

use crate::farm_queue_job::{caller, string_at, Emulator, DEFAULT_ACCESS_KEY_ID};
use crate::fleet::{create_fleet_path, customer_managed, get_fleet_path, ROLE_ARN};

/// How long a seeded live session lasts: longer than any row takes to run, so none of them
/// expires while the calls it signs are being served.
const SESSION_LIFETIME: Duration = Duration::from_secs(3600);

/// The emulator's account: every identity named here belongs to it.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// One session of an assumed role, as STS reports it:
/// `arn:aws:sts::<account>:assumed-role/<role>/<session>`.
fn assumed_role_arn(role: &str, session: &str) -> String {
    format!("arn:aws:sts::{}:assumed-role/{role}/{session}", account())
}

/// The ARN of the IAM role a session was minted for.
fn role_arn(role: &str) -> String {
    format!("arn:aws:iam::{}:role/{role}", account())
}

/// An IAM user of this account.
fn user_arn(user: &str) -> String {
    format!("arn:aws:iam::{}:user/{user}", account())
}

/// Which credential a call is signed with — and so which principal this service must
/// record for it.
#[derive(Debug, Clone, Copy)]
enum Credential {
    /// A long-term key no store of the account holds anything for.
    UnheldLongTerm,
    /// A long-term key the account holds for an IAM user, as `iam:CreateAccessKey`
    /// records one.
    IamUserKey { user: &'static str },
    /// The temporary credentials of an assumed role, live in the session registry.
    AssumedRole {
        role: &'static str,
        session: &'static str,
    },
    /// The temporary credentials of an `sts:GetSessionToken` session, which speak for the
    /// IAM user that asked for them.
    SessionToken { user: &'static str },
    /// The temporary credentials of an assumed role whose expiry the clock has passed.
    ExpiredAssumedRole {
        role: &'static str,
        session: &'static str,
    },
}

impl Credential {
    /// The principal a request signed with this credential speaks for.
    fn principal(self) -> String {
        match self {
            Self::UnheldLongTerm | Self::ExpiredAssumedRole { .. } => caller(),
            Self::IamUserKey { user } | Self::SessionToken { user } => user_arn(user),
            Self::AssumedRole { role, session } => assumed_role_arn(role, session),
        }
    }

    /// Write the record the account holds for this credential into the shared engine, the
    /// way the operation that mints one records it. A key nothing holds needs no record.
    fn register(self, engine: &engine::InMemoryEngine, access_key_id: &str) {
        let now = SystemTime::now();
        let (identity, kind, role, expires_at) = match self {
            Self::UnheldLongTerm => return,
            Self::IamUserKey { user } => {
                hold_long_term_key_for(engine, user, access_key_id);
                return;
            }
            Self::AssumedRole { role, session } => (
                sts::Identity {
                    user_id: format!("{}:{session}", sts::role_id(&role_arn(role))),
                    account: account(),
                    arn: assumed_role_arn(role, session),
                },
                sts::SessionKind::AssumedRole,
                Some(role_arn(role)),
                now + SESSION_LIFETIME,
            ),
            Self::SessionToken { user } => (
                sts::Identity {
                    user_id: sts::unique_id("AIDA", &user_arn(user)),
                    account: account(),
                    arn: user_arn(user),
                },
                sts::SessionKind::SessionToken,
                None,
                now + SESSION_LIFETIME,
            ),
            Self::ExpiredAssumedRole { role, session } => (
                sts::Identity {
                    user_id: format!("{}:{session}", sts::role_id(&role_arn(role))),
                    account: account(),
                    arn: assumed_role_arn(role, session),
                },
                sts::SessionKind::AssumedRole,
                Some(role_arn(role)),
                // One second BEHIND the clock: an expiry the clock has already passed.
                now - Duration::from_secs(1),
            ),
        };
        let vend = sts::NewSession {
            role_arn: role,
            ..sts::NewSession::new(identity, kind, SESSION_LIFETIME.as_secs() as i32)
        };
        let registry =
            sts::service_state(engine).expect("the in-memory engine carries the STS store");
        registry
            .lock()
            .insert(sts::Session {
                // The vend's own clock, which the rows vary: the credential was handed out
                // a lifetime ago and stops being valid at `expires_at`. Everything else
                // about the record is what the registry's own constructor builds.
                created_at: now - SESSION_LIFETIME,
                expires_at,
                ..vend.into_session(access_key_id, now)
            })
            .expect("the session registry has room for one session");
    }
}

/// Make `access_key_id` a long-term access key the ACCOUNT HOLDS for the IAM user
/// `user_name` — the user record `iam:CreateUser` writes and the key record
/// `iam:CreateAccessKey` writes, in the one shared engine the resolution reads them from.
fn hold_long_term_key_for(
    engine: &engine::InMemoryEngine,
    user_name: &str,
    access_key_id: &str,
) {
    let arn = user_arn(user_name);
    let state = iam::service_state(engine).expect("the in-memory engine carries the IAM store");
    let mut account = state.lock();
    account.users.insert(
        iam::name_key(user_name),
        User {
            path: "/".to_string(),
            user_name: user_name.to_string(),
            user_id: sts::unique_id("AIDA", &arn),
            arn,
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    account.access_keys.insert(
        access_key_id.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(user_name),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
        },
    );
}

/// One row: the credential a call is signed with, and the access key id it presents.
struct Row {
    case: &'static str,
    access_key_id: &'static str,
    credential: Credential,
}

/// The rows: one per kind of credential the rule distinguishes, plus the harness's own key
/// and a second unheld key, which have to resolve to the SAME principal even though they
/// are different credentials.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the harness's own long-term key, which no store holds",
            access_key_id: DEFAULT_ACCESS_KEY_ID,
            credential: Credential::UnheldLongTerm,
        },
        Row {
            case: "another long-term key no store holds",
            access_key_id: "AKIDSECONDKEY",
            credential: Credential::UnheldLongTerm,
        },
        Row {
            case: "a long-term key the account holds for an IAM user",
            access_key_id: "AKIDHELDKEY",
            credential: Credential::IamUserKey { user: "Mary_Major" },
        },
        Row {
            case: "an assumed role's session credential",
            access_key_id: "ASIAROLESESSION",
            credential: Credential::AssumedRole {
                role: "DeadlineArtist",
                session: "render-1",
            },
        },
        Row {
            case: "a session-token credential of an IAM user",
            access_key_id: "ASIAUSERSESSION",
            credential: Credential::SessionToken { user: "Li_Juan" },
        },
        Row {
            case: "an assumed role's session credential past its expiry",
            access_key_id: "ASIAEXPIREDSESSION",
            credential: Credential::ExpiredAssumedRole {
                role: "DeadlineArtist",
                session: "render-2",
            },
        },
    ]
}

/// An environment template naming the environment it describes, which is what
/// `GetQueueEnvironment` reports as the environment's `name`.
fn environment_template(name: &str) -> String {
    json!({
        "specificationVersion": "environment-2023-09",
        "environment": {"name": name, "variables": {"SEAT": "1"}},
    })
    .to_string()
}

/// Read `path` as `access_key_id` and fail the test unless it is served.
async fn read_as(emulator: &Emulator, access_key_id: &str, case: &str, path: &str) -> Value {
    let (status, body) = emulator
        .call_as(access_key_id, "GET", path, None, None)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GET {path}, got {status} with {body}"
    );
    body
}

/// Create `path` with `body` as `access_key_id` and fail the test unless it is accepted.
async fn create_as(
    emulator: &Emulator,
    access_key_id: &str,
    case: &str,
    path: &str,
    body: Value,
) -> Value {
    let (status, answer) = emulator
        .call_as(access_key_id, "POST", path, Some(body), None)
        .await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected a success from POST {path}, got {status} with {answer}"
    );
    answer
}

/// Assert `member` of `document` is the principal `expected` — and, whatever it is, that it
/// is not built out of the access key id the call was signed with.
fn assert_principal(
    case: &str,
    member: &str,
    document: &Value,
    expected: &str,
    access_key_id: &str,
) {
    let actual = string_at(document, member);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {member} to be the calling principal {expected}, got \
         {actual:?} in {document}"
    );
    assert!(
        !actual.contains(access_key_id),
        "case {case:?}: expected {member} to name a principal rather than the signing \
         credential {access_key_id}, got {actual:?}"
    );
}

#[tokio::test]
async fn created_by_is_the_principal_the_signing_credential_speaks_for() {
    for row in rows() {
        let emulator = Emulator::new();
        row.credential.register(emulator.engine(), row.access_key_id);
        let case = row.case;
        let expected = row.credential.principal();

        // A farm, a queue under it and a queue environment under that, all created by the
        // row's credential: three resource types whose author this service stamps, each
        // read back through its own reader.
        let farm = create_as(
            &emulator,
            row.access_key_id,
            case,
            "/2023-10-12/farms",
            json!({"displayName": "authored"}),
        )
        .await;
        let farm_id = string_at(&farm, "farmId");
        let queue = create_as(
            &emulator,
            row.access_key_id,
            case,
            &format!("/2023-10-12/farms/{farm_id}/queues"),
            json!({"displayName": "authored"}),
        )
        .await;
        let queue_id = string_at(&queue, "queueId");
        let environments = format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/environments");
        let environment = create_as(
            &emulator,
            row.access_key_id,
            case,
            &environments,
            json!({
                "priority": 50,
                "templateType": "JSON",
                "template": environment_template("seats"),
            }),
        )
        .await;
        let environment_id = string_at(&environment, "queueEnvironmentId");

        for (member_path, reader) in [
            ("farm", format!("/2023-10-12/farms/{farm_id}")),
            ("queue", format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}")),
            (
                "queue environment",
                format!("{environments}/{environment_id}"),
            ),
        ] {
            let read = read_as(&emulator, row.access_key_id, case, &reader).await;
            assert_principal(
                &format!("{case} / {member_path}"),
                "createdBy",
                &read,
                &expected,
                row.access_key_id,
            );
        }

        // The author is stored once, so a later read — and a read made by a DIFFERENT
        // credential — reports the same principal.
        let environment_path = format!("{environments}/{environment_id}");
        let again = read_as(&emulator, "AKIDTHIRDPARTY", case, &environment_path).await;
        assert_principal(
            &format!("{case} / re-read by another credential"),
            "createdBy",
            &again,
            &expected,
            row.access_key_id,
        );
    }
}

#[tokio::test]
async fn an_update_records_its_own_caller_and_leaves_the_author_alone() {
    // Two principals of one account: a role session creates the fleet, an IAM user's own
    // long-term key updates it. The rule has to tell them apart.
    let author = Row {
        case: "created by a role session",
        access_key_id: "ASIAFLEETSESSION",
        credential: Credential::AssumedRole {
            role: "DeadlineArtist",
            session: "render-3",
        },
    };
    let updater = Row {
        case: "updated by an IAM user's key",
        access_key_id: "AKIDFLEETUPDATER",
        credential: Credential::IamUserKey { user: "Mary_Major" },
    };

    let emulator = Emulator::new();
    row_register(&emulator, &author);
    row_register(&emulator, &updater);
    let case = "a fleet created by one principal and updated by another";

    let farm = create_as(
        &emulator,
        author.access_key_id,
        case,
        "/2023-10-12/farms",
        json!({"displayName": "two principals"}),
    )
    .await;
    let farm_id = string_at(&farm, "farmId");
    let fleet = create_as(
        &emulator,
        author.access_key_id,
        case,
        &create_fleet_path(&farm_id),
        json!({
            "displayName": "authored by the session",
            "roleArn": ROLE_ARN,
            "maxWorkerCount": 4,
            "configuration": customer_managed("LINUX", "x86_64", json!({})),
        }),
    )
    .await;
    let fleet_id = string_at(&fleet, "fleetId");
    let fleet_path = get_fleet_path(&farm_id, &fleet_id);

    let created = read_as(&emulator, author.access_key_id, case, &fleet_path).await;
    assert_principal(
        case,
        "createdBy",
        &created,
        &author.credential.principal(),
        author.access_key_id,
    );
    assert!(
        created.get("updatedBy").is_none(),
        "case {case:?}: expected no updatedBy before an update, got {created}"
    );

    let (status, updated) = emulator
        .call_as(
            updater.access_key_id,
            "PATCH",
            &fleet_path,
            Some(json!({"description": "updated by the user"})),
            None,
        )
        .await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected UpdateFleet to be accepted, got {status} with {updated}"
    );

    let after = read_as(&emulator, updater.access_key_id, case, &fleet_path).await;
    assert_principal(
        case,
        "createdBy",
        &after,
        &author.credential.principal(),
        author.access_key_id,
    );
    assert_principal(
        case,
        "updatedBy",
        &after,
        &updater.credential.principal(),
        updater.access_key_id,
    );
}

/// Write the account's record of one row's credential into the shared engine.
fn row_register(emulator: &Emulator, row: &Row) {
    row.credential.register(emulator.engine(), row.access_key_id);
}
