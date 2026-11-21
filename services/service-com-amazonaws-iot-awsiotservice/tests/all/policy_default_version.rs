// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `SetDefaultPolicyVersion`: which version of an IoT policy
//! is the OPERATIVE one, and what every reader of the policy resolves through it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: "Sets the specified version of the specified policy as the
//! policy's default (operative) version" — so after the call, the pointer every reader of
//! the policy resolves through names the version the request named: `GetPolicy` reports it
//! as `defaultVersionId` and hands back ITS document, `GetPolicyVersion` and
//! `ListPolicyVersions` flag exactly that one version as the default, and
//! `DeletePolicyVersion` holds THAT version back (409) while deleting any other (200). A
//! policy, or a version of it, the account does not hold is a 404 and writes nothing.
//!
//! Beside the pointer, the policy's `generationId` is asserted as what it is: an opaque
//! token derived from the policy's content, so it is stable across reads, it moves when a
//! write moves the pointer, and it comes back when a later write puts the content back.
//!
//! The table spans what the rule generalises over — WHICH version is pointed at (the
//! first, a middle one, the newest, and the one already in effect), how many versions the
//! policy holds (from one up to the most a policy may hold), how many times the pointer
//! has already moved, and whether the version was created with `setAsDefault` — rather
//! than one recorded request. Beside that, the pointer belongs to the policy the call
//! named: an account holding several policies keeps one pointer per policy.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MAX_POLICY_VERSIONS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "eu-central-1";

/// How many hex characters a policy's `generationId` carries.
const GENERATION_ID_CHARACTERS: usize = 64;

/// One emulator: a router with this service mounted on its own engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("a well-formed policy request");
        Reply::of(self.router.dispatch(request).await)
    }

    /// `POST /policies/{policyName}` — `CreatePolicy`, whose version is 1 and the default.
    async fn create_policy(&self, case: &str, policy_name: &str) {
        let reply = self
            .dispatch(
                "POST",
                &format!("/policies/{policy_name}"),
                Some(&json!({ "policyDocument": document(1) })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: CreatePolicy must answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// `POST /policies/{policyName}/version` — `CreatePolicyVersion`, answering the id.
    async fn create_version(
        &self,
        case: &str,
        policy_name: &str,
        index: usize,
        set_as_default: bool,
    ) -> String {
        let reply = self
            .dispatch(
                "POST",
                &format!("/policies/{policy_name}/version?setAsDefault={set_as_default}"),
                Some(&json!({ "policyDocument": document(index) })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: CreatePolicyVersion must answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["policyVersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no policyVersionId in {}", reply.text()))
            .to_string()
    }

    /// `PATCH /policies/{policyName}/version/{policyVersionId}` —
    /// `SetDefaultPolicyVersion`.
    async fn set_default(&self, policy_name: &str, version_id: &str) -> Reply {
        self.dispatch(
            "PATCH",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    /// `GET /policies/{policyName}` — `GetPolicy`.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.dispatch("GET", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `GET /policies/{policyName}/version/{policyVersionId}` — `GetPolicyVersion`.
    async fn get_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    /// `GET /policies/{policyName}/version` — `ListPolicyVersions`.
    async fn list_versions(&self, policy_name: &str) -> Reply {
        self.dispatch("GET", &format!("/policies/{policy_name}/version"), None)
            .await
    }

    /// `DELETE /policies/{policyName}/version/{policyVersionId}` — `DeletePolicyVersion`.
    async fn delete_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.dispatch(
            "DELETE",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("a response body is JSON ({error}): {}", self.text()))
    }

    /// One string member of the answer.
    fn member(&self, name: &str) -> String {
        self.json()[name]
            .as_str()
            .unwrap_or_else(|| panic!("no {name} in {}", self.text()))
            .to_string()
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The `(versionId, isDefaultVersion)` pairs a `ListPolicyVersions` answer carries.
    fn flagged_versions(&self) -> Vec<(String, bool)> {
        self.json()["policyVersions"]
            .as_array()
            .unwrap_or_else(|| panic!("no policyVersions in {}", self.text()))
            .iter()
            .map(|version| {
                (
                    version["versionId"].as_str().unwrap_or_default().to_string(),
                    version["isDefaultVersion"].as_bool().unwrap_or_default(),
                )
            })
            .collect()
    }
}

/// An IoT policy document allowing one action, numbered so each version differs.
fn document(index: usize) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["iot:Publish"],
            "Resource": [format!("arn:aws:iot:{REGION}:000000000000:topic/version-{index}")],
        }],
    })
    .to_string()
}

