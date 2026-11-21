// SPDX-License-Identifier: CC0-1.0

//! WHICH record a key push is served against, for both EC2 Instance Connect pushes.
//!
//! RULE: an EC2 instance is Amazon EC2's resource, so a key push resolves the id it names
//! through the owner of that id — the instance EC2 reports IS the record the push is decided
//! by, an id EC2 answers with no such instance for is not found whatever this service's own
//! fallback registry holds for it, and that registry decides a push only where no EC2 can be
//! asked at all. `SendSSHPublicKey` and `SendSerialConsoleSSHPublicKey` apply the same rule,
//! out of one place
//! (`service_com_amazonaws_ec2instanceconnect_awsec2instanceconnectservice::state::ec2::InstanceLookup::resolve`),
//! so neither operation can answer a different error than the other for the same instance id.
//!
//! This suite lives beside the router because the rule is about ANOTHER service's answer: the
//! instance a row pushes at is launched through Amazon EC2's own `RunInstances` on the SAME
//! engine that serves EC2 Instance Connect, and the row where EC2 cannot be asked is an
//! emulator with no EC2 mounted at all.
//!
//! The rows span what the rule generalises over — the three answers an owner can give for an
//! id (it reports an instance, it reports none, it cannot be asked) — and every row keeps a
//! record for the SAME id in this service's own fallback registry, in the lifecycle state that
//! would produce a DIFFERENT answer. A row that consulted the registry where EC2 answered, or
//! that ignored it where no EC2 was mounted, therefore cannot pass. Both operations are driven
//! in every row.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2instanceconnect_awsec2instanceconnectservice as ec2ic;

use crate::query_wire::{block_of, element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The EC2 API version an ec2Query body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the launch below runs from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// An instance type that runs on the AWS Nitro System, which is what the serial console
/// supports — so the serial-console row is decided by the instance's resolution rather than
/// by its type.
const NITRO_INSTANCE_TYPE: &str = "m5.large";

/// The `X-Amz-Target` values of this service's wire protocol
/// (`AWSEC2InstanceConnectService.<OperationName>`).
const SEND_SSH: &str = "AWSEC2InstanceConnectService.SendSSHPublicKey";
const SEND_SERIAL: &str = "AWSEC2InstanceConnectService.SendSerialConsoleSSHPublicKey";

/// The OS user every `SendSSHPublicKey` below names.
const OS_USER: &str = "ec2-user";

/// A well-formed instance id (`^i-[a-f0-9]+$`, 10..32 characters) that no launch below
/// creates, so EC2 holds no instance under it and the push is refused for the instance rather
/// than for the shape of its id.
const UNLAUNCHED_INSTANCE_ID: &str = "i-0a1b2c3d4e5f6a7b8";

/// The Availability Zone the fallback registry's record carries.
const REGISTRY_ZONE: &str = "us-east-1a";

/// The message AWS words `EC2InstanceNotFoundException` with.
const NOT_FOUND_MESSAGE: &str = "Instance not found.";

/// How long a poll waits for a condition the clock owes (a launched instance reaching
/// `running`). Generous by design: it is a deadline, not a measurement, and nothing below
/// asserts how long anything took.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One emulator over one shared engine: EC2 Instance Connect always, and Amazon EC2 only
/// where the row has an owner to ask.
fn emulator(with_ec2: bool) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2ic::EmulatedService::new(Arc::clone(&engine)));
    if with_ec2 {
        router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
            Arc::clone(&engine),
        ));
    }
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and which Region it is served in; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// POST one ec2Query action that must succeed, answered with its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must succeed, got {status} with {answered}"
    );
    answered
}

