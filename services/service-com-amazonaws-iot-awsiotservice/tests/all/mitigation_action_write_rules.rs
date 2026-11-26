// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of what the DEVICE DEFENDER MITIGATION ACTION write path
//! ACCEPTS and what it REFUSES: `CreateMitigationAction` and `UpdateMitigationAction`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! Three rules under test:
//!
//! 1. THE ROLE. `roleArn` is "The ARN of the IAM role that is used to apply the mitigation
//!    action" and the model constrains it by LENGTH alone — `RoleArn`, 20..2048, with no
//!    `@pattern` (`smithy_query(query="operation",operation="CreateMitigationAction")`) — so
//!    a value long enough to pass the request boundary and yet not the ARN of an IAM role is
//!    refused by the service itself: `InvalidRequestException` (400), the fixed clause
//!    `Invalid Role ARN:: ` followed by the value exactly as the request sent it, and nothing
//!    is persisted. The update reads a `roleArn` it carries the same way, and the modelled
//!    length bound stays the BOUNDARY's refusal, stated in the constraint envelope.
//! 2. THE PARAMETERS. `MitigationActionParams` documents "You can specify only one type of
//!    parameter (in other words, you can apply only one action for each defined mitigation
//!    action)", so parameters defining none and parameters defining more than one are both
//!    refused `InvalidRequestException` (400) with one entirely fixed sentence — it names
//!    neither the member nor the types the request defined — by the create and the update
//!    alike.
//! 3. THE REPEAT CREATE. The collision this family states is an existing resource "with
//!    different attributes", so what a create ASKS for decides it, not the name alone: a
//!    create under a name the account already holds answers the STORED record (200, with
//!    that record's `actionArn` and `actionId`, and nothing about it changed) when it asks
//!    for the same action — the same role applying the same parameters — and is refused
//!    `ResourceAlreadyExistsException` (409) carrying the family's sentence ALONE, with no
//!    `resourceId` and no `resourceArn`, when it asks for a different one.
//!
//! The tables span what those rules generalise over rather than one recorded request: the
//! forms an IAM role ARN is written in (a plain name, a path, another partition, the colon
//! resource form) and the classes of value that are no role ARN (not an ARN at all, another
//! vendor, another IAM resource type, an ARN truncated before its resource, an ARN not
//! written in lower case); the parameter cardinalities 0, 2, 3 and 6 over different members;
//! every modelled `MitigationActionType` for the repeat create, read against the generated
//! `MITIGATION_ACTION_TYPE_VALUES` so a type the model gains is a row this suite demands;
//! and which attribute a second create differs in (the role, the parameters, both, or only
//! the tags, which are not an attribute of the action).

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::ops::types::MITIGATION_ACTION_TYPE_VALUES;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The region every request in this suite is signed for; the ARN assertions read it back.
const REGION: &str = "us-west-2";

/// A well-formed role ARN: the role the actions in this suite are applied with.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-mitigation";

/// A SECOND well-formed role ARN, so a create can ask for a DIFFERENT action under a name
/// the account already holds.
const OTHER_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-other";

/// `InvalidRequestException` as AWS IoT names it in `x-amzn-errortype`: the shape name, a
/// `:`, and the service's published API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The whole sentence a request whose `actionParams` do not define exactly one type of
/// change is refused with: entirely fixed text, with no variable part at all.
const ONE_PARAMETER_MESSAGE: &str = "Expected only one action parameter to be set";

/// The fixed text of the clause a `roleArn` that is not the ARN of an IAM role is refused
/// with; the value the request carried follows it and is its only variable part.
const INVALID_ROLE_ARN_CLAUSE: &str = "Invalid Role ARN:: ";

/// The fixed text of the clause this family states a taken name with; the ARN of the
/// resource that already exists follows it.
const ALREADY_EXISTS_CLAUSE: &str = "Resource already exists for account with different attributes, ";

