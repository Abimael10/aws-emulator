// SPDX-License-Identifier: CC0-1.0

//! What becomes of a TEMPLATE-DRIVEN launch whose root volume is encrypted under a customer
//! managed key, over every FORM the key reference can take, end to end over the wire.
//!
//! The rule: an encrypted EBS volume's data key comes from the KMS key the block device
//! mapping names, so the launch is accepted — HTTP 200, answering the `pending` instance it
//! minted — and the instance then RUNS only if the key that reference resolves to will
//! produce that data key. A key in a state KMS will not use, and a well-formed reference
//! that resolves to no key of the account at all, leave the instance with no volume to boot
//! from: it reaches `terminated` carrying the `Client.InvalidKMSKey.InvalidState` state
//! reason instead of `running`. WHICH form the reference takes decides nothing — the key it
//! names does.
//!
//! The reference forms are the ones the member publishes, "key ID, key alias, key ARN, or
//! alias ARN" (`smithy_query(query="doc",shape="EbsBlockDevice",member="KmsKeyId")`), and
//! every row is driven through a launch TEMPLATE, the launch form an Auto Scaling or
//! CloudFormation shaped workload uses: the template carries the mapping and `RunInstances`
//! names only the template, so the key reaches the launch from the stored launch parameters
//! rather than from the request.
//!
//! The rows are the four forms crossed with the key states the rule generalises over — a key
//! KMS will produce a data key under, a DISABLED key, a key whose deletion is SCHEDULED, and
//! a reference of that form naming nothing at all — so no row is the specification.
//!
//! Every call drives the real HTTP path: an `ec2Query` form POST through the router's
//! `dispatch` for Amazon EC2 and an `awsJson1_1` POST for AWS KMS, the same bytes an SDK
//! client sends. Nothing reaches into either service's state struct — each key is MINTED by
//! KMS, moved into its state through KMS's own API, and the outcome is read back only through
//! `DescribeInstances`. AWS KMS is mounted BESIDE Amazon EC2 on one engine, exactly as the
//! assembled router mounts them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_kms_trentservice as kms;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, indexed};
use super::wire::{child, child_text, document};

/// The Region every call of this suite is signed for.
const REGION: &str = "eu-west-1";

/// An AMI id of the right FORM that this account holds no image for: the launch is still
/// served, and its root device is the one the image this crate publishes by default carries —
/// which the suite reads off a probe launch rather than writing out here.
const AMI: &str = "ami-0123456789abcdef0";

/// The `StateReason` an instance whose volume key will not produce a data key reports, spelled
/// out here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
const INVALID_KEY_STATE_CODE: &str = "Client.InvalidKMSKey.InvalidState";
const INVALID_KEY_STATE_MESSAGE: &str =
    "Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state";

/// The two states a launch settles in.
const RUNNING: &str = "running";
const TERMINATED: &str = "terminated";

/// The state every launch answers with, and the one an instance on its way out passes
/// through: neither is a state an instance stays in, so a settling poll waits to leave both.
const PENDING: &str = "pending";
const SHUTTING_DOWN: &str = "shutting-down";

/// A key id of the right form — the UUID a KMS key id is — that no key of this account
/// carries: KMS mints a fresh id per key, so the nil UUID names none of them.
const ABSENT_KEY_ID: &str = "00000000-0000-0000-0000-000000000000";

/// An alias name of the right form that this account holds no alias for.
const ABSENT_ALIAS: &str = "alias/no-alias-of-this-account";

/// How long a poll keeps looking for a clock-owed move before it gives up — a generous
/// multiple of the service's own span, so a loaded host is never the reason a case fails.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Amazon EC2 and AWS KMS over ONE shared engine, exactly as the assembled router mounts
/// them: the key a launch's volumes name is a resource of that service, created through its
/// own API.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kms::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// One AWS KMS call, which must be served, answering its payload.
async fn kms_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://kms.{REGION}.amazonaws.com/"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDKEYFORMS/20260101/{REGION}/kms/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(::bytes::Bytes::from(
            ::serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed KMS request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        ::serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One customer managed key KMS minted, as `(key id, key ARN)` — both read off the create, so