/// One EC2 Instance Connect call: `POST /` with an `X-Amz-Target` and an awsJson1_1 body.
async fn connect(router: &MultiServiceRouter, target: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("ec2-instance-connect.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header("authorization", authorization("ec2-instance-connect"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The error code an awsJson1_1 error envelope carries in `__type`, or `""` for a success.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `Message` of an error body, or `""` for a success.
fn message(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Key material satisfying the modelled `SSHPublicKey` length (80..4096); `tag` makes each
/// one distinct, so the key read back out of state is the key that call pushed.
fn ssh_key(tag: &str) -> String {
    format!("ssh-ed25519 {} {tag}@emulator", "A".repeat(80))
}

/// This service's store, as its own `service_state` accessor hands it back.
type ConnectStore = engine::ServiceState<
    ec2ic::state::ComAmazonawsEc2instanceconnectAwsec2instanceconnectserviceState,
>;

/// This service's own store in the shared engine — reached exactly as a handler reaches it.
fn connect_store(engine: &Arc<engine::InMemoryEngine>) -> ConnectStore {
    ec2ic::state::service_state(&**engine).expect("the service state must be reachable")
}

/// Launch one Nitro instance through EC2's own `RunInstances` and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let launched = ec2(
        router,
        case,
        "RunInstances",
        &[
            ("ImageId", AMI),
            ("InstanceType", NITRO_INSTANCE_TYPE),
            ("MinCount", "1"),
            ("MaxCount", "1"),
        ],
    )
    .await;
    element(&launched, "instanceId")
}

/// Poll EC2 until it reports `instance_id` `running`, the state a push is served in. The
/// CONDITION is what the wait ends on.
async fn poll_until_running(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let described = ec2(
            router,
            case,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        )
        .await;
        let state_block = block_of(&described, "instanceState").unwrap_or_default();
        let names = elements(state_block, "name");
        if names.iter().any(|name| name == "running") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: EC2 must report {instance_id} running, got {names:?} in {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: what the owner of the instance answers for the id a push names, and what the
/// fallback rule owes BOTH pushes for it.
struct Row {
    case: &'static str,
    /// Whether an Amazon EC2 is mounted beside this service at all: with none, no service can
    /// answer for an instance and nothing is known about the id.
    ec2_mounted: bool,
    /// Whether the id the pushes name is one EC2 launched (EC2 reports an instance) or one EC2
    /// was never asked to create (EC2 answers it holds none).
    launched_by_ec2: bool,
    /// The lifecycle state this service's OWN fallback registry holds for that same id. Where
    /// EC2 answers, this is deliberately the state that would produce another answer, so a
    /// push that read the registry there could not pass.
    registry_state: ec2ic::state::InstanceState,
    /// The error code both pushes must answer with, or `""` where both must succeed.
    expected_error: &'static str,
}

#[tokio::test]
async fn both_pushes_resolve_the_instance_through_its_owner_and_fall_back_only_unanswered() {
    let rows = [
        Row {
            case: "EC2 reports the instance it launched",
            ec2_mounted: true,
            launched_by_ec2: true,
            // EC2 reports it running, so the push is served; a registry that decided it would
            // refuse the same id for the state it keeps.
            registry_state: ec2ic::state::InstanceState::Terminated,
            expected_error: "",
        },
        Row {
            case: "EC2 holds no instance under the id",
            ec2_mounted: true,
            launched_by_ec2: false,
            // Running and serial-console capable: a registry that had a say would serve the
            // push EC2 answered for with no instance.
            registry_state: ec2ic::state::InstanceState::Running,
            expected_error: "EC2InstanceNotFoundException",
        },
        Row {
            case: "no EC2 is mounted, so nothing can be asked about the id",
            ec2_mounted: false,
            launched_by_ec2: false,
            registry_state: ec2ic::state::InstanceState::Running,
            expected_error: "",
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, engine) = emulator(row.ec2_mounted);
        let store = connect_store(&engine);

        // The account-level serial-console toggle, granted where the row's serial-console push
        // can be granted it: through EC2's own `EnableSerialConsoleAccess` where EC2 is
        // mounted, and on this service's own toggle where it is not. Either way the
        // serial-console row is decided by the instance's resolution rather than by the
        // account gate in front of it.
        if row.ec2_mounted {
            ec2(&router, case, "EnableSerialConsoleAccess", &[]).await;
        } else {
            store.lock().serial_console_access_enabled = true;
        }

        let instance_id = if row.launched_by_ec2 {
            let launched = launch(&router, case).await;
            poll_until_running(&router, case, &launched).await;
            launched
        } else {
            UNLAUNCHED_INSTANCE_ID.to_string()
        };

        // The fallback registry holds a record for the very id under test, in every row.
        store.lock().register_instance(ec2ic::state::Instance::new(
            &instance_id,
            REGISTRY_ZONE,
            NITRO_INSTANCE_TYPE,
            row.registry_state,
            /* serial_console_supported */ true,
        ));

        let ssh_material = ssh_key("ssh");
        let (status, body) = connect(
            &router,
            SEND_SSH,
            &json!({
                "InstanceId": instance_id,
                "InstanceOSUser": OS_USER,
                "SSHPublicKey": ssh_material,
            }),
        )
        .await;
        assert_eq!(
            error_code(&body),
            row.expected_error,
            "case {case:?}: SendSSHPublicKey must answer the error the owner's answer owes it, \
             got {status} {body}"
        );

        let serial_material = ssh_key("serial");
        let (serial_status, serial_body) = connect(
            &router,
            SEND_SERIAL,
            &json!({
                "InstanceId": instance_id,
                "SerialPort": ec2ic::state::SERIAL_CONSOLE_PORT,
                "SSHPublicKey": serial_material,
            }),
        )
        .await;
        assert_eq!(
            error_code(&serial_body),
            row.expected_error,
            "case {case:?}: SendSerialConsoleSSHPublicKey must answer exactly what \
             SendSSHPublicKey answered for the same id, got {serial_status} {serial_body}"
        );

        if row.expected_error.is_empty() {
            // A success is backed by the key really stored against that instance, for each
            // operation's own key space.
            assert_eq!(
                status, 200,
                "case {case:?}: SendSSHPublicKey must answer HTTP 200, got {status} {body}"
            );
            assert_eq!(
                body.get("Success").and_then(Value::as_bool),
                Some(true),
                "case {case:?}: SendSSHPublicKey must answer Success true, got {body}"
            );
            assert_eq!(
                serial_status, 200,
                "case {case:?}: SendSerialConsoleSSHPublicKey must answer HTTP 200, got \
                 {serial_status} {serial_body}"
            );
            assert_eq!(
                serial_body.get("Success").and_then(Value::as_bool),
                Some(true),
                "case {case:?}: SendSerialConsoleSSHPublicKey must answer Success true, got \
                 {serial_body}"
            );

            let now = Instant::now();
            let guard = store.lock();
            let stored = guard
                .active_ssh_key(&instance_id, OS_USER, now)
                .map(|pushed| pushed.public_key.clone());
            assert_eq!(
                stored.as_deref(),
                Some(ssh_material.as_str()),
                "case {case:?}: the pushed key must be readable from state, got {stored:?}"
            );
            let stored_serial = guard
                .active_serial_console_key(&instance_id, ec2ic::state::SERIAL_CONSOLE_PORT, now)
                .map(|pushed| pushed.public_key.clone());
            assert_eq!(
                stored_serial.as_deref(),
                Some(serial_material.as_str()),
                "case {case:?}: the pushed serial-console key must be readable from state, got \
                 {stored_serial:?}"
            );
        } else {
            // Both refusals carry the message AWS words that error with, and neither stored a
            // key for the id its owner holds no instance under.
            assert_eq!(
                message(&body),
                NOT_FOUND_MESSAGE,
                "case {case:?}: SendSSHPublicKey must answer the message AWS words that error \
                 with, got {body}"
            );
            assert_eq!(
                message(&serial_body),
                NOT_FOUND_MESSAGE,
                "case {case:?}: SendSerialConsoleSSHPublicKey must answer the message AWS \
                 words that error with, got {serial_body}"
            );
            let guard = store.lock();
            assert_eq!(
                (guard.pushed_keys.len(), guard.serial_console_keys.len()),
                (0, 0),
                "case {case:?}: a refused push must store no key"
            );
        }
    }
}
