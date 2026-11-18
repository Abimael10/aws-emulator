// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DeletePolicy`: what this account's state has to say
//! before an IoT policy is removed, and what removing one does to the registry every
//! other policy operation reads.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rule under test: a delete that another record in this account's state still
//! depends on is REFUSED the modelled `DeleteConflictException` (HTTP 409) and removes
//! nothing, for as long as any such dependency remains — so the refusal is idempotent
//! and clearing one of several dependencies does not unblock it — and the delete
//! succeeds only once the last one is cleared, after which the family's reader answers
//! the modelled `ResourceNotFoundException` (HTTP 404) and the name is free for a later
//! create; a name the registry does not hold is refused that same 404 rather than a
//! hollow success. For `DeletePolicy` the dependencies are the two the operation
//! documents — "A policy cannot be deleted if it has non-default versions or it is
//! attached to any certificate" — so a policy any target still names and a policy that
//! still holds a version other than the one in effect are both held back.
//!
//! The tables span what that rule generalises over rather than one recorded request:
//! every target form the modelled member documents (an ACTIVE certificate ARN, an
//! INACTIVE one, a thing group ARN and a Cognito identity), one, two and three attached
//! targets at once, policy names of several forms and lengths, and a policy holding a
//! second version whichever of the two is in effect.
//!
//! The last two suites take the rule where BOTH dependencies hold at once — a policy a
//! target still names AND that still holds a version other than the one in effect — and
//! where the successful delete lands: clearing either half on its own leaves the delete
//! refused, worded for the half that remains, in whichever order the documented
//! remediation (`DeletePolicyVersion`, then `DetachPolicy`) is worked; and once it
//! succeeds "its default version is deleted with it", so the readers of one version and
//! of all of them refuse the name, and a policy created under that name again starts its
//! versions over.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The longest legal `policyName`: the member is modelled `length 1..128`
/// (`smithy_query(query="operation",operation="DeletePolicy")`).
const MAX_POLICY_NAME_LEN: usize = 128;

/// An IoT policy document every policy in this suite is created with: which document a
/// policy carries is not what this suite is about, so one valid document serves the
/// creates.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:Connect","Resource":"*"}]}"#;