/// neither is written out here.
async fn key(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let created = kms_ok(router, case, "CreateKey", json!({})).await;
    let metadata = &created["KeyMetadata"];
    let key_id = metadata["KeyId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateKey reports the key it minted, got {created}")
        })
        .to_string();
    let key_arn = metadata["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reports the key's ARN, got {created}"))
        .to_string();
    (key_id, key_arn)
}

/// An alias of this account for the key `target_key_id`, answering the alias name.
///
/// Created BEFORE the key is moved into the state its rows are about: "the KMS key that you use
/// for this operation must be in a compatible key state" (`CreateAlias`), so an alias for a key
/// whose deletion is already scheduled could not be made — which says nothing about what a
/// launch naming that alias does, the subject here.
async fn alias(router: &MultiServiceRouter, case: &str, name: &str, target_key_id: &str) -> String {
    let alias_name = format!("alias/{name}");
    kms_ok(
        router,
        case,
        "CreateAlias",
        json!({ "AliasName": alias_name.clone(), "TargetKeyId": target_key_id }),
    )
    .await;
    alias_name
}

/// The `(partition, account)` of a KMS key ARN KMS itself minted, and a check that the ARN is
/// the Region's own.
///
/// Read off the ARN rather than written out, so the ARN forms built below are this emulator's
/// own partition and account rather than a value from any one run.
fn partition_and_account(case: &str, key_arn: &str) -> (String, String) {
    let segments: Vec<&str> = key_arn.split(':').collect();
    assert!(
        segments.len() >= 6 && segments[0] == "arn" && segments[2] == "kms",
        "case {case:?}: expected a KMS key ARN, got {key_arn:?}"
    );
    assert_eq!(
        segments[3], REGION,
        "case {case:?}: the key ARN names the Region it was created through, got {key_arn:?}"
    );
    (segments[1].to_string(), segments[4].to_string())
}

/// One `RunInstances`, with `extra` beside the counts every row carries.
async fn launch(router: &MultiServiceRouter, extra: &[(String, String)]) -> (u16, String) {
    let mut params = vec![p("MinCount", "1"), p("MaxCount", "1")];
    params.extend_from_slice(extra);
    call(router, REGION, "RunInstances", &params).await
}

/// One `DescribeInstances` narrowed to `instance_id`, as its body.
async fn describe(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must answer 200, got {status} with {body}"
    );
    body
}

/// The root device name an instance launched from [`AMI`] reports, read off a probe launch
/// rather than written here: it is the AMI's own `rootDeviceName`, and the device a mapping has
/// to name to define the root volume.
async fn root_device(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = launch(router, &[p("ImageId", AMI)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the probe launch must be served, got {status} with body {body}"
    );
    let instance_id = element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: the launch must report its instance: {body}"));
    let described = describe(router, case, &instance_id).await;
    element(&described, "rootDeviceName").unwrap_or_else(|| {
        panic!("case {case:?}: the instance must report its root device: {described}")
    })
}

/// A launch template named `name` whose one version encrypts `device` under `key_reference`,
/// answering the `LaunchTemplate.LaunchTemplateId` field a launch names it by.
async fn template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    device: &str,
    key_reference: &str,
) -> (String, String) {
    let params = vec![
        p("LaunchTemplateName", name),
        p("LaunchTemplateData.ImageId", AMI),
        p("LaunchTemplateData.BlockDeviceMapping.1.DeviceName", device),
        p(
            "LaunchTemplateData.BlockDeviceMapping.1.Ebs.Encrypted",
            "true",
        ),
        p(
            "LaunchTemplateData.BlockDeviceMapping.1.Ebs.KmsKeyId",
            key_reference,
        ),
    ];
    let (status, body) = call(router, REGION, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must answer 200, got {status} with {body}"
    );
    let launch_template_id = element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: no launch template id in {body}"));
    p("LaunchTemplate.LaunchTemplateId", launch_template_id)
}

