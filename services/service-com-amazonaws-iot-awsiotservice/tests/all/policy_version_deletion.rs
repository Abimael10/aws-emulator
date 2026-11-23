// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DeletePolicyVersion`: which version of an IoT policy a
//! delete may remove, and what holds the delete of the version in EFFECT back.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * "You cannot delete the default version of a policy using this action. To delete
//!   the default version of a policy, use DeletePolicy": the delete of the version in
//!   effect is refused `DeleteConflictException` (409) and the version REMAINS;
//! * every other version is removed, and the readers of the policy's versions stop
//!   reporting it while the policy itself and its default version stand;
//! * a policy the account does not hold, and a version it does not hold, are each
//!   `ResourceNotFoundException` (404).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "sa-east-1";

/// One emulator: a router with this service mounted on its own engine.
///
/// Shared with the sibling suite that pins this operation's refusal sentences
/// (`crate::policy_version_deletion_refusals`), so the wire harness lives in one place.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    pub(crate) async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
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
            .expect("the policy request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    pub(crate) async fn create_policy(&self, case: &str, policy_name: &str) {
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
            "case {case}: expected CreatePolicy to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// Add one version, answering the id it was given.
    pub(crate) async fn create_version(
        &self,
        case: &str,
        policy_name: &str,
        index: usize,
        default: bool,
    ) -> String {
        let reply = self
            .dispatch(
                "POST",
                &format!("/policies/{policy_name}/version?setAsDefault={default}"),
                Some(&json!({ "policyDocument": document(index) })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreatePolicyVersion to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["policyVersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no policyVersionId in {}", reply.text()))
            .to_string()
    }

    pub(crate) async fn delete_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.dispatch(
            "DELETE",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    pub(crate) async fn get_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    pub(crate) async fn list_versions(&self, policy_name: &str) -> Reply {
        self.dispatch("GET", &format!("/policies/{policy_name}/version"), None)
            .await
    }

    async fn delete_policy(&self, policy_name: &str) -> Reply {
        self.dispatch("DELETE", &format!("/policies/{policy_name}"), None)
            .await
    }
}

/// A dispatched response, reduced to what the assertions read.
pub(crate) struct Reply {
    pub(crate) status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    pub(crate) fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The version ids a `ListPolicyVersions` answer carries.
    pub(crate) fn version_ids(&self) -> Vec<String> {
        self.json()["policyVersions"]
            .as_array()
            .unwrap_or_else(|| panic!("no policyVersions in {}", self.text()))
            .iter()
            .map(|version| version["versionId"].as_str().unwrap_or_default().to_string())
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

/// RULE: the version in EFFECT cannot be deleted and remains; every other version is
/// removed and stops being reported.
#[tokio::test]
async fn the_default_version_is_held_back_and_every_other_version_is_deleted() {
    // The rows span what the rule generalises over: WHICH version is the default one —
    // the policy's first, and a later one a create pointed at — so the refusal follows
    // the pointer rather than an id.
    struct Case {
        label: &'static str,
        policy: &'static str,
        /// Whether the second create moves the default pointer to its own version.
        move_default: bool,
    }
    let cases = [
        Case {
            label: "the policy's first version is the default",
            policy: "first-default",
            move_default: false,
        },
        Case {
            label: "a later version is the default",
            policy: "later-default",
            move_default: true,
        },
    ];

    for case in &cases {
        let emulator = Emulator::new();
        emulator.create_policy(case.label, case.policy).await;
        let second = emulator
            .create_version(case.label, case.policy, 2, case.move_default)
            .await;
        let third = emulator
            .create_version(case.label, case.policy, 3, false)
            .await;
        let default_version = if case.move_default { second.clone() } else { "1".to_string() };
        let others: Vec<String> = ["1".to_string(), second.clone(), third.clone()]
            .into_iter()
            .filter(|version_id| *version_id != default_version)
            .collect();

        // The version in effect is held back, and it is STILL there afterwards.
        let refused = emulator.delete_version(case.policy, &default_version).await;
        assert_eq!(
            refused.status,
            409,
            "case {}: expected 409 for the default version, got {} body {}",
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
        assert_eq!(
            emulator
                .get_version(case.policy, &default_version)
                .await
                .status,
            200,
            "case {}: the refused delete must leave the version in place",
            case.label
        );

        // Every other version is deleted, and the readers stop reporting it.
        for version_id in &others {
            let deleted = emulator.delete_version(case.policy, version_id).await;
            assert_eq!(
                deleted.status,
                200,
                "case {}: expected version {version_id} to be deletable, got {} body {}",
                case.label,
                deleted.status,
                deleted.text()
            );
            let read = emulator.get_version(case.policy, version_id).await;
            assert_eq!(
                read.status,
                404,
                "case {}: version {version_id} must be gone, got {} body {}",
                case.label,
                read.status,
                read.text()
            );
        }
        let listed = emulator.list_versions(case.policy).await.version_ids();
        assert_eq!(
            listed,
            vec![default_version.clone()],
            "case {}: expected only the default version left, got {listed:?}",
            case.label
        );

        // A repeat of the delete of a version that is gone is the same 404 as a version
        // the policy never held.
        for version_id in others.iter().cloned().chain(["9".to_string()]) {
            let again = emulator.delete_version(case.policy, &version_id).await;
            assert_eq!(
                again.status,
                404,
                "case {}: expected 404 for version {version_id}, got {} body {}",
                case.label,
                again.status,
                again.text()
            );
            assert_eq!(
                again.error_code(),
                "ResourceNotFoundException",
                "case {}: got {}",
                case.label,
                again.text()
            );
        }

        // With only the default version left, the policy itself can be deleted — which
        // is what the deletes above really removed.
        let deleted_policy = emulator.delete_policy(case.policy).await;
        assert_eq!(
            deleted_policy.status,
            200,
            "case {}: expected the policy to be deletable once only its default version \
             remains, got {} body {}",
            case.label,
            deleted_policy.status,
            deleted_policy.text()
        );
    }
}

/// RULE: a policy the account does not hold is a 404, whichever version the request
/// names.
#[tokio::test]
async fn a_version_of_a_policy_the_account_does_not_hold_is_a_404() {
    let emulator = Emulator::new();
    for version_id in ["1", "2", "99"] {
        let refused = emulator.delete_version("no-such-policy", version_id).await;
        assert_eq!(
            refused.status,
            404,
            "case version {version_id} of an absent policy: expected 404, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "ResourceNotFoundException",
            "case version {version_id} of an absent policy: got {}",
            refused.text()
        );
    }
}
