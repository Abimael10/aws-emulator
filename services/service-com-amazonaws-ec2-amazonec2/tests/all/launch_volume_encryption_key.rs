// SPDX-License-Identifier: CC0-1.0

//! The KMS key the EBS volumes of a LAUNCH are encrypted under, end to end over the wire.
//!
//! Every case drives the real HTTP path — an `ec2Query` form POST through the router's
//! `dispatch` for Amazon EC2, and an `awsJson1_1` POST for AWS KMS — so these are the same
//! bytes an SDK client sends. Nothing reaches into either service's state struct: a key's
//! state is only ever moved through KMS's own API, and an instance's fate is only ever read
//! back through `DescribeInstances`.
//!
//! Two rules, one pair of members on one block device mapping:
//!
//! 1. A key only names HOW a volume is encrypted — "if KmsKeyId is specified, the encrypted
//!    state must be true" (`CreateVolumeRequest$KmsKeyId`, the rule the same pair carries
//!    wherever a volume is created) — so a mapping entry that names a `KmsKeyId` without
//!    setting `Encrypted` to true names a volume Amazon EC2 cannot create, and the launch is
//!    refused `InvalidParameterDependency` for the half it left out. That is parameter
//!    validation, so it is answered before the `DryRun` short-circuit and nothing is
//!    launched.
//! 2. An encrypted volume's data key comes from the KMS key the mapping names, so a key KMS
//!    will not produce one under — disabled, pending deletion, or naming no key of the
//!    account — leaves the instance with no volume to boot from: the launch is still served
//!    and answers the `pending` instance it minted, and that instance reaches `terminated`
//!    with the `Client.InvalidKMSKey.InvalidState` state reason instead of `running`. A key
//!    that will serve, and a launch naming no customer managed key, reach `running` as ever.
//!
//! The rows span what each rule generalises over: WHERE the mapping comes from (the
//! request's own `BlockDeviceMappings` and a launch template's), WHICH device it is for (the
//! AMI's own root device and another one), what the `Encrypted` member says (absent, false,
//! true), whether the call is a dry run, and — for rule 2 — the KEY STATE (enabled, disabled,
//! pending deletion, disabled and enabled again, no key of the account at all). So no row is
//! the specification.
//!
//! AWS KMS is mounted BESIDE Amazon EC2 on one engine, exactly as the assembled router
//! mounts them, and every key a row names was MINTED by KMS itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_kms_trentservice as kms;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, indexed};
use super::wire::{child, child_text, document};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// An AMI id of the right FORM that this account holds no image for: the launch is served,
/// and its root device is the one the image this crate publishes by default carries — which
/// every row reads off a probe launch rather than writing it out here.
const AMI: &str = "ami-0123456789abcdef0";

/// A device that is not the AMI's root one, so a row can show the rules hold for any entry of
/// the mapping rather than only for the device the instance boots from.
const OTHER_DEVICE: &str = "/dev/sdf";

/// The refusal a mapping that names a key without asking for encryption is answered with, and
/// EC2's whole sentence for it — spelled out here rather than imported from the service crate,
/// so a wrong constant in the implementation cannot make this suite pass.
const DEPENDENCY_CODE: &str = "InvalidParameterDependency";
const DEPENDENCY_MESSAGE: &str = "The parameter KmsKeyId requires the parameter Encrypted to be set.";

/// The `StateReason` an instance whose volume key will not serve it reports, likewise spelled
/// out here.
const INVALID_KEY_STATE_CODE: &str = "Client.InvalidKMSKey.InvalidState";
const INVALID_KEY_STATE_MESSAGE: &str =
    "Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state";

/// The two `InstanceStateName`s a launch settles in.
const RUNNING: &str = "running";
const TERMINATED: &str = "terminated";

/// The state every launch answers with, and the state an instance on its way out passes
/// through: the two a settling poll waits to leave, neither of them a state an instance stays
/// in.
const PENDING: &str = "pending";
const SHUTTING_DOWN: &str = "shutting-down";

/// How long a poll keeps looking for the move the clock owes an instance before it gives up: a
/// generous multiple of the service's own span, so a loaded host is never the reason a case
/// fails.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

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
                "AWS4-HMAC-SHA256 Credential=AKIDLAUNCHKEY/20260101/{REGION}/kms/aws4_request, \
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