/// One well-formed `actionParams` per modelled `MitigationActionType`: the member of
/// `MitigationActionParams` that defines that type, carrying the values that member's own
/// shape declares (`smithy_query(query="shape",shape="MitigationActionParams")`).
fn params_catalog() -> Vec<(&'static str, Value)> {
    vec![
        (
            "UPDATE_DEVICE_CERTIFICATE",
            json!({ "updateDeviceCertificateParams": { "action": "DEACTIVATE" } }),
        ),
        (
            "UPDATE_CA_CERTIFICATE",
            json!({ "updateCACertificateParams": { "action": "DEACTIVATE" } }),
        ),
        (
            "ADD_THINGS_TO_THING_GROUP",
            json!({
                "addThingsToThingGroupParams": {
                    "thingGroupNames": ["quarantine"],
                    "overrideDynamicGroups": true,
                },
            }),
        ),
        (
            "REPLACE_DEFAULT_POLICY_VERSION",
            json!({ "replaceDefaultPolicyVersionParams": { "templateName": "BLANK_POLICY" } }),
        ),
        (
            "ENABLE_IOT_LOGGING",
            json!({
                "enableIoTLoggingParams": {
                    "roleArnForLogging": "arn:aws:iam::000000000000:role/iot-logging",
                    "logLevel": "WARN",
                },
            }),
        ),
        (
            "PUBLISH_FINDING_TO_SNS",
            json!({
                "publishFindingToSnsParams": {
                    "topicArn": "arn:aws:sns:us-west-2:000000000000:defender-findings",
                },
            }),
        ),
    ]
}

/// The parameters of the catalog entries `types` names, merged into one `actionParams` —
/// which then defines that many types of change.
fn params_defining(types: &[&str]) -> Value {
    let catalog = params_catalog();
    let mut merged = json!({});
    for wanted in types {
        let (_action_type, params) = catalog
            .iter()
            .find(|(action_type, _params)| action_type == wanted)
            .expect("every type a row names is in the catalog");
        for (member, value) in params
            .as_object()
            .expect("a catalog entry's parameters are a JSON object")
        {
            merged
                .as_object_mut()
                .expect("the merged parameters are a JSON object")
                .insert(member.clone(), value.clone());
        }
    }
    merged
}

/// The ARN this plane mints for `action_name`.
fn expected_arn(action_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:mitigationaction/{action_name}")
}

/// Values for `roleArn` and whether each one IS the ARN of an IAM role.
///
/// Every value is at least the modelled minimum of 20 characters, so the request boundary
/// admits all of them and what decides each row is the service's own reading of the form.
/// The admitted forms are what an IAM role ARN is written as —
/// `arn:<partition>:iam::<account>:role/<name>`, with a path allowed inside the name and
/// `:` as the alternative resource separator — and the refused rows are the classes of
/// value that name no role: no ARN at all, another vendor's ARN, another IAM resource
/// type, an ARN truncated before its resource, and one not written in lower case.
fn role_rows() -> Vec<(&'static str, &'static str, bool)> {
    vec![
        ("a role ARN as IAM writes one", ROLE_ARN, true),
        (
            "a role ARN carrying a service-role path",
            "arn:aws:iam::000000000000:role/service-role/AWSIoTDeviceDefenderMitigation",
            true,
        ),
        (
            "a role ARN in another partition",
            "arn:aws-cn:iam::000000000000:role/defender-in-china",
            true,
        ),
        (
            "a role ARN in the colon resource form",
            "arn:aws:iam::000000000000:role:defender-colon-form",
            true,
        ),
        (
            "a value that is not an ARN at all",
            "not-a-valid-arn-but-long-enough-to-pass-length-check",
            false,
        ),
        (
            "an IAM user, which is no role",
            "arn:aws:iam::000000000000:user/defender-operator",
            false,
        ),
        (
            "an IAM policy, which is no role",
            "arn:aws:iam::000000000000:policy/defender-policy",
            false,
        ),
        (
            "another vendor's ARN of the same resource type",
            "arn:aws:iot:us-west-2:000000000000:role/defender",
            false,
        ),
        (
            "an ARN truncated before its resource",
            "arn:aws:iam::000000000000",
            false,
        ),
        (
            "an ARN not written in lower case",
            "ARN:AWS:IAM::000000000000:ROLE/defender",
            false,
        ),
    ]
}