/// One row: which version of a three-version policy is pointed at, and whether the third
/// version was created as the default already.
struct Case {
    label: &'static str,
    policy: &'static str,
    /// The 1-based position among the policy's versions the row points at.
    pointed_at: usize,
    /// Whether the policy's last create already carried `setAsDefault=true`.
    created_default: bool,
}

/// RULE: the version the call names becomes the operative one for every reader of the
/// policy, and the version in effect is the one the delete holds back.
#[tokio::test]
async fn the_pointer_moves_and_every_reader_of_the_policy_follows_it() {
    // The rows span what the rule generalises over: WHICH of the policy's versions is
    // pointed at — the first, the middle one, the newest, and the one already in effect —
    // and whether the pointer was last moved by a create or by this call.
    let cases = [
        Case {
            label: "the policy's first version",
            policy: "first-version",
            pointed_at: 1,
            created_default: false,
        },
        Case {
            label: "a middle version",
            policy: "middle-version",
            pointed_at: 2,
            created_default: false,
        },
        Case {
            label: "the newest version",
            policy: "newest-version",
            pointed_at: 3,
            created_default: false,
        },
        Case {
            label: "the version already in effect, set again",
            policy: "already-default",
            pointed_at: 3,
            created_default: true,
        },
    ];

    for case in &cases {
        let emulator = Emulator::new();
        emulator.create_policy(case.label, case.policy).await;
        let second = emulator
            .create_version(case.label, case.policy, 2, false)
            .await;
        let third = emulator
            .create_version(case.label, case.policy, 3, case.created_default)
            .await;
        let versions = ["1".to_string(), second, third];
        let pointed = versions[case.pointed_at - 1].clone();

        // The pointer BEFORE the call, so the move is observed rather than assumed.
        let before = emulator.get_policy(case.policy).await;
        let generation_before = before.member("generationId");

        let pointed_at = emulator.set_default(case.policy, &pointed).await;
        assert_eq!(
            pointed_at.status,
            200,
            "case {}: SetDefaultPolicyVersion must answer 200, got {} body {}",
            case.label,
            pointed_at.status,
            pointed_at.text()
        );
        assert!(
            pointed_at.text().is_empty(),
            "case {}: the answer carries no body, got {}",
            case.label,
            pointed_at.text()
        );

        // `GetPolicy` reports the version in effect and hands back ITS document.
        let read = emulator.get_policy(case.policy).await;
        assert_eq!(
            read.member("defaultVersionId"),
            pointed,
            "case {}: GetPolicy must report the version just set, got {}",
            case.label,
            read.text()
        );
        assert_eq!(
            read.member("policyDocument"),
            document(case.pointed_at),
            "case {}: GetPolicy must hand back the operative version's document, got {}",
            case.label,
            read.text()
        );

        // Every version reader flags exactly the one version that is in effect.
        for (position, version_id) in versions.iter().enumerate() {
            let expected = version_id == &pointed;
            let version = emulator.get_version(case.policy, version_id).await;
            assert_eq!(
                version.json()["isDefaultVersion"].as_bool(),
                Some(expected),
                "case {}: GetPolicyVersion of version {version_id} (position {}) must report \
                 isDefaultVersion {expected}, got {}",
                case.label,
                position + 1,
                version.text()
            );
        }
        let listed = emulator.list_versions(case.policy).await.flagged_versions();
        let flagged: Vec<&String> = listed
            .iter()
            .filter(|(_, is_default)| *is_default)
            .map(|(version_id, _)| version_id)
            .collect();
        assert_eq!(
            flagged,
            vec![&pointed],
            "case {}: exactly the operative version is flagged, got {listed:?}",
            case.label
        );

        // The version in EFFECT is the one the delete holds back; every other goes.
        let refused = emulator.delete_version(case.policy, &pointed).await;
        assert_eq!(
            refused.status,
            409,
            "case {}: the operative version must be held back, got {} body {}",
            case.label,
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "DeleteConflictException",
            "case {}: got {}",
            case.label,
            refused.text()
        );
        for version_id in versions.iter().filter(|version_id| *version_id != &pointed) {
            let deleted = emulator.delete_version(case.policy, version_id).await;
            assert_eq!(
                deleted.status,
                200,
                "case {}: version {version_id} is not in effect and must be deletable, got {} \
                 body {}",
                case.label,
                deleted.status,
                deleted.text()
            );
        }

        // The generation token the policy now carries: 64 hex characters, stable across
        // reads, and not the one it carried before these writes.
        let generation_after = emulator.get_policy(case.policy).await.member("generationId");
        assert_eq!(
            generation_after.len(),
            GENERATION_ID_CHARACTERS,
            "case {}: expected a {GENERATION_ID_CHARACTERS}-character generationId, got \
             {generation_after:?}",
            case.label
        );
        assert!(
            generation_after
                .chars()
                .all(|character| matches!(character, '0'..='9' | 'a'..='f')),
            "case {}: expected lowercase hex, got {generation_after:?}",
            case.label
        );
        assert_ne!(
            generation_after, generation_before,
            "case {}: the deletes changed what the policy holds, so its generation moved",
            case.label
        );
        assert_eq!(
            emulator.get_policy(case.policy).await.member("generationId"),
            generation_after,
            "case {}: a read must not move the generation",
            case.label
        );
    }
}