/// One customer managed key KMS minted, answering its key id.
async fn key(router: &MultiServiceRouter, case: &str) -> String {
    let created = kms_ok(router, case, "CreateKey", json!({})).await;
    created["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateKey reports the key it minted, got {created}")
        })
        .to_string()
}

/// One `RunInstances` of a single instance, with `extra` beside the members every row carries.
async fn launch(router: &MultiServiceRouter, extra: &[(String, String)]) -> (u16, String) {
    let mut params = vec![p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")];
    params.extend_from_slice(extra);
    call(router, REGION, "RunInstances", &params).await
}

/// The root device name an instance launched from [`AMI`] reports, read off a probe launch
/// rather than written here: it is the AMI's own `rootDeviceName`.
async fn root_device(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = launch(router, &[]).await;
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

/// The `BlockDeviceMapping.1` fields of a REQUEST that name `device` with the Amazon EBS
/// members `members` carries.
fn request_mapping(device: &str, members: &[(&str, &str)]) -> Params {
    let mut params = vec![p("BlockDeviceMapping.1.DeviceName", device)];
    for (member, value) in members {
        params.push(p(&format!("BlockDeviceMapping.1.Ebs.{member}"), *value));
    }
    params
}

/// The same mapping as the launch parameters of a launch template VERSION.
fn template_mapping(device: &str, members: &[(&str, &str)]) -> Params {
    let mut params = vec![p("LaunchTemplateData.BlockDeviceMapping.1.DeviceName", device)];
    for (member, value) in members {
        params.push(p(
            &format!("LaunchTemplateData.BlockDeviceMapping.1.Ebs.{member}"),
            *value,
        ));
    }
    params
}

/// A launch template named `name` whose one version carries `data`, answering the
/// `LaunchTemplate.LaunchTemplateId` field a launch names it by.
///
/// The template itself is always stored: Amazon EC2 keeps the launch parameters a version
/// carries and resolves them when the version is CONSUMED, so a version naming a key — with or
/// without the encryption flag, usable or not — is created and only its launch is judged.
async fn template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    data: &[(String, String)],
) -> (String, String) {
    let mut params = vec![p("LaunchTemplateName", name), p("LaunchTemplateData.ImageId", AMI)];
    params.extend_from_slice(data);
    let (status, body) = call(router, REGION, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must answer 200, got {status} with {body}"
    );
    let launch_template_id = element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: no launch template id in {body}"));
    p("LaunchTemplate.LaunchTemplateId", launch_template_id)
}

/// How many instances the account holds — what a refused launch must leave untouched. Read as
/// the SET of reported ids, so an id a report names twice is still one instance.
async fn instance_count(router: &MultiServiceRouter, case: &str) -> usize {
    let (status, body) = call(router, REGION, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must answer 200, got {status} with {body}"
    );
    elements(&body, "instanceId")
        .into_iter()
        .collect::<std::collections::BTreeSet<String>>()
        .len()
}

/// The `InstanceStateName` the one instance of `body` reports, read through the document so
/// the state of the INSTANCE is never confused with a member of the same name elsewhere.
fn reported_state(case: &str, body: &str) -> String {
    let parsed = document(case, body);
    let instance = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "instanceState").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no instance reported in {body}"));
    child(instance, "instanceState")
        .and_then(|state| child_text(state, "name"))
        .unwrap_or_else(|| panic!("case {case:?}: the instance reports no state in {body}"))
}

/// What the instance `instance_id` settled as once the clock-owed move fell due: its
/// `InstanceStateName` and the `StateReason` it reports, as `(state, code, message)`.
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
        let reading = {
            let parsed = document(case, &body);
            let instance = parsed
                .descendants()
                .find(|node| node.is_element() && child(*node, "instanceState").is_some())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no instance reported for {instance_id}: {body}")
                });
            let state = child(instance, "instanceState")
                .and_then(|state| child_text(state, "name"))
                .unwrap_or_default();
            let reason = child(instance, "stateReason");
            (
                state,
                reason.and_then(|reason| child_text(reason, "code")),
                reason.and_then(|reason| child_text(reason, "message")),
            )
        };
        if reading.0 != PENDING && reading.0 != SHUTTING_DOWN {
            return reading;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} must settle, last answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE 1 over the mapping entries that name a key: the refusal, and that it is answered
