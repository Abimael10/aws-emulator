// SPDX-License-Identifier: CC0-1.0

//! The VOLUMES a launch created when the KMS key they name will not serve them, end to end
//! over the wire.
//!
//! The instance's own fate under such a key — `pending` and then `terminated` with the
//! key-state reason — is the subject of `launch_volume_encryption_key`; this suite is about
//! the other half of that outcome, and about the launch as a BATCH:
//!
//! 1. A volume whose data key KMS will not produce never comes into existence, so the volumes
//!    a launch created for an instance it then takes back are WITHDRAWN rather than left
//!    attached to an instance that is gone: nothing is attached to the instance and the id the
//!    volume was created under is absent from `DescribeVolumes` altogether — exactly as the
//!    volume of a `CreateVolume` under such a key is. A key that will serve leaves the
//!    instance's root volume where it belongs, `in-use` by the instance holding it.
//! 2. The key is asked about the LAUNCH, so every instance one call minted shares its fate:
//!    a launch of several instances under an unusable key terminates all of them, not the
//!    first.
//!
//! Every case drives the real HTTP path — an `ec2Query` form POST through the router's
//! `dispatch` for Amazon EC2, and an `awsJson1_1` POST for AWS KMS — so these are the same
//! bytes an SDK client sends. Nothing reaches into either service's state struct: a key's
//! state is only ever moved through KMS's own API, and the outcome is only ever read back
//! through `DescribeInstances` and `DescribeVolumes`.
//!
//! AWS KMS is mounted BESIDE Amazon EC2 on one engine, exactly as the assembled router
//! mounts them, and every key a row names was MINTED by KMS itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_kms_trentservice as kms;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, filters, indexed};
use super::wire::{child, child_text, document, Param};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// An AMI id of the right FORM that this account holds no image for: the launch is still
/// served, and its root device is the one the image this crate publishes by default carries —
/// which every row reads off a probe launch rather than writing it out here.
const AMI: &str = "ami-0123456789abcdef0";

/// The `StateReason$Code` EC2 records on an instance whose EBS volume could not be created
/// because the KMS key it was to be encrypted under will not serve it.
const INVALID_KMS_KEY_STATE: &str = "Client.InvalidKMSKey.InvalidState";

/// The whole `StateReason$Message` that goes with it: the code followed by fixed text that
/// restates neither the key nor the state it is in.
const INCORRECT_KEY_STATE: &str =
    "Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state";

/// The state a launch whose volumes cannot be created leaves its instance in.
const TERMINATED: &str = "terminated";

/// The state every other launch leaves its instance in.
const RUNNING: &str = "running";

/// How long a poll loop keeps looking for a clock-owed move before it gives up — a generous
/// multiple of the service's own span, so a loaded host is never the reason a case fails.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

fn p(name: &str, value: impl Into<String>) -> Param {
    (name.to_string(), value.into())
}

/// Amazon EC2 and AWS KMS over ONE shared engine, exactly as the assembled router mounts
/// them: the key an encrypted volume names is a resource of that service, created through its
/// own API.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kms::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// One call that must be served, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
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
                "AWS4-HMAC-SHA256 Credential=AKIDROOTVOLUME/20260101/{REGION}/kms/aws4_request, \
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

/// One `RunInstances` of `count` instances from [`AMI`] with `extra` beside it, answering the
/// instance ids it reports — every one of them `pending`, which is what a launch answers
/// whatever becomes of the instances afterwards.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    count: u32,
    extra: &[Param],
) -> Vec<String> {
    let mut params = vec![
        p("ImageId", AMI),
        p("MinCount", count.to_string()),
        p("MaxCount", count.to_string()),
    ];
    params.extend_from_slice(extra);
    let body = ok(router, case, "RunInstances", &params).await;
    let launched = elements(&body, "instanceId");
    assert_eq!(
        launched.len(),
        count as usize,
        "case {case:?}: the launch must report {count} instances, got {body}"
    );
    launched
}