/// One emulator: the shared engine plus a router with this service mounted, reached the way
/// the production router reaches it.
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
            // The credential scope names the `iot` signing name the model's `aws.auth#sigv4`
            // trait declares, which is what routes the request, and the region the served
            // ARNs are built for.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the mitigation-action request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// `CreateMitigationAction`, bound to `POST /mitigationactions/actions/{actionName}`.
    async fn create(&self, action_name: &str, body: &Value) -> Reply {
        self.dispatch(
            "POST",
            &format!("/mitigationactions/actions/{action_name}"),
            Some(body),
        )
        .await
    }

    /// `UpdateMitigationAction`, bound to `PATCH /mitigationactions/actions/{actionName}`.
    async fn update(&self, action_name: &str, body: &Value) -> Reply {
        self.dispatch(
            "PATCH",
            &format!("/mitigationactions/actions/{action_name}"),
            Some(body),
        )
        .await
    }

    /// `DescribeMitigationAction`, bound to `GET /mitigationactions/actions/{actionName}`.
    async fn describe(&self, action_name: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/mitigationactions/actions/{action_name}"),
            None,
        )
        .await
    }

    /// Create `action_name` with `role_arn` and `params`, asserting it was accepted, and
    /// answer the record a read of it reports.
    async fn created_record(
        &self,
        case: &str,
        action_name: &str,
        role_arn: &str,
        params: &Value,
    ) -> Value {
        let created = self
            .create(
                action_name,
                &json!({ "roleArn": role_arn, "actionParams": params }),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200 from the create, got {} body {}",
            created.status,
            created.text()
        );
        let stored = self.describe(action_name).await;
        assert_eq!(
            stored.status,
            200,
            "case {case}: expected the created action to read back, got {} body {}",
            stored.status,
            stored.text()
        );
        stored.json()
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The `x-amzn-errortype` header verbatim, including any namespace the shape name is
    /// qualified with.
    error_type: Option<String>,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Self {
            status,
            error_type,
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    /// The error code a restJson1 error response carries in its body.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The `message` the body carries.
    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }

    fn member(&self, member: &str) -> Option<Value> {
        self.json().get(member).cloned()
    }
}

/// The catalog covers the whole modelled set of types, so a `MitigationActionType` the
/// model gains is a row these tables demand rather than a case nothing exercises.
#[test]
fn the_catalog_spans_every_modeled_mitigation_action_type() {
    let covered: BTreeSet<&str> = params_catalog()
        .into_iter()
        .map(|(action_type, _params)| action_type)
        .collect();
    let modeled: BTreeSet<&str> = MITIGATION_ACTION_TYPE_VALUES.iter().copied().collect();
    assert_eq!(
        covered, modeled,
        "expected one catalog entry per modelled MitigationActionType \
         {MITIGATION_ACTION_TYPE_VALUES:?}, got {covered:?}"
    );
}

/// RULE: a `roleArn` that satisfies the modelled length and is still not the ARN of an IAM
/// role is refused `InvalidRequestException` (400) with the clause and the value the
/// request carried, and nothing is created; one that IS a role ARN is accepted, whichever
/// form it is written in.
#[tokio::test]
async fn a_mitigation_actions_role_must_be_the_arn_of_an_iam_role() {
    let action_name = "role-form-under-test";
    let params = params_catalog()[0].1.clone();

    for (case, role_arn, admitted) in role_rows() {
        // A fresh emulator per row: what the row proves must depend on this row alone.
        let emulator = Emulator::new();
        let reply = emulator
            .create(
                action_name,
                &json!({ "roleArn": role_arn, "actionParams": params }),
            )
            .await;

        if admitted {
            assert_eq!(
                reply.status,
                200,
                "case {case}: expected 200 for {role_arn:?}, got {} body {}",
                reply.status,
                reply.text()
            );
            let stored = emulator.describe(action_name).await;
            assert_eq!(
                stored.json()["roleArn"].as_str(),
                Some(role_arn),
                "case {case}: expected the stored action to hold the role it was created \
                 with, got {}",
                stored.text()
            );
            continue;
        }

        assert_eq!(
            reply.status,
            400,
            "case {case}: expected 400 for {role_arn:?}, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(QUALIFIED_INVALID_REQUEST),
            "case {case}: expected the qualified error-shape name, got {:?}",
            reply.error_type
        );
        assert_eq!(
            reply.error_code(),
            "InvalidRequestException",
            "case {case}: got {}",
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            format!("{INVALID_ROLE_ARN_CLAUSE}{role_arn}"),
            "case {case}: expected the clause and the value the request carried, got {:?}",
            reply.error_message()
        );
        let missing = emulator.describe(action_name).await;
        assert_eq!(
            missing.status,
            404,
            "case {case}: expected the refused create to leave no action behind, got {} \
             body {}",
            missing.status,
            missing.text()
        );
    }
}