/// whatever the call would otherwise have done.
#[tokio::test]
async fn a_block_device_key_without_the_encryption_flag_refuses_the_launch() {
    let router = emulator();
    let root = root_device(&router, "the root device probe").await;
    // A key KMS holds and will serve: the refusal below is about the PARAMETERS, not about
    // the key, so every row names a key that is in no way at fault.
    let usable = key(&router, "the key every row names").await;

    // (case, the fields the launch carries, the refusal it must answer)
    let mut rows: Vec<(String, Params, Option<&str>)> = Vec::new();
    for (flag, refused) in [
        (None, true),
        (Some("false"), true),
        (Some("true"), false),
    ] {
        for device in [root.as_str(), OTHER_DEVICE] {
            for dry_run in [false, true] {
                let mut members: Vec<(&str, &str)> = vec![("KmsKeyId", usable.as_str())];
                if let Some(flag) = flag {
                    members.push(("Encrypted", flag));
                }
                let mut params = request_mapping(device, &members);
                if dry_run {
                    params.push(p("DryRun", "true"));
                }
                rows.push((
                    format!(
                        "a request mapping for {device} with Encrypted={flag:?}, DryRun={dry_run}"
                    ),
                    params,
                    if refused {
                        Some(DEPENDENCY_CODE)
                    } else if dry_run {
                        Some("DryRunOperation")
                    } else {
                        None
                    },
                ));
            }
        }
    }
    // The same mapping, carried by a launch template instead: a template-driven launch is held
    // to the rule exactly as a spelled-out one is.
    for (index, (flag, refused)) in [(None, true), (Some("false"), true), (Some("true"), false)]
        .into_iter()
        .enumerate()
    {
        let case = format!("a template mapping with Encrypted={flag:?}");
        let mut members: Vec<(&str, &str)> = vec![("KmsKeyId", usable.as_str())];
        if let Some(flag) = flag {
            members.push(("Encrypted", flag));
        }
        let named = template(
            &router,
            &case,
            &format!("key-dependency-{index}"),
            &template_mapping(&root, &members),
        )
        .await;
        rows.push((
            case,
            vec![named, p("DryRun", "true")],
            if refused {
                Some(DEPENDENCY_CODE)
            } else {
                Some("DryRunOperation")
            },
        ));
    }

    for (case, params, expected) in &rows {
        let before = instance_count(&router, case).await;
        let (status, body) = launch(&router, params).await;
        match expected {
            Some(code) => {
                assert_eq!(
                    error_code(&body),
                    *code,
                    "case {case:?}: expected the code {code}, got {status} with {body}"
                );
                if *code == DEPENDENCY_CODE {
                    assert_eq!(
                        status, 400,
                        "case {case:?}: a parameter refusal is a 400, got {status} with {body}"
                    );
                    assert_eq!(
                        error_message(&body),
                        DEPENDENCY_MESSAGE,
                        "case {case:?}: expected EC2's own sentence, got {body}"
                    );
                }
                assert_eq!(
                    instance_count(&router, case).await,
                    before,
                    "case {case:?}: a refused launch mints no instance, got {body}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a mapping that pairs the key with Encrypted=true is \
                     served, got {status} with {body}"
                );
                assert_eq!(
                    instance_count(&router, case).await,
                    before + 1,
                    "case {case:?}: the served launch mints one instance, got {body}"
                );
            }
        }
    }
}