/// The `BlockDeviceMapping.1` fields that name `device` with the Amazon EBS members `members`
/// carries.
fn mapping(device: &str, members: &[(&str, &str)]) -> Vec<Param> {
    let mut params = vec![p("BlockDeviceMapping.1.DeviceName", device)];
    for (member, value) in members {
        params.push(p(&format!("BlockDeviceMapping.1.Ebs.{member}"), *value));
    }
    params
}

/// The root device name an instance launched from [`AMI`] reports, read off a launch rather
/// than written here: it is the AMI's own `rootDeviceName`, and the device a mapping has to
/// name to define the root volume.
async fn root_device(router: &MultiServiceRouter, case: &str) -> String {
    let instance_id = launch(router, case, 1, &[]).await.remove(0);
    let described = ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    element(&described, "rootDeviceName").unwrap_or_else(|| {
        panic!("case {case:?}: the instance must report its root device: {described}")
    })
}

/// What `DescribeInstances` reports about `instance_id` right now: the `instanceState` name and
/// the `stateReason`, as `(code, message)`.
///
/// Read through the document, so a member of the instance is never taken from one of the sets
/// nested inside it.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, Option<(String, String)>) {
    let body = ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "instanceState").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: {instance_id} must be reported: {body}"));
    let state = child(instance, "instanceState")
        .and_then(|state| child_text(state, "name"))
        .unwrap_or_else(|| panic!("case {case:?}: {instance_id} must report a state: {body}"));
    let reason = child(instance, "stateReason").map(|reason| {
        (
            child_text(reason, "code").unwrap_or_default(),
            child_text(reason, "message").unwrap_or_default(),
        )
    });
    (state, reason)
}