/// The `instanceState` name and the `stateReason` `DescribeInstances` reports for one instance
/// right now, as `(state, code, message)`.
///
/// Read through the document, so a member of the INSTANCE is never taken from one of the sets
/// nested inside it.
fn reading(case: &str, body: &str, instance_id: &str) -> (String, Option<String>, Option<String>) {
    let parsed = document(case, body);
    let instance = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "instanceState").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no instance reported for {instance_id}: {body}"));
    let state = child(instance, "instanceState")
        .and_then(|state| child_text(state, "name"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: {instance_id} must report a state, got {body}")
        });
    let reason = child(instance, "stateReason");
    (
        state,
        reason.and_then(|reason| child_text(reason, "code")),
        reason.and_then(|reason| child_text(reason, "message")),
    )
}

/// What the instance `instance_id` settled as once the clock-owed moves of its launch fell due.
///
/// Looked for until [`POLL_DEADLINE`] passes rather than waited out, so the answer is the one
/// the clock settled on however fast the host runs.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, Option<String>, Option<String>) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let body = describe(router, case, instance_id).await;
        let answer = reading(case, &body, instance_id);
        if answer.0 != PENDING && answer.0 != SHUTTING_DOWN {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} must settle, last answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: the case, the `KmsKeyId` the template's mapping carries, and the state the instance