/// RULE 2 over the states a KMS key can be in when a launch's volumes name it.
#[tokio::test]
async fn an_instance_runs_only_while_its_volume_key_will_produce_a_data_key() {
    let router = emulator();
    let root = root_device(&router, "the root device probe").await;

    // Each key is minted by KMS itself and then moved into the state its row is about, through
    // KMS's own API.
    let enabled = key(&router, "an enabled key").await;
    let disabled = key(&router, "a disabled key").await;
    kms_ok(
        &router,
        "a disabled key",
        "DisableKey",
        json!({ "KeyId": disabled.clone() }),
    )
    .await;
    let pending_deletion = key(&router, "a key pending deletion").await;
    kms_ok(
        &router,
        "a key pending deletion",
        "ScheduleKeyDeletion",
        json!({ "KeyId": pending_deletion.clone(), "PendingWindowInDays": 7 }),
    )
    .await;
    let re_enabled = key(&router, "a re-enabled key").await;
    for operation in ["DisableKey", "EnableKey"] {
        kms_ok(
            &router,
            "a re-enabled key",
            operation,
            json!({ "KeyId": re_enabled.clone() }),
        )
        .await;
    }

    // (case, the fields the launch carries, the state it settles in)
    let mut rows: Vec<(String, Params, &str)> = vec![
        (
            "a launch naming no customer managed key".to_string(),
            Vec::new(),
            RUNNING,
        ),
        (
            "an encrypted root volume naming no key of its own".to_string(),
            request_mapping(&root, &[("Encrypted", "true")]),
            RUNNING,
        ),
    ];
    for (case, key_id, expected) in [
        ("a key KMS will produce a data key under", &enabled, RUNNING),
        ("a disabled key", &disabled, TERMINATED),
        ("a key scheduled for deletion", &pending_deletion, TERMINATED),
        ("a key disabled and then enabled again", &re_enabled, RUNNING),
    ] {
        rows.push((
            format!("{case}, on the root device of the request's own mapping"),
            request_mapping(&root, &[("Encrypted", "true"), ("KmsKeyId", key_id)]),
            expected,
        ));
    }
    // A key identifier no key of the account carries: there is no key to encrypt the volume
    // with, which is the same outcome as a key in a state KMS will not use.
    rows.push((
        "a key identifier no key of the account carries".to_string(),
        request_mapping(
            &root,
            &[
                ("Encrypted", "true"),
                ("KmsKeyId", "11111111-2222-3333-4444-555555555555"),
            ],
        ),
        TERMINATED,
    ));
    // The same unusable key on a device that is NOT the root one: the rule is about the volumes
    // the launch creates, not about which device the instance boots from.
    rows.push((
        "a disabled key on a device that is not the root one".to_string(),
        request_mapping(
            OTHER_DEVICE,
            &[("Encrypted", "true"), ("KmsKeyId", disabled.as_str())],
        ),
        TERMINATED,
    ));
    // And the same key states reached through a launch TEMPLATE, which is the launch form an
    // Auto Scaling or CloudFormation shaped workload uses.
    for (index, (case, key_id, expected)) in [
        ("a template naming a key KMS will serve", &enabled, RUNNING),
        ("a template naming a disabled key", &disabled, TERMINATED),
    ]
    .into_iter()
    .enumerate()
    {
        let named = template(
            &router,
            case,
            &format!("key-state-{index}"),
            &template_mapping(&root, &[("Encrypted", "true"), ("KmsKeyId", key_id)]),
        )
        .await;
        rows.push((case.to_string(), vec![named], expected));
    }

    for (case, params, expected) in &rows {
        let (status, body) = launch(&router, params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the launch is accepted whatever the key's state, got {status} \
             with {body}"
        );
        // Whatever becomes of it, the launch answers the instance it minted in the transient
        // state: the span the clock owes it is never zero.
        assert_eq!(
            reported_state(case, &body),
            PENDING,
            "case {case:?}: a fresh instance is {PENDING}, got {body}"
        );
        let instance_id = element(&body, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: the launch reports its instance: {body}"));

        let (state, code, message) = settled(&router, case, &instance_id).await;
        assert_eq!(
            state, *expected,
            "case {case:?}: expected {expected}, got {state:?} for {instance_id}"
        );
        if *expected == TERMINATED {
            assert_eq!(
                code.as_deref(),
                Some(INVALID_KEY_STATE_CODE),
                "case {case:?}: expected the state reason {INVALID_KEY_STATE_CODE}, got \
                 {code:?} for {instance_id}"
            );
            assert_eq!(
                message.as_deref(),
                Some(INVALID_KEY_STATE_MESSAGE),
                "case {case:?}: expected the state reason message \
                 {INVALID_KEY_STATE_MESSAGE:?}, got {message:?} for {instance_id}"
            );
            // The volumes that launch created never came into being either: the key refused
            // their data key, so the account holds none of them.
            let (status, volumes) = call(&router, REGION, "DescribeVolumes", &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeVolumes must answer 200, got {status} with {volumes}"
            );
            assert!(
                !volumes.contains(instance_id.as_str()),
                "case {case:?}: no volume is attached to {instance_id}, got {volumes}"
            );
        } else {
            assert_ne!(
                code.as_deref(),
                Some(INVALID_KEY_STATE_CODE),
                "case {case:?}: a usable key leaves no key-state reason on {instance_id}"
            );
        }
    }
}