/// RULE: the update reads a `roleArn` it carries exactly as the create does — a value that
/// names no IAM role is refused in the same sentence and the stored role stands, and one
/// that names a role replaces it.
#[tokio::test]
async fn the_update_reads_a_role_arn_the_way_the_create_does() {
    let emulator = Emulator::new();
    let action_name = "updated-role";
    let params = params_catalog()[0].1.clone();
    emulator
        .created_record("the update's subject", action_name, ROLE_ARN, &params)
        .await;

    for (case, role_arn, admitted) in role_rows() {
        let reply = emulator
            .update(action_name, &json!({ "roleArn": role_arn }))
            .await;
        let stored = emulator.describe(action_name).await;

        if admitted {
            assert_eq!(
                reply.status,
                200,
                "case {case}: expected 200 from the update of {role_arn:?}, got {} body {}",
                reply.status,
                reply.text()
            );
            assert_eq!(
                stored.json()["roleArn"].as_str(),
                Some(role_arn),
                "case {case}: expected the update to replace the stored role, got {}",
                stored.text()
            );
            // Back to the role the following rows expect to find stored.
            let restored = emulator
                .update(action_name, &json!({ "roleArn": ROLE_ARN }))
                .await;
            assert_eq!(
                restored.status,
                200,
                "case {case}: expected 200 restoring the role, got {} body {}",
                restored.status,
                restored.text()
            );
            continue;
        }

        assert_eq!(
            reply.status,
            400,
            "case {case}: expected 400 from the update of {role_arn:?}, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            format!("{INVALID_ROLE_ARN_CLAUSE}{role_arn}"),
            "case {case}: expected the clause and the value the request carried, got {:?}",
            reply.error_message()
        );
        assert_eq!(
            stored.json()["roleArn"].as_str(),
            Some(ROLE_ARN),
            "case {case}: expected the refused update to leave the stored role as it was, \
             got {}",
            stored.text()
        );
    }
}

/// RULE: the modelled `@length` on `roleArn` is still the BOUNDARY's refusal — a value
/// short of the minimum is refused in the constraint envelope, naming the member and the
/// bound, rather than in the service's own clause about the ARN form.
#[tokio::test]
async fn a_role_arn_short_of_the_modeled_minimum_is_refused_by_the_boundary() {
    let emulator = Emulator::new();
    let action_name = "role-short-of-the-bound";
    // 19 characters: one short of the modelled minimum of 20, and no ARN either.
    let too_short = "arn:aws:iam::0:role";
    assert_eq!(
        too_short.len(),
        19,
        "case the length row: expected a value one short of the modelled minimum, got {}",
        too_short.len()
    );

    let reply = emulator
        .create(
            action_name,
            &json!({ "roleArn": too_short, "actionParams": params_catalog()[0].1.clone() }),
        )
        .await;
    assert_eq!(
        reply.status,
        400,
        "case the length row: expected 400, got {} body {}",
        reply.status,
        reply.text()
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some(QUALIFIED_INVALID_REQUEST),
        "case the length row: expected the qualified error-shape name, got {:?}",
        reply.error_type
    );
    let message = reply.error_message();
    assert!(
        message.contains("roleArn") && message.contains("constraint"),
        "case the length row: expected the constraint envelope naming the member, got \
         {message:?}"
    );
    assert!(
        !message.starts_with(INVALID_ROLE_ARN_CLAUSE),
        "case the length row: the modelled bound is refused before the ARN form is read, \
         got {message:?}"
    );
    let missing = emulator.describe(action_name).await;
    assert_eq!(
        missing.status,
        404,
        "case the length row: expected no action to be created, got {} body {}",
        missing.status,
        missing.text()
    );
}