/// RULE: a policy's generation token follows its CONTENT — moving the pointer moves it,
/// and moving the pointer back brings it back.
#[tokio::test]
async fn the_generation_token_follows_what_the_policy_holds() {
    let emulator = Emulator::new();
    let case = "a pointer moved away and back";
    emulator.create_policy(case, "generation").await;
    let second = emulator.create_version(case, "generation", 2, false).await;

    let first_generation = emulator.get_policy("generation").await.member("generationId");
    assert_eq!(
        emulator.set_default("generation", &second).await.status,
        200,
        "case {case}: the pointer must move"
    );
    let moved_generation = emulator.get_policy("generation").await.member("generationId");
    assert_ne!(
        moved_generation, first_generation,
        "case {case}: a moved pointer is a new generation"
    );
    assert_eq!(
        emulator.set_default("generation", "1").await.status,
        200,
        "case {case}: the pointer must move back"
    );
    assert_eq!(
        emulator.get_policy("generation").await.member("generationId"),
        first_generation,
        "case {case}: the policy holds what it held, so it is the same generation"
    );
}

/// RULE: a policy — or a version of one — the account does not hold is a 404 whose
/// sentence names what was missing, and nothing is written.
#[tokio::test]
async fn pointing_at_what_the_account_does_not_hold_is_not_found() {
    let emulator = Emulator::new();
    let case = "a policy with one version";
    emulator.create_policy(case, "only-one").await;

    // (case, the policy named, the version named, the sentence it must answer). The
    // POLICY miss and the VERSION miss are two sentences: the policy family's names the
    // resource, and the version family's names the policy and the version it addressed as
    // `name=` / `versionId=`, both written out here rather than taken from the crate.
    let rows = [
        (
            "a policy the account does not hold",
            "never-created",
            "1",
            "Policy never-created does not exist".to_string(),
        ),
        (
            "another name the account does not hold",
            "other",
            "9",
            "Policy other does not exist".to_string(),
        ),
        (
            "a version the policy does not hold",
            "only-one",
            "2",
            "Policy version does not exist (name=only-one, versionId=2)".to_string(),
        ),
        (
            "a far higher version",
            "only-one",
            "99",
            "Policy version does not exist (name=only-one, versionId=99)".to_string(),
        ),
    ];
    for (label, policy_name, version_id, message) in rows {
        let refused = emulator.set_default(policy_name, version_id).await;
        assert_eq!(
            refused.status,
            404,
            "case {label}: expected 404, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.member("message"),
            message,
            "case {label}: expected the message {message:?}, got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "ResourceNotFoundException",
            "case {label}: got {}",
            refused.text()
        );
    }

    // Nothing above moved the pointer the one policy carries.
    let read = emulator.get_policy("only-one").await;
    assert_eq!(
        read.member("defaultVersionId"),
        "1",
        "case {case}: a refused call writes nothing, got {}",
        read.text()
    );
}