/// it launches must settle in.
type FormRow = (String, String, &'static str);

/// One key state as ONE reference form spells it: the state's name, the reference of that form
/// which resolves to it, and the state the launch naming it settles in.
type StateReference = (&'static str, String, &'static str);

/// The four key states, as one reference form spells each of them.
type FormStates = [StateReference; 4];

/// One reference form, and that form's reference for every key state.
type ReferenceForm = (&'static str, FormStates);

/// The rule over the four reference forms the member publishes, crossed with the key states a
/// reference of each form can resolve to.
#[tokio::test]
async fn a_template_launch_runs_only_when_its_volume_key_reference_resolves_to_a_usable_key() {
    let router = emulator();
    let root = root_device(&router, "the root device probe").await;

    // Three keys KMS minted, each with an alias of its own made while the key still accepts
    // alias management, and then moved into the state its rows are about through KMS's own API.
    let (enabled_id, enabled_arn) = key(&router, "a key KMS will serve").await;
    let enabled_alias = alias(&router, "a key KMS will serve", "serving-key", &enabled_id).await;

    let (disabled_id, disabled_arn) = key(&router, "a disabled key").await;
    let disabled_alias = alias(&router, "a disabled key", "disabled-key", &disabled_id).await;
    kms_ok(
        &router,
        "a disabled key",
        "DisableKey",
        json!({ "KeyId": disabled_id.clone() }),
    )
    .await;

    let (pending_id, pending_arn) = key(&router, "a key scheduled for deletion").await;
    let pending_alias = alias(
        &router,
        "a key scheduled for deletion",
        "pending-deletion-key",
        &pending_id,
    )
    .await;
    kms_ok(
        &router,
        "a key scheduled for deletion",
        "ScheduleKeyDeletion",
        json!({ "KeyId": pending_id.clone(), "PendingWindowInDays": 7 }),
    )
    .await;

    // The partition and account every ARN form below is built in, taken from an ARN KMS itself
    // minted.
    let (partition, account) = partition_and_account("a key KMS will serve", &enabled_arn);
    let alias_arn = |name: &str| format!("arn:{partition}:kms:{REGION}:{account}:{name}");
    let absent_key_arn = format!("arn:{partition}:kms:{REGION}:{account}:key/{ABSENT_KEY_ID}");

    for minted in [&enabled_id, &disabled_id, &pending_id] {
        assert_ne!(
            minted.as_str(),
            ABSENT_KEY_ID,
            "the absent key id {ABSENT_KEY_ID:?} must name none of the keys KMS minted"
        );
    }

    // (the form, and what a reference of that form is for each of the four key states)
    let forms: [ReferenceForm; 4] = [
        (
            "the key ID",
            [
                ("a key KMS will serve", enabled_id.clone(), RUNNING),
                ("a disabled key", disabled_id.clone(), TERMINATED),
                (
                    "a key scheduled for deletion",
                    pending_id.clone(),
                    TERMINATED,
                ),
                (
                    "no key of the account",
                    ABSENT_KEY_ID.to_string(),
                    TERMINATED,
                ),
            ],
        ),
        (
            "the key ARN",
            [
                ("a key KMS will serve", enabled_arn.clone(), RUNNING),
                ("a disabled key", disabled_arn.clone(), TERMINATED),
                (
                    "a key scheduled for deletion",
                    pending_arn.clone(),
                    TERMINATED,
                ),
                ("no key of the account", absent_key_arn.clone(), TERMINATED),
            ],
        ),
        (
            "the key alias",
            [
                ("a key KMS will serve", enabled_alias.clone(), RUNNING),
                ("a disabled key", disabled_alias.clone(), TERMINATED),
                (
                    "a key scheduled for deletion",
                    pending_alias.clone(),
                    TERMINATED,
                ),
                (
                    "no key of the account",
                    ABSENT_ALIAS.to_string(),
                    TERMINATED,
                ),
            ],
        ),
        (
            "the alias ARN",
            [
                (
                    "a key KMS will serve",
                    alias_arn(&enabled_alias),
                    RUNNING,
                ),
                ("a disabled key", alias_arn(&disabled_alias), TERMINATED),
                (
                    "a key scheduled for deletion",
                    alias_arn(&pending_alias),
                    TERMINATED,
                ),
                (
                    "no key of the account",
                    alias_arn(ABSENT_ALIAS),
                    TERMINATED,
                ),
            ],
        ),
    ];
    let rows: Vec<FormRow> = forms
        .iter()
        .flat_map(|(form, states)| {
            states.iter().map(move |(state, reference, expected)| {
                (
                    format!("{state} named by {form}"),
                    reference.clone(),
                    *expected,
                )
            })
        })
        .collect();

    // Every row's launch is made FIRST and the instances are then settled together: each
    // instance's moves are owed by the clock from its own launch, so the suite looks for the
    // state each one settled in rather than waiting out one span after another.
    let mut launched: Vec<(FormRow, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (case, reference, _) = row;
        let named = template(
            &router,
            case,
            &format!("volume-key-form-{index}"),
            &root,
            reference,
        )
        .await;
        let (status, body) = launch(&router, &[named]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the launch is accepted whatever the key reference resolves to, \
             got {status} with {body}"
        );
        // Whatever becomes of it, the launch answers the instance it minted in the transient
        // state: the span the clock owes it is never zero.
        let instance_id = element(&body, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: the launch reports its instance: {body}"));
        let (state, _, _) = reading(case, &body, &instance_id);
        assert_eq!(
            state, PENDING,
            "case {case:?}: a fresh instance is {PENDING}, got {body}"
        );
        launched.push((row.clone(), instance_id));
    }

    for ((case, reference, expected), instance_id) in &launched {
        let (state, code, message) = settled(&router, case, instance_id).await;
        assert_eq!(
            state, *expected,
            "case {case:?}: expected {expected} for the reference {reference:?}, got {state:?} \
             with the reason {code:?} on {instance_id}"
        );
        if *expected == TERMINATED {
            assert_eq!(
                code.as_deref(),
                Some(INVALID_KEY_STATE_CODE),
                "case {case:?}: expected the state reason {INVALID_KEY_STATE_CODE} for the \
                 reference {reference:?}, got {code:?} on {instance_id}"
            );
            assert_eq!(
                message.as_deref(),
                Some(INVALID_KEY_STATE_MESSAGE),
                "case {case:?}: expected the state reason message \
                 {INVALID_KEY_STATE_MESSAGE:?} for the reference {reference:?}, got {message:?} \
                 on {instance_id}"
            );
        } else {
            assert_ne!(
                code.as_deref(),
                Some(INVALID_KEY_STATE_CODE),
                "case {case:?}: a reference resolving to a usable key leaves no key-state \
                 reason on {instance_id}"
            );
        }
    }
}