/// What became of `instance_id` once the clock-owed moves of its launch fell due: the state it
/// settled in, the `stateReason` it reports there, and every state this suite SAW it in on the
/// way.
///
/// Looked for until the deadline passes rather than waited out, so the answer is the one the
/// clock settled on however fast the host runs.
async fn fate(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, Option<(String, String)>, Vec<String>) {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (state, reason) = reported(router, case, instance_id).await;
        if seen.last().map(String::as_str) != Some(state.as_str()) {
            seen.push(state.clone());
        }
        if state == RUNNING || state == TERMINATED {
            return (state, reason, seen);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} must settle, last seen {seen:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The volume `DescribeVolumes` reports attached to `instance_id`, as `(volume id, status)`, or
/// `None` when the account holds none.
async fn attached_volume(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Option<(String, String)> {
    let body = ok(
        router,
        case,
        "DescribeVolumes",
        &filters(&[("attachment.instance-id", &[instance_id])]),
    )
    .await;
    let volume_id = element(&body, "volumeId")?;
    Some((volume_id, element(&body, "status").unwrap_or_default()))
}

/// One row of the key-state table: the case, the Amazon EBS members the launch's root device
/// entry carries, and the state the instance must settle in.
type KeyStateRow = (&'static str, Vec<(&'static str, String)>, &'static str);

/// RULE 1: the volumes of a launch the key refused are GONE, and the volume of one it serves
/// is in use by the instance that holds it.
///
/// The rows span the key states the rule generalises over — a key KMS will produce a data key
/// under, the same key once DISABLED, a key scheduled for deletion, a key identifier no key of
/// the account carries, and a root volume encrypted under no key of its own — so what becomes
/// of the volume follows the key's state rather than the shape of the request.
#[tokio::test]
async fn the_volumes_of_a_launch_under_an_unusable_key_are_withdrawn() {
    let router = emulator();

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

    let device = root_device(&router, "the AMI's own root device").await;

    let rows: Vec<KeyStateRow> = vec![
        (
            "a key KMS will produce a data key under",
            vec![("Encrypted", "true".to_string()), ("KmsKeyId", enabled)],
            RUNNING,
        ),
        (
            "a disabled key",
            vec![
                ("Encrypted", "true".to_string()),
                ("KmsKeyId", disabled.clone()),
            ],
            TERMINATED,
        ),
        (
            "a key scheduled for deletion",
            vec![
                ("Encrypted", "true".to_string()),
                ("KmsKeyId", pending_deletion),
            ],
            TERMINATED,
        ),
        (
            "a key identifier no key of the account carries",
            vec![
                ("Encrypted", "true".to_string()),
                (
                    "KmsKeyId",
                    "11111111-2222-3333-4444-555555555555".to_string(),
                ),
            ],
            TERMINATED,
        ),
        (
            "an encrypted root volume naming no key of its own",
            vec![("Encrypted", "true".to_string())],
            RUNNING,
        ),
    ];

    for (case, members, expected) in &rows {
        let borrowed: Vec<(&str, &str)> = members
            .iter()
            .map(|(member, value)| (*member, value.as_str()))
            .collect();
        let instance_id = launch(&router, case, 1, &mapping(&device, &borrowed))
            .await
            .remove(0);
        // The root volume the launch created, named before the clock has settled its fate: a
        // volume that cannot be created is withdrawn at the instant it would have become
        // available, so this is the one chance to learn its id.
        let created_volume = attached_volume(&router, case, &instance_id).await;
        let (settled, reason, seen) = fate(&router, case, &instance_id).await;
        assert_eq!(
            settled, *expected,
            "case {case:?}: expected {expected}, got {settled:?} with reason {reason:?} \
             (states seen: {seen:?})"
        );

        if *expected == TERMINATED {
            // It never ran: the instance went from the state the launch answered to the end
            // the refused volumes owed it.
            assert!(
                !seen.iter().any(|state| state == RUNNING),
                "case {case:?}: an instance whose volumes cannot be created must never run, \
                 states seen {seen:?}"
            );
            assert_eq!(
                reason,
                Some((
                    INVALID_KMS_KEY_STATE.to_string(),
                    INCORRECT_KEY_STATE.to_string()
                )),
                "case {case:?}: expected the invalid-key-state reason, got {reason:?}"
            );
            // The volume is GONE, not merely unreported: nothing is attached to the instance
            // any more and the id it was created under is absent from the listing.
            let attached = attached_volume(&router, case, &instance_id).await;
            assert_eq!(
                attached, None,
                "case {case:?}: a volume that could not be created must be gone, got \
                 {attached:?}"
            );
            if let Some((volume_id, _)) = &created_volume {
                let listed = ok(&router, case, "DescribeVolumes", &[]).await;
                assert!(
                    !listed.contains(volume_id.as_str()),
                    "case {case:?}: {volume_id} must be gone from the listing, got {listed}"
                );
            }
        } else {
            let attached = attached_volume(&router, case, &instance_id).await;
            assert_eq!(
                attached.as_ref().map(|(_, status)| status.as_str()),
                // "in-use" — the `VolumeState` of a volume that was created and is attached to
                // the instance holding it, which is what a launch's root volume is.
                Some("in-use"),
                "case {case:?}: the root volume must be in use by the instance, got {attached:?}"
            );
        }
    }
}

/// RULE 2: the key is asked about the LAUNCH, so every instance one call minted is taken back
/// — not just the first.
#[tokio::test]
async fn every_instance_of_a_launch_under_an_unusable_key_terminates() {
    let router = emulator();
    let case = "three instances under a disabled key";
    let disabled = key(&router, case).await;
    kms_ok(
        &router,
        case,
        "DisableKey",
        json!({ "KeyId": disabled.clone() }),
    )
    .await;
    let device = root_device(&router, case).await;

    let launched = launch(
        &router,
        case,
        3,
        &mapping(&device, &[("Encrypted", "true"), ("KmsKeyId", &disabled)]),
    )
    .await;
    for instance_id in &launched {
        let (settled, reason, seen) = fate(&router, case, instance_id).await;
        assert_eq!(
            settled, TERMINATED,
            "case {case:?}: {instance_id} must terminate, got {settled:?} with reason \
             {reason:?} (states seen: {seen:?})"
        );
        assert_eq!(
            reason,
            Some((
                INVALID_KMS_KEY_STATE.to_string(),
                INCORRECT_KEY_STATE.to_string()
            )),
            "case {case:?}: {instance_id} must carry the invalid-key-state reason, got \
             {reason:?}"
        );
        let attached = attached_volume(&router, case, instance_id).await;
        assert_eq!(
            attached, None,
            "case {case:?}: the volumes of {instance_id} must be gone, got {attached:?}"
        );
    }
}