/// RULE: whichever version the call names becomes the operative one, however many versions
/// the policy holds and however many times the pointer has already moved — the last call
/// decides, so a move overwrites the previous pointer rather than adding to it.
#[tokio::test]
async fn every_version_a_policy_holds_can_become_the_operative_one_in_turn() {
    let case = "a policy holding every version it may hold";
    let emulator = Emulator::new();
    emulator.create_policy(case, "at-the-cap").await;

    // The rows run from the policy's first version up to the most one may hold: the cap
    // this crate keeps a policy's versions under
    // (`service_com_amazonaws_iot_awsiotservice::state::MAX_POLICY_VERSIONS`), so the
    // table spans the whole range of positions a pointer can name, its last entry
    // included.
    let mut versions = vec!["1".to_string()];
    for index in 2..=MAX_POLICY_VERSIONS {
        versions.push(
            emulator
                .create_version(case, "at-the-cap", index, false)
                .await,
        );
    }
    assert_eq!(
        versions.len(),
        MAX_POLICY_VERSIONS,
        "case {case}: expected {MAX_POLICY_VERSIONS} versions to point at, got {versions:?}"
    );

    for (position, version_id) in versions.iter().enumerate() {
        let pointed_at = emulator.set_default("at-the-cap", version_id).await;
        assert_eq!(
            pointed_at.status,
            200,
            "case {case}: pointing at version {version_id} must answer 200, got {} body {}",
            pointed_at.status,
            pointed_at.text()
        );

        let read = emulator.get_policy("at-the-cap").await;
        assert_eq!(
            read.member("defaultVersionId"),
            *version_id,
            "case {case}: GetPolicy must report version {version_id} as the operative one, \
             got {}",
            read.text()
        );
        assert_eq!(
            read.member("policyDocument"),
            document(position + 1),
            "case {case}: GetPolicy must hand back version {version_id}'s document, got {}",
            read.text()
        );

        let listed = emulator.list_versions("at-the-cap").await.flagged_versions();
        let flagged: Vec<&String> = listed
            .iter()
            .filter(|(_, is_default)| *is_default)
            .map(|(listed_id, _)| listed_id)
            .collect();
        assert_eq!(
            flagged,
            vec![version_id],
            "case {case}: exactly version {version_id} is flagged after pointing at it, got \
             {listed:?}"
        );
    }
}