/// A second, different document, for the cases that add a version or re-create a name.
const OTHER_POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:Publish","Resource":"*"}]}"#;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name
/// and the region: the scope is how the router picks the claiming service, and how the
/// service picks the region its ARNs are scoped to. The signature itself is never
/// checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `targets` of a `ListTargetsForPolicy` answer.
    fn targets(&self) -> Vec<String> {
        self.body
            .get("targets")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `versionId` of every version a `ListPolicyVersions` answer carries, in the
    /// order it reported them.
    fn version_ids(&self) -> Vec<String> {
        self.body
            .get("policyVersions")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        entry
                            .get("versionId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `versionId` of the version a `ListPolicyVersions` answer flags
    /// `isDefaultVersion` — the one in EFFECT, which a delete of the policy takes with
    /// it.
    fn default_version_id(&self) -> Option<String> {
        self.body
            .get("policyVersions")
            .and_then(Value::as_array)?
            .iter()
            .find(|entry| {
                entry
                    .get("isDefaultVersion")
                    .and_then(Value::as_bool)
                    .unwrap_or_default()
            })
            .and_then(|entry| entry.get("versionId"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Whether the body carries no member at all — what an operation whose output shape
    /// is `Unit` answers with.
    fn is_empty_body(&self) -> bool {
        match &self.body {
            Value::Null => true,
            Value::Object(members) => members.is_empty(),
            _ => false,
        }
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
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
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str, document: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    /// `GetPolicy`: `GET /policies/{policyName}`.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `DeletePolicy`: `DELETE /policies/{policyName}`.
    async fn delete_policy(&self, policy_name: &str) -> Reply {
        self.call("DELETE", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `CreatePolicyVersion`: `POST /policies/{policyName}/version`, whose
    /// `setAsDefault` travels as a query parameter.
    async fn create_policy_version(
        &self,
        policy_name: &str,
        document: &str,
        set_as_default: bool,
    ) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}/version?setAsDefault={set_as_default}"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target.
    async fn attach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
        )
        .await
    }

    /// `DetachPolicy`: `POST /target-policies/{policyName}` carrying the target.
    async fn detach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "POST",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
        )
        .await
    }

    /// `ListTargetsForPolicy`: `POST /policy-targets/{policyName}`.
    async fn list_targets_for_policy(&self, policy_name: &str) -> Reply {
        self.call("POST", &format!("/policy-targets/{policy_name}"), None)
            .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a policy is attached to.
    async fn create_certificate(&self, set_as_active: bool) -> Reply {
        self.call(
            "POST",
            &format!("/keys-and-certificate?setAsActive={set_as_active}"),
            None,
        )
        .await
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`.
    async fn create_thing_group(&self, name: &str) -> Reply {
        self.call("POST", &format!("/thing-groups/{name}"), Some(&json!({})))
            .await
    }

    /// `ListPolicyVersions`: `GET /policies/{policyName}/version` — the reader that
    /// reports which versions a policy still holds.
    async fn list_policy_versions(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}/version"), None)
            .await
    }

    /// `GetPolicyVersion`: `GET /policies/{policyName}/version/{policyVersionId}`.
    async fn get_policy_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.call(
            "GET",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    /// `DeletePolicyVersion`: `DELETE /policies/{policyName}/version/{policyVersionId}`
    /// — the remediation `DeletePolicy` documents for a policy holding a non-default
    /// version.
    async fn delete_policy_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }
}

/// The sentence a delete refuses a policy a principal is still attached to with: fixed
/// text, with the policy named by the `policyName` the request carried.
fn attached_message(policy_name: &str) -> String {
    format!(
        "The policy cannot be deleted as the policy is attached to one or more principals \
         (name={policy_name})"
    )
}

/// The sentence a delete refuses a policy that still holds a non-default version with:
/// fixed text, with the policy named by the `policyName` the request carried, and the
/// remediation the caller has to perform before retrying.
fn non_default_versions_message(policy_name: &str) -> String {
    format!(
        "The policy cannot be deleted as multiple versions of the policy exist \
         (name={policy_name}). Use DeletePolicyVersion action to delete all non-default \
         versions of the policy and retry."
    )
}

/// The sentence every operation addressing ONE policy refuses a name the registry does
/// not hold with.
fn missing_message(policy_name: &str) -> String {
    format!("Policy {policy_name} does not exist")
}

/// Which kind of target a row attaches the policy to: the forms
/// `ListAttachedPolicies$target` documents — "Valid principals are CertificateArn
/// (arn:aws:iot:region:accountId:cert/certificateId), thingGroupArn
/// (arn:aws:iot:region:accountId:thinggroup/groupName) and CognitoId (region:id)".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    ActiveCertificate,
    InactiveCertificate,
    ThingGroup,
    CognitoIdentity,
}

/// One row: a policy name and the targets it is attached to before the delete.
struct Case {
    label: &'static str,
    policy_name: String,
    targets: Vec<TargetKind>,
}

/// The table every case list below is built from. It spans what the rule generalises
/// over: the number of attached targets (one, two and three), every target form the
/// modelled member documents, and the form and length of the policy name.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one ACTIVE certificate",
            policy_name: "device-policy".to_string(),
            targets: vec![TargetKind::ActiveCertificate],
        },
        Case {
            label: "two certificates, one ACTIVE and one INACTIVE",
            policy_name: "Second_Policy-1".to_string(),
            targets: vec![
                TargetKind::ActiveCertificate,
                TargetKind::InactiveCertificate,
            ],
        },
        Case {
            label: "a thing group alone",
            policy_name: "fleet.policy".to_string(),
            targets: vec![TargetKind::ThingGroup],
        },
        Case {
            label: "a Cognito identity alone",
            policy_name: "app+policy=1".to_string(),
            targets: vec![TargetKind::CognitoIdentity],
        },
        Case {
            label: "three targets of three different forms",
            policy_name: "mixed@policy".to_string(),
            targets: vec![
                TargetKind::ActiveCertificate,
                TargetKind::ThingGroup,
                TargetKind::CognitoIdentity,
            ],
        },
        Case {
            label: "the longest legal policy name, with two certificates",
            policy_name: "p".repeat(MAX_POLICY_NAME_LEN),
            targets: vec![
                TargetKind::ActiveCertificate,
                TargetKind::InactiveCertificate,
            ],
        },
    ]
}

/// Mint the target `kind` names and attach `policy_name` to it, answering the target
/// string the attach recorded.
async fn given_attached_target(
    emulator: &Emulator,
    policy_name: &str,
    kind: TargetKind,
    index: usize,
) -> String {
    let target = match kind {
        TargetKind::ActiveCertificate | TargetKind::InactiveCertificate => {
            let set_as_active = kind == TargetKind::ActiveCertificate;
            let issued = emulator.create_certificate(set_as_active).await;
            assert_eq!(
                issued.status, 200,
                "case {policy_name:?}/{kind:?}: CreateKeysAndCertificate must answer 200, \
                 got {issued:?}"
            );
            issued.text("certificateArn").unwrap_or_else(|| {
                panic!("case {policy_name:?}/{kind:?}: no certificateArn in {issued:?}")
            })
        }
        TargetKind::ThingGroup => {
            let name = format!("group-{index}-{}", policy_name.len());
            let created = emulator.create_thing_group(&name).await;
            assert_eq!(
                created.status, 200,
                "case {policy_name:?}/{kind:?}: CreateThingGroup must answer 200, got \
                 {created:?}"
            );
            created.text("thingGroupArn").unwrap_or_else(|| {
                panic!("case {policy_name:?}/{kind:?}: no thingGroupArn in {created:?}")
            })
        }
        // A Cognito identity lives in another service, so it is named rather than
        // created: the relation resolves it no further than its `<region>:<id>` form.
        TargetKind::CognitoIdentity => {
            format!("{REGION}:0bb1c2d3-4e5f-6789-abcd-ef012345678{index}")
        }
    };

    let attached = emulator.attach_policy(policy_name, &target).await;
    assert_eq!(
        attached.status, 200,
        "case {policy_name:?}/{kind:?}: AttachPolicy must answer 200, got {attached:?}"
    );
    target
}

#[tokio::test]
async fn a_policy_any_target_still_names_is_not_deleted_until_the_last_detach() {
    for case in cases() {
        let label = case.label;
        let policy_name = case.policy_name.as_str();
        // A fresh emulator per row, so the delete below is gated by this row's targets.
        let emulator = Emulator::new();

        let created = emulator.create_policy(policy_name, POLICY_DOCUMENT).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreatePolicy must succeed, got {created:?}"
        );

        let mut attached: Vec<String> = Vec::new();
        for (index, kind) in case.targets.iter().enumerate() {
            attached.push(given_attached_target(&emulator, policy_name, *kind, index).await);
        }

        // The refusal is idempotent: it holds for as long as ANY target remains, so
        // detaching one of several does not unblock the delete. The loop walks the
        // targets off one at a time and asserts the same refusal before each detach.
        while !attached.is_empty() {
            let remaining = attached.len();
            let refused = emulator.delete_policy(policy_name).await;
            assert_eq!(
                refused.status, 409,
                "case {label:?}: with {remaining} target(s) attached the delete must be \
                 refused 409, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("DeleteConflictException"),
                "case {label:?}: with {remaining} target(s) attached, expected \
                 DeleteConflictException, got {refused:?}"
            );
            assert_eq!(
                refused.text("message"),
                Some(attached_message(policy_name)),
                "case {label:?}: unexpected delete-conflict message, got {refused:?}"
            );

            // The refused delete removed nothing: the policy and every attachment it
            // still holds are exactly as they were.
            let read = emulator.get_policy(policy_name).await;
            assert_eq!(
                read.status, 200,
                "case {label:?}: a refused delete must leave the policy readable, got \
                 {read:?}"
            );
            let listed = emulator.list_targets_for_policy(policy_name).await;
            assert_eq!(
                listed.targets().len(),
                remaining,
                "case {label:?}: a refused delete must leave every attachment, got {listed:?}"
            );

            let target = attached.remove(0);
            let detached = emulator.detach_policy(policy_name, &target).await;
            assert_eq!(
                detached.status, 200,
                "case {label:?}: DetachPolicy must succeed for {target:?}, got {detached:?}"
            );
        }

        // The last detach unblocks it: the delete succeeds, its output shape is `Unit`,
        // and the registry stops reporting the policy.
        let deleted = emulator.delete_policy(policy_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: with nothing attached the delete must succeed, got {deleted:?}"
        );
        assert!(
            deleted.is_empty_body(),
            "case {label:?}: a successful delete reports no members, got {deleted:?}"
        );

        let read = emulator.get_policy(policy_name).await;
        assert_eq!(
            read.status, 404,
            "case {label:?}: the deleted policy must be gone for every later reader, got \
             {read:?}"
        );
        assert_eq!(
            read.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {label:?}: expected ResourceNotFoundException, got {read:?}"
        );
        // `GetPolicy` reports "the policy document of the default version", so it words
        // the miss after the version it could not find: fixed text with no variable part.
        assert_eq!(
            read.text("message"),
            Some("Policy default version not found".to_string()),
            "case {label:?}: unexpected not-found message from the reader, got {read:?}"
        );
    }
}

#[tokio::test]
async fn a_policy_that_still_holds_a_non_default_version_is_not_deleted() {
    // The second half of the documented rule: the delete takes the default version with
    // it, so a policy holding any OTHER version is held back — whichever of the two is
    // in effect, which is what the second round below asserts.
    for set_as_default in [false, true] {
        let emulator = Emulator::new();
        let policy_name = format!("versioned_{set_as_default}");

        let created = emulator.create_policy(&policy_name, POLICY_DOCUMENT).await;
        assert_eq!(
            created.status, 200,
            "case setAsDefault={set_as_default}: CreatePolicy must succeed, got {created:?}"
        );
        // One version and nothing attached: the policy is deletable at this point, which
        // the round below confirms by adding the version that blocks it.
        let added = emulator
            .create_policy_version(&policy_name, OTHER_POLICY_DOCUMENT, set_as_default)
            .await;
        assert_eq!(
            added.status, 200,
            "case setAsDefault={set_as_default}: CreatePolicyVersion must succeed, got \
             {added:?}"
        );

        let refused = emulator.delete_policy(&policy_name).await;
        assert_eq!(
            refused.status, 409,
            "case setAsDefault={set_as_default}: a policy holding a non-default version \
             must be refused 409, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("DeleteConflictException"),
            "case setAsDefault={set_as_default}: expected DeleteConflictException, got \
             {refused:?}"
        );
        assert_eq!(
            refused.text("message"),
            Some(non_default_versions_message(&policy_name)),
            "case setAsDefault={set_as_default}: unexpected delete-conflict message, got \
             {refused:?}"
        );

        let read = emulator.get_policy(&policy_name).await;
        assert_eq!(
            read.status, 200,
            "case setAsDefault={set_as_default}: a refused delete must leave the policy \
             readable, got {read:?}"
        );
    }
}

#[tokio::test]
async fn a_policy_with_one_version_and_no_target_is_deleted_and_frees_its_name() {
    let emulator = Emulator::new();

    // Several policies live in one registry, so each delete is a delete of one record
    // out of many rather than of the only record there is.
    for case in cases() {
        let created = emulator
            .create_policy(&case.policy_name, POLICY_DOCUMENT)
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePolicy must succeed, got {created:?}",
            case.label
        );
    }

    let mut remaining = cases();
    while !remaining.is_empty() {
        let case = remaining.remove(0);
        let label = case.label;
        // What every surviving policy reports right before the delete, so the comparison
        // after it is against a value this test read rather than one it rebuilt.
        let mut survivors = Vec::new();
        for other in &remaining {
            survivors.push((
                other.label,
                other.policy_name.clone(),
                emulator.get_policy(&other.policy_name).await,
            ));
        }

        let deleted = emulator.delete_policy(&case.policy_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: an unattached single-version policy must be deleted, got \
             {deleted:?}"
        );

        for (other_label, other_name, before) in survivors {
            let after = emulator.get_policy(&other_name).await;
            assert_eq!(
                before.body, after.body,
                "case {label:?}: deleting it must leave {other_label:?} untouched, got \
                 {after:?}"
            );
        }

        // The name is the policy's identity, so the delete frees it: the re-create
        // records the NEW document rather than anything the deleted policy carried.
        let recreated = emulator
            .create_policy(&case.policy_name, OTHER_POLICY_DOCUMENT)
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {label:?}: the name must be free after the delete, got {recreated:?}"
        );
        let read = emulator.get_policy(&case.policy_name).await;
        assert_eq!(
            read.text("policyDocument").as_deref(),
            Some(OTHER_POLICY_DOCUMENT),
            "case {label:?}: the re-created policy must report the new document, got \
             {read:?}"
        );
        assert_eq!(
            read.text("policyArn"),
            Some(format!(
                "arn:aws:iot:{REGION}:{ACCOUNT_ID}:policy/{}",
                case.policy_name
            )),
            "case {label:?}: the re-created policy must carry the region's ARN, got {read:?}"
        );
        // Leave the re-created policy out of the way of the next row's comparison.
        let deleted = emulator.delete_policy(&case.policy_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: the re-created policy must delete again, got {deleted:?}"
        );
    }
}

#[tokio::test]
async fn a_policy_the_registry_does_not_hold_is_refused_not_found() {
    let emulator = Emulator::new();

    // One policy exists and stays, so the refusals below are about the NAME not being in
    // the registry rather than the registry being empty.
    let created = emulator.create_policy("existing-policy", POLICY_DOCUMENT).await;
    assert_eq!(
        created.status, 200,
        "the setup CreatePolicy must succeed, got {created:?}"
    );

    // A policy created and then deleted: the SECOND delete of it is refused for exactly
    // the same reason a name nobody created is.
    let created = emulator.create_policy("deleted-once", POLICY_DOCUMENT).await;
    assert_eq!(
        created.status, 200,
        "the setup CreatePolicy must succeed, got {created:?}"
    );
    let deleted = emulator.delete_policy("deleted-once").await;
    assert_eq!(
        deleted.status, 200,
        "the first DeletePolicy must succeed, got {deleted:?}"
    );

    // The names differ in form, length and case — a policy name is case-sensitive, so a
    // name that differs from an existing one only in case names no policy.
    for name in [
        "a",
        "deleted-once",
        "unknown-policy",
        "existing-policy-2",
        "EXISTING-POLICY",
        "Missing+Policy=1,2.x@host-9",
        &"n".repeat(MAX_POLICY_NAME_LEN),
    ] {
        let refused = emulator.delete_policy(name).await;
        assert_eq!(
            refused.status, 404,
            "case {name:?}: a delete of a name the registry does not hold must be refused \
             404, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {name:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.text("message"),
            Some(missing_message(name)),
            "case {name:?}: unexpected not-found message, got {refused:?}"
        );
    }

    // The refused deletes changed nothing: the policy that was there is still there.
    let survivor = emulator.get_policy("existing-policy").await;
    assert_eq!(
        survivor.status, 200,
        "a refused delete must leave the existing policy in place, got {survivor:?}"
    );
}

/// Which of the two dependencies the documented remediation clears FIRST: the rule is
/// about what still remains, so the order the caller works in must not change the
/// outcome.
#[derive(Debug, Clone, Copy)]
enum ClearedFirst {
    NonDefaultVersion,
    Attachment,
}

#[tokio::test]
async fn a_policy_held_back_by_both_halves_is_deleted_only_after_the_last_one_is_cleared() {
    // The two halves of the rule hold TOGETHER, and each on its own keeps holding: the
    // rows differ along which half the caller clears first and which target form the
    // attachment uses, because what decides the delete is the dependency that remains
    // rather than the sequence of calls that cleared the rest.
    let rows = [
        (
            "the non-default version cleared first, an ACTIVE certificate attached",
            ClearedFirst::NonDefaultVersion,
            TargetKind::ActiveCertificate,
        ),
        (
            "the attachment cleared first, an ACTIVE certificate attached",
            ClearedFirst::Attachment,
            TargetKind::ActiveCertificate,
        ),
        (
            "the non-default version cleared first, a thing group attached",
            ClearedFirst::NonDefaultVersion,
            TargetKind::ThingGroup,
        ),
        (
            "the attachment cleared first, an INACTIVE certificate attached",
            ClearedFirst::Attachment,
            TargetKind::InactiveCertificate,
        ),
        (
            "the attachment cleared first, a Cognito identity attached",
            ClearedFirst::Attachment,
            TargetKind::CognitoIdentity,
        ),
    ];

    for (label, cleared_first, target_kind) in rows {
        let emulator = Emulator::new();
        let policy_name = "held-by-both";

        let created = emulator.create_policy(policy_name, POLICY_DOCUMENT).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreatePolicy must succeed, got {created:?}"
        );
        let added = emulator
            .create_policy_version(policy_name, OTHER_POLICY_DOCUMENT, false)
            .await;
        assert_eq!(
            added.status, 200,
            "case {label:?}: CreatePolicyVersion must succeed, got {added:?}"
        );
        let non_default_version = added
            .text("policyVersionId")
            .unwrap_or_else(|| panic!("case {label:?}: no policyVersionId in {added:?}"));
        let target = given_attached_target(&emulator, policy_name, target_kind, 0).await;

        // With BOTH halves in place the delete is refused the one modelled conflict, and
        // the sentence it states is one of the two the two halves are worded with.
        let refused = emulator.delete_policy(policy_name).await;
        assert_eq!(
            refused.status, 409,
            "case {label:?}: a policy held back by both halves must be refused 409, got \
             {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("DeleteConflictException"),
            "case {label:?}: expected DeleteConflictException, got {refused:?}"
        );
        let stated = refused.text("message");
        assert!(
            stated.as_deref() == Some(attached_message(policy_name).as_str())
                || stated.as_deref() == Some(non_default_versions_message(policy_name).as_str()),
            "case {label:?}: the refusal must state one of the two documented conflicts, \
             got {refused:?}"
        );

        // Clearing ONE half leaves the other holding the delete back, and the refusal is
        // then worded for the half that remains.
        let remaining_message = match cleared_first {
            ClearedFirst::NonDefaultVersion => {
                let deleted = emulator
                    .delete_policy_version(policy_name, &non_default_version)
                    .await;
                assert_eq!(
                    deleted.status, 200,
                    "case {label:?}: DeletePolicyVersion must clear version \
                     {non_default_version:?}, got {deleted:?}"
                );
                attached_message(policy_name)
            }
            ClearedFirst::Attachment => {
                let detached = emulator.detach_policy(policy_name, &target).await;
                assert_eq!(
                    detached.status, 200,
                    "case {label:?}: DetachPolicy must clear {target:?}, got {detached:?}"
                );
                non_default_versions_message(policy_name)
            }
        };

        let still_refused = emulator.delete_policy(policy_name).await;
        assert_eq!(
            still_refused.status, 409,
            "case {label:?}: one half cleared must leave the delete refused 409, got \
             {still_refused:?}"
        );
        assert_eq!(
            still_refused.error_code.as_deref(),
            Some("DeleteConflictException"),
            "case {label:?}: expected DeleteConflictException, got {still_refused:?}"
        );
        assert_eq!(
            still_refused.text("message"),
            Some(remaining_message.clone()),
            "case {label:?}: the refusal must be worded for the half that remains, got \
             {still_refused:?}"
        );

        // Neither refused delete removed anything: the policy still reads back, and so do
        // the versions it holds.
        let read = emulator.get_policy(policy_name).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: a refused delete must leave the policy readable, got {read:?}"
        );
        let versions = emulator.list_policy_versions(policy_name).await;
        let expected_versions: usize = match cleared_first {
            ClearedFirst::NonDefaultVersion => 1,
            ClearedFirst::Attachment => 2,
        };
        assert_eq!(
            versions.version_ids().len(),
            expected_versions,
            "case {label:?}: a refused delete must leave every version it did not clear, \
             got {versions:?}"
        );

        // Clearing the SECOND half unblocks it.
        match cleared_first {
            ClearedFirst::NonDefaultVersion => {
                let detached = emulator.detach_policy(policy_name, &target).await;
                assert_eq!(
                    detached.status, 200,
                    "case {label:?}: DetachPolicy must clear {target:?}, got {detached:?}"
                );
            }
            ClearedFirst::Attachment => {
                let deleted = emulator
                    .delete_policy_version(policy_name, &non_default_version)
                    .await;
                assert_eq!(
                    deleted.status, 200,
                    "case {label:?}: DeletePolicyVersion must clear version \
                     {non_default_version:?}, got {deleted:?}"
                );
            }
        }

        let deleted = emulator.delete_policy(policy_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: with both halves cleared the delete must succeed, got \
             {deleted:?}"
        );
        let read = emulator.get_policy(policy_name).await;
        assert_eq!(
            read.status, 404,
            "case {label:?}: the deleted policy must be gone for every later reader, got \
             {read:?}"
        );
    }
}

#[tokio::test]
async fn a_deleted_policy_takes_the_version_in_effect_with_it() {
    // "When a policy is deleted using DeletePolicy, its default version is deleted with
    // it": the rows differ along WHICH id is in effect when the policy goes — the first
    // version a create minted, and a later one the account re-pointed the policy at —
    // because what the delete removes is the record, not an id.
    struct Row {
        label: &'static str,
        policy_name: &'static str,
        /// How many versions beyond the first the policy is given, the last of them
        /// pointed at as the one in effect; the earlier ones are deleted first, as the
        /// documented remediation requires.
        extra_versions: usize,
    }
    let rows = [
        Row {
            label: "the first version is the one in effect",
            policy_name: "effect-first",
            extra_versions: 0,
        },
        Row {
            label: "the second version is the one in effect",
            policy_name: "effect-second",
            extra_versions: 1,
        },
        Row {
            label: "the fourth version is the one in effect",
            policy_name: "effect-fourth",
            extra_versions: 3,
        },
    ];

    for row in rows {
        let label = row.label;
        let emulator = Emulator::new();

        let created = emulator
            .create_policy(row.policy_name, POLICY_DOCUMENT)
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreatePolicy must succeed, got {created:?}"
        );
        for index in 0..row.extra_versions {
            let added = emulator
                .create_policy_version(row.policy_name, OTHER_POLICY_DOCUMENT, true)
                .await;
            assert_eq!(
                added.status, 200,
                "case {label:?}: CreatePolicyVersion #{index} must succeed, got {added:?}"
            );
        }

        // Only the version in effect is left standing, so nothing but the documented
        // rule's own subject holds the delete back.
        let listed = emulator.list_policy_versions(row.policy_name).await;
        let in_effect = listed
            .default_version_id()
            .unwrap_or_else(|| panic!("case {label:?}: no default version in {listed:?}"));
        for version_id in listed.version_ids() {
            if version_id == in_effect {
                continue;
            }
            let cleared = emulator
                .delete_policy_version(row.policy_name, &version_id)
                .await;
            assert_eq!(
                cleared.status, 200,
                "case {label:?}: DeletePolicyVersion must clear version {version_id:?}, \
                 got {cleared:?}"
            );
        }
        let remaining = emulator.list_policy_versions(row.policy_name).await;
        assert_eq!(
            remaining.version_ids(),
            vec![in_effect.clone()],
            "case {label:?}: only the version in effect must remain, got {remaining:?}"
        );
        // The row's point: the id in effect is the one this row set up.
        assert_eq!(
            in_effect,
            (row.extra_versions + 1).to_string(),
            "case {label:?}: unexpected version in effect, got {in_effect:?}"
        );

        let deleted = emulator.delete_policy(row.policy_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: the delete must succeed, got {deleted:?}"
        );

        // The default version went with the policy: the reader of ONE version and the
        // reader of ALL of them both refuse the name, worded the way every operation
        // addressing the policy record words a miss.
        let read_version = emulator
            .get_policy_version(row.policy_name, &in_effect)
            .await;
        assert_eq!(
            read_version.status, 404,
            "case {label:?}: the version in effect must be gone with the policy, got \
             {read_version:?}"
        );
        assert_eq!(
            read_version.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {label:?}: expected ResourceNotFoundException, got {read_version:?}"
        );
        assert_eq!(
            read_version.text("message"),
            Some(missing_message(row.policy_name)),
            "case {label:?}: unexpected not-found message from the version reader, got \
             {read_version:?}"
        );
        let listed_after = emulator.list_policy_versions(row.policy_name).await;
        assert_eq!(
            listed_after.status, 404,
            "case {label:?}: the deleted policy must hold no versions to list, got \
             {listed_after:?}"
        );
        assert_eq!(
            listed_after.text("message"),
            Some(missing_message(row.policy_name)),
            "case {label:?}: unexpected not-found message from the version listing, got \
             {listed_after:?}"
        );

        // Nothing of the deleted record survived under the name: a policy created under
        // it again starts its versions over at the first id rather than continuing the
        // numbering the deleted policy had reached.
        let recreated = emulator
            .create_policy(row.policy_name, POLICY_DOCUMENT)
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {label:?}: the name must be free after the delete, got {recreated:?}"
        );
        let fresh = emulator.list_policy_versions(row.policy_name).await;
        assert_eq!(
            fresh.version_ids(),
            vec!["1".to_string()],
            "case {label:?}: the re-created policy must start its versions over, got \
             {fresh:?}"
        );
    }
}