/// RULE: `actionParams` that do not define exactly ONE type of change are refused
/// `InvalidRequestException` (400) with one entirely fixed sentence — whether they define
/// none or several, and whether the create or the update carried them — and nothing is
/// written.
#[tokio::test]
async fn action_params_defining_other_than_one_type_are_refused_in_one_sentence() {
    // The cardinalities the rule generalises over: none set, two set (over different
    // members, so no one pair is the case), three set, and every member set.
    let rows: Vec<(&str, Value)> = vec![
        ("no parameter type at all", json!({})),
        (
            "two types, the certificate pair",
            params_defining(&["UPDATE_DEVICE_CERTIFICATE", "UPDATE_CA_CERTIFICATE"]),
        ),
        (
            "two types, a certificate and a notification",
            params_defining(&["UPDATE_CA_CERTIFICATE", "PUBLISH_FINDING_TO_SNS"]),
        ),
        (
            "three types",
            params_defining(&[
                "ADD_THINGS_TO_THING_GROUP",
                "REPLACE_DEFAULT_POLICY_VERSION",
                "ENABLE_IOT_LOGGING",
            ]),
        ),
        (
            "every modelled type at once",
            params_defining(
                &params_catalog()
                    .iter()
                    .map(|(action_type, _params)| *action_type)
                    .collect::<Vec<&str>>(),
            ),
        ),
    ];

    let emulator = Emulator::new();
    // One stored action, so the update rows below have a record to be refused against.
    let stored_name = "params-under-test";
    let stored_params = params_catalog()[0].1.clone();
    let stored_record = emulator
        .created_record(
            "the parameters' subject",
            stored_name,
            ROLE_ARN,
            &stored_params,
        )
        .await;

    for (case, params) in rows {
        for (operation, reply) in [
            (
                "CreateMitigationAction",
                emulator
                    .create(
                        "params-refused",
                        &json!({ "roleArn": ROLE_ARN, "actionParams": params.clone() }),
                    )
                    .await,
            ),
            (
                "UpdateMitigationAction",
                emulator
                    .update(stored_name, &json!({ "actionParams": params.clone() }))
                    .await,
            ),
        ] {
            assert_eq!(
                reply.status,
                400,
                "case {operation} with {case}: expected 400, got {} body {}",
                reply.status,
                reply.text()
            );
            assert_eq!(
                reply.error_type.as_deref(),
                Some(QUALIFIED_INVALID_REQUEST),
                "case {operation} with {case}: expected the qualified error-shape name, \
                 got {:?}",
                reply.error_type
            );
            assert_eq!(
                reply.error_code(),
                "InvalidRequestException",
                "case {operation} with {case}: got {}",
                reply.text()
            );
            assert_eq!(
                reply.error_message(),
                ONE_PARAMETER_MESSAGE,
                "case {operation} with {case}: expected the one fixed sentence, got {:?}",
                reply.error_message()
            );
        }

        let missing = emulator.describe("params-refused").await;
        assert_eq!(
            missing.status,
            404,
            "case {case}: expected the refused create to leave no action behind, got {} \
             body {}",
            missing.status,
            missing.text()
        );
        let after = emulator.describe(stored_name).await;
        assert_eq!(
            after.json(),
            stored_record,
            "case {case}: expected the refused update to leave the stored record as it \
             was, got {}",
            after.text()
        );
    }
}