/// RULE: the operative version is a property of the policy the call named, so a call that
/// moves one policy's pointer — and a call refused for a version no policy holds — leaves
/// every other policy's pointer where it was.
///
/// The refusal rows here deliberately revisit the 404 that
/// [`pointing_at_what_the_account_does_not_hold_is_not_found`] states: what this case adds
/// is that the refusal moves NO policy's pointer in an account holding several.
#[tokio::test]
async fn the_operative_version_moves_only_for_the_policy_the_call_named() {
    let case = "three policies holding different numbers of versions";
    let emulator = Emulator::new();

    // (the policy, how many versions it holds): the rows differ in version count, so the
    // policy the call names is not distinguishable by its shape.
    let policies = [("one-version", 1usize), ("two-versions", 2), ("newest", 3)];
    for (policy, held) in policies {
        emulator.create_policy(case, policy).await;
        for index in 2..=held {
            emulator.create_version(case, policy, index, false).await;
        }
    }

    // Each row moves ONE policy's pointer, and names what every policy must report
    // afterwards.
    let rows = [
        PointerMove {
            label: "the three-version policy is pointed at its middle version",
            moved: "newest",
            version_id: "2",
            operative: [("one-version", "1"), ("two-versions", "1"), ("newest", "2")],
        },
        PointerMove {
            label: "the two-version policy is pointed at its newest version",
            moved: "two-versions",
            version_id: "2",
            operative: [("one-version", "1"), ("two-versions", "2"), ("newest", "2")],
        },
        PointerMove {
            label: "the one-version policy is pointed at the version already in effect",
            moved: "one-version",
            version_id: "1",
            operative: [("one-version", "1"), ("two-versions", "2"), ("newest", "2")],
        },
        PointerMove {
            label: "the three-version policy is pointed at its newest version",
            moved: "newest",
            version_id: "3",
            operative: [("one-version", "1"), ("two-versions", "2"), ("newest", "3")],
        },
    ];
    for row in &rows {
        let pointed_at = emulator.set_default(row.moved, row.version_id).await;
        assert_eq!(
            pointed_at.status,
            200,
            "case {}: expected 200, got {} body {}",
            row.label,
            pointed_at.status,
            pointed_at.text()
        );
        assert_operative_versions(row.label, &emulator, &row.operative).await;
    }

    // A version the policy does NOT hold is the version miss, and it moves no policy's
    // pointer. The rows differ in the FORM of the id: one the policy could hold but does
    // not (one past its newest), and one no policy can ever hold because the number is
    // larger than any version id this service mints.
    for (label, version_id) in [
        ("one past the policy's newest version", "4".to_string()),
        (
            "a number past every version id a policy is given",
            "9".repeat(20),
        ),
    ] {
        let refused = emulator.set_default("newest", &version_id).await;
        assert_eq!(
            refused.status,
            404,
            "case {label}: a version the policy does not hold is a 404, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.member("message"),
            format!("Policy version does not exist (name=newest, versionId={version_id})"),
            "case {label}: got {}",
            refused.text()
        );
        assert_operative_versions(
            &format!("{label}, so the refused call wrote nothing"),
            &emulator,
            &[("one-version", "1"), ("two-versions", "2"), ("newest", "3")],
        )
        .await;
    }
}

/// One row of the isolation table: the policy whose pointer the row moves, the version it
/// names, and the version every policy in the account must report as its operative one
/// afterwards.
struct PointerMove {
    label: &'static str,
    moved: &'static str,
    /// The version this row's call names.
    version_id: &'static str,
    /// `(policy, the version it reports as operative)`, one entry per policy.
    operative: [(&'static str, &'static str); 3],
}

/// Assert what each policy named reports as its operative version, and that its document
/// is that version's.
async fn assert_operative_versions(
    case: &str,
    emulator: &Emulator,
    expected: &[(&'static str, &'static str)],
) {
    for (policy, version_id) in expected {
        let read = emulator.get_policy(policy).await;
        assert_eq!(
            read.member("defaultVersionId"),
            *version_id,
            "case {case}: {policy} must report version {version_id} as the operative one, \
             got {}",
            read.text()
        );
        let position: usize = version_id
            .parse()
            .expect("an operative version id this suite asserts is a number");
        assert_eq!(
            read.member("policyDocument"),
            document(position),
            "case {case}: {policy} must hand back version {version_id}'s document, got {}",
            read.text()
        );
    }
}
