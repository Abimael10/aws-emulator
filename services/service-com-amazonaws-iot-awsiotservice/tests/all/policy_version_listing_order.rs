// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the ORDER `ListPolicyVersions` reports a policy's versions
//! in, and of which of them it flags as the operative one.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a listing reports exactly the versions the policy HOLDS,
//! NEWEST-first (descending `versionId`, which is the order a caller reading
//! `policyVersions[0]` takes as the policy's newest version), and flags
//! `isDefaultVersion` on exactly the one version that is in effect — whichever of them a
//! create or a `SetDefaultPolicyVersion` last pointed the policy at.
//!
//! The table spans what the rule generalises over rather than one recorded request: how
//! many versions the policy holds (one, two, three and the cap), whether the ids it holds
//! are contiguous (a non-default version deleted leaves a gap, and a create after that
//! takes a fresh id), and which version is in effect (the first, a middle one, the newest,
//! and one re-pointed afterwards).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MAX_POLICY_VERSIONS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "us-west-2";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// One string member of the body, or the empty string when it is absent.
    fn member(&self, name: &str) -> String {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `(versionId, isDefaultVersion)` pairs a `ListPolicyVersions` answer carries, in
    /// the order it reported them.
    fn flagged_versions(&self) -> Vec<(String, bool)> {
        self.body
            .get("policyVersions")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("no policyVersions in {:?}", self.body))
            .iter()
            .map(|version| {
                (
                    version
                        .get("versionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    version
                        .get("isDefaultVersion")
                        .and_then(Value::as_bool)
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The `versionId`s a `ListPolicyVersions` answer carries, in its order.
    fn version_ids(&self) -> Vec<String> {
        self.flagged_versions()
            .into_iter()
            .map(|(version_id, _)| version_id)
            .collect()
    }

    /// The `versionId`s the answer flags as the operative version.
    fn default_version_ids(&self) -> Vec<String> {
        self.flagged_versions()
            .into_iter()
            .filter(|(_, is_default)| *is_default)
            .map(|(version_id, _)| version_id)
            .collect()
    }
}

/// One emulator: the shared engine plus a router with this service mounted.
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`, whose document becomes version 1.
    async fn create_policy(&self, policy_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": document(1) })),
        )
        .await
    }

    /// `CreatePolicyVersion`: `POST /policies/{policyName}/version`.
    async fn create_version(&self, policy_name: &str, index: usize, set_as_default: bool) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}/version"),
            Some(&json!({
                "policyDocument": document(index),
                "setAsDefault": set_as_default,
            })),
        )
        .await
    }

    /// `SetDefaultPolicyVersion`: `PATCH /policies/{policyName}/version/{policyVersionId}`.
    async fn set_default(&self, policy_name: &str, version_id: &str) -> Reply {
        self.call(
            "PATCH",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    /// `DeletePolicyVersion`: `DELETE /policies/{policyName}/version/{policyVersionId}`.
    async fn delete_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    /// `ListPolicyVersions`: `GET /policies/{policyName}/version`.
    async fn list_versions(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}/version"), None)
            .await
    }

    /// `GetPolicy`: `GET /policies/{policyName}`, which reports the operative version.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}"), None)
            .await
    }
}

/// An IoT policy document allowing one action, numbered so each version differs.
fn document(index: usize) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["iot:Publish"],
            "Resource": [format!("arn:aws:iot:{REGION}:000000000000:topic/order-{index}")],
        }],
    })
    .to_string()
}

/// One row: how the policy is built up, and which version ends up in effect.
struct Case {
    label: &'static str,
    policy: &'static str,
    /// How many versions to create beyond the one the create wrote.
    extra_versions: usize,
    /// The version ids to delete afterwards, which leaves gaps in what the policy holds.
    deleted: &'static [&'static str],
    /// Whether another version is created after those deletes — it takes a FRESH id, one
    /// past the highest the policy ever held.
    creates_after_delete: bool,
    /// The version the policy is pointed at before the listing is read, when the row
    /// moves the pointer at all.
    pointed_at: Option<&'static str>,
}

/// RULE: a listing reports exactly the versions the policy holds, newest-first, with
/// `isDefaultVersion` on exactly the operative one.
#[tokio::test]
async fn the_listing_reports_the_versions_held_newest_first_flagging_the_operative_one() {
    // The rows differ in how many versions the policy holds, whether its ids are
    // contiguous, and which version is in effect.
    let cases = [
        Case {
            label: "one version, the one the create wrote",
            policy: "single",
            extra_versions: 0,
            deleted: &[],
            creates_after_delete: false,
            pointed_at: None,
        },
        Case {
            label: "two versions, the newest in effect",
            policy: "pair",
            extra_versions: 1,
            deleted: &[],
            creates_after_delete: false,
            pointed_at: Some("2"),
        },
        Case {
            label: "three versions, the first in effect",
            policy: "three-first",
            extra_versions: 2,
            deleted: &[],
            creates_after_delete: false,
            pointed_at: Some("1"),
        },
        Case {
            label: "three versions, a middle one in effect",
            policy: "three-middle",
            extra_versions: 2,
            deleted: &[],
            creates_after_delete: false,
            pointed_at: Some("2"),
        },
        Case {
            label: "the cap, a middle version in effect",
            policy: "at-the-cap",
            extra_versions: MAX_POLICY_VERSIONS - 1,
            deleted: &[],
            creates_after_delete: false,
            pointed_at: Some("3"),
        },
        Case {
            label: "the cap with two non-default versions deleted",
            policy: "with-gaps",
            extra_versions: MAX_POLICY_VERSIONS - 1,
            deleted: &["2", "4"],
            creates_after_delete: false,
            pointed_at: Some("3"),
        },
        Case {
            label: "a deleted version replaced by one under a fresh id",
            policy: "fresh-id",
            extra_versions: MAX_POLICY_VERSIONS - 1,
            deleted: &["1", "2"],
            creates_after_delete: true,
            pointed_at: Some("5"),
        },
        Case {
            label: "only the operative version left",
            policy: "last-one-standing",
            extra_versions: 2,
            deleted: &["1", "3"],
            creates_after_delete: false,
            pointed_at: Some("2"),
        },
    ];

    for case in &cases {
        let emulator = Emulator::new();
        let created = emulator.create_policy(case.policy).await;
        assert_eq!(
            created.status, 200,
            "case {}: the create must answer 200, got {} {:?}",
            case.label, created.status, created.body
        );
        // The ids the policy holds, tracked the way a caller would: from what each write
        // answered, never assumed.
        let mut held = vec!["1".to_string()];
        for index in 2..=(case.extra_versions + 1) {
            let version = emulator.create_version(case.policy, index, false).await;
            assert_eq!(
                version.status, 200,
                "case {}: creating version {index} must answer 200, got {} {:?}",
                case.label, version.status, version.body
            );
            held.push(version.member("policyVersionId"));
        }

        // The pointer is moved BEFORE the deletes, because the version in effect is the
        // one a delete holds back.
        if let Some(version_id) = case.pointed_at {
            let pointed = emulator.set_default(case.policy, version_id).await;
            assert_eq!(
                pointed.status, 200,
                "case {}: pointing at version {version_id} must answer 200, got {} {:?}",
                case.label, pointed.status, pointed.body
            );
        }
        for version_id in case.deleted {
            let deleted = emulator.delete_version(case.policy, version_id).await;
            assert_eq!(
                deleted.status, 200,
                "case {}: deleting version {version_id} must answer 200, got {} {:?}",
                case.label, deleted.status, deleted.body
            );
            held.retain(|id| id != version_id);
        }
        if case.creates_after_delete {
            let version = emulator
                .create_version(case.policy, MAX_POLICY_VERSIONS + 1, false)
                .await;
            assert_eq!(
                version.status, 200,
                "case {}: the create after the deletes must answer 200, got {} {:?}",
                case.label, version.status, version.body
            );
            held.push(version.member("policyVersionId"));
        }

        // What the listing must report: the ids the policy holds, newest-first — the
        // numeric order reversed, which is not the order the writes happened in once a
        // delete and a later create have moved ids around.
        let mut expected = held.clone();
        expected.sort_by_key(|version_id| {
            ::std::cmp::Reverse(
                version_id
                    .parse::<u32>()
                    .expect("every version id this suite tracks is a number"),
            )
        });

        let listed = emulator.list_versions(case.policy).await;
        assert_eq!(
            listed.status, 200,
            "case {}: ListPolicyVersions must answer 200, got {} {:?}",
            case.label, listed.status, listed.body
        );
        assert_eq!(
            listed.version_ids(),
            expected,
            "case {}: expected the versions held, newest-first, got {:?}",
            case.label,
            listed.flagged_versions()
        );

        // Exactly the operative version is flagged, and it is the one the policy's own
        // reader reports.
        let operative = emulator.get_policy(case.policy).await.member("defaultVersionId");
        assert_eq!(
            listed.default_version_ids(),
            vec![operative.clone()],
            "case {}: expected version {operative} alone to be flagged, got {:?}",
            case.label,
            listed.flagged_versions()
        );
        if let Some(version_id) = case.pointed_at {
            assert_eq!(
                operative, version_id,
                "case {}: the version pointed at must be the operative one, got {operative}",
                case.label
            );
        }
    }
}

/// RULE: the listing's order is a property of the ids the policy HOLDS, not of the order
/// its versions were written or re-pointed in — so moving the pointer around reorders
/// nothing.
#[tokio::test]
async fn moving_the_pointer_does_not_reorder_the_listing() {
    let emulator = Emulator::new();
    let case = "a policy at the cap, pointed at each version in turn";
    assert_eq!(
        emulator.create_policy("stable-order").await.status,
        200,
        "case {case}: the create must answer 200"
    );
    for index in 2..=MAX_POLICY_VERSIONS {
        assert_eq!(
            emulator
                .create_version("stable-order", index, false)
                .await
                .status,
            200,
            "case {case}: creating version {index} must answer 200"
        );
    }

    // Newest-first, whatever the pointer is doing: the ids the policy holds, descending.
    let expected: Vec<String> = (1..=MAX_POLICY_VERSIONS)
        .rev()
        .map(|index| index.to_string())
        .collect();

    // Every version in turn, and then back to the first: the order is the same each time
    // and only the flagged entry moves.
    let pointed_in_turn: Vec<String> = (1..=MAX_POLICY_VERSIONS)
        .map(|index| index.to_string())
        .chain(["1".to_string()])
        .collect();
    for version_id in &pointed_in_turn {
        assert_eq!(
            emulator.set_default("stable-order", version_id).await.status,
            200,
            "case {case}: pointing at version {version_id} must answer 200"
        );
        let listed = emulator.list_versions("stable-order").await;
        assert_eq!(
            listed.version_ids(),
            expected,
            "case {case}: pointing at version {version_id} must not reorder the listing, \
             got {:?}",
            listed.flagged_versions()
        );
        assert_eq!(
            listed.default_version_ids(),
            vec![version_id.clone()],
            "case {case}: expected version {version_id} alone to be flagged, got {:?}",
            listed.flagged_versions()
        );
    }
}