/// RULE: a create under a name the account already holds that asks for the SAME action —
/// the same role applying the same parameters — answers that stored record: 200 with its
/// own `actionArn` and `actionId`, and nothing about the record changes. Tags are not an
/// attribute of the action, so a repeat differing only in them is still the same action.
#[tokio::test]
async fn a_repeat_create_of_the_same_action_answers_the_stored_record() {
    let emulator = Emulator::new();

    // One row per modelled `MitigationActionType`: the rule holds whichever type of change
    // the action applies.
    for (action_type, params) in params_catalog() {
        let action_name = format!("repeat-{}", action_type.to_lowercase());
        let first = emulator
            .create(
                &action_name,
                &json!({
                    "roleArn": ROLE_ARN,
                    "actionParams": params.clone(),
                    "tags": [{ "Key": "owner", "Value": "defender" }],
                }),
            )
            .await;
        assert_eq!(
            first.status,
            200,
            "case {action_type}: expected 200 from the first create, got {} body {}",
            first.status,
            first.text()
        );
        let minted_arn = first.json()["actionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let minted_id = first.json()["actionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            minted_arn,
            expected_arn(&action_name),
            "case {action_type}: expected the ARN this plane mints, got {}",
            first.text()
        );
        let stored = emulator.describe(&action_name).await.json();

        // The repeats that ask for the same action: byte for byte, and differing only in
        // the tags the request carries.
        for (case, body) in [
            (
                "the identical request",
                json!({
                    "roleArn": ROLE_ARN,
                    "actionParams": params.clone(),
                    "tags": [{ "Key": "owner", "Value": "defender" }],
                }),
            ),
            (
                "the same action with other tags",
                json!({
                    "roleArn": ROLE_ARN,
                    "actionParams": params.clone(),
                    "tags": [{ "Key": "attempt", "Value": "second" }],
                }),
            ),
            (
                "the same action with no tags at all",
                json!({ "roleArn": ROLE_ARN, "actionParams": params.clone() }),
            ),
        ] {
            let repeat = emulator.create(&action_name, &body).await;
            assert_eq!(
                repeat.status,
                200,
                "case {action_type} / {case}: expected 200, got {} body {}",
                repeat.status,
                repeat.text()
            );
            assert_eq!(
                (
                    repeat.json()["actionArn"].as_str(),
                    repeat.json()["actionId"].as_str()
                ),
                (Some(minted_arn.as_str()), Some(minted_id.as_str())),
                "case {action_type} / {case}: expected the stored record's identity, got {}",
                repeat.text()
            );
            let after = emulator.describe(&action_name).await;
            assert_eq!(
                after.json(),
                stored,
                "case {action_type} / {case}: expected the repeat create to write nothing, \
                 got {}",
                after.text()
            );
        }
    }
}

/// RULE: a create under a name the account already holds that asks for a DIFFERENT action
/// is the conflict: `ResourceAlreadyExistsException` (409) whose body carries the family's
/// clause and the ARN of the action that already exists — and nothing else, no `resourceId`
/// and no `resourceArn` — with the stored record left exactly as it was.
#[tokio::test]
async fn a_create_asking_for_a_different_action_under_a_taken_name_is_the_conflict() {
    let catalog = params_catalog();
    let params = catalog[0].1.clone();
    let other_params = catalog[1].1.clone();

    // What the second create differs in: the role, the parameters, or both — the attributes
    // a create states about the action.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a different role",
            json!({ "roleArn": OTHER_ROLE_ARN, "actionParams": params.clone() }),
        ),
        (
            "different parameters",
            json!({ "roleArn": ROLE_ARN, "actionParams": other_params.clone() }),
        ),
        (
            "a different role and different parameters",
            json!({ "roleArn": OTHER_ROLE_ARN, "actionParams": other_params.clone() }),
        ),
    ];

    for (case, body) in rows {
        // A fresh emulator per row: the refusal must depend only on what this row created.
        let emulator = Emulator::new();
        let action_name = "taken-name";
        let stored = emulator
            .created_record(case, action_name, ROLE_ARN, &params)
            .await;

        let refused = emulator.create(action_name, &body).await;
        assert_eq!(
            refused.status,
            409,
            "case {case}: expected 409, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceAlreadyExistsException"),
            "case {case}: expected the error-shape name in the header, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.error_code(),
            "ResourceAlreadyExistsException",
            "case {case}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            format!("{ALREADY_EXISTS_CLAUSE}{}", expected_arn(action_name)),
            "case {case}: expected the family's clause and the existing action's ARN, got \
             {:?}",
            refused.error_message()
        );
        assert_eq!(
            (refused.member("resourceId"), refused.member("resourceArn")),
            (None, None),
            "case {case}: this refusal reports no resourceId or resourceArn, got {}",
            refused.text()
        );

        let after = emulator.describe(action_name).await;
        assert_eq!(
            after.json(),
            stored,
            "case {case}: expected the refused create to leave the stored record as it was, \
             got {}",
            after.text()
        );
    }
}
