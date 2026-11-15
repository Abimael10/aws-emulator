// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `DeregisterInstance` does when the instance it names is
//! not registered with the service.
//!
//! The rule under test: a deregistration names the instance that must END UP not
//! registered with the service, so repeating it for an instance that service already
//! removed asks for a state the service is already in — it is served, files its own
//! `DEREGISTER_INSTANCE` operation and changes nothing — while an instance the service
//! never held has nothing to deregister and is refused with `InstanceNotFound`.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, i.e. the same path the
//! emulator serves in production (`awsJson1_1`: `POST /` with `X-Amz-Target`), reusing
//! the harness in `cloud_map_core`.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_map_core::{call, dns_namespace, emulator, namespace, ok, service};

/// Register `instance_id` with `service_id`.
async fn register(router: &MultiServiceRouter, service_id: &str, instance_id: &str) {
    ok(
        router,
        "RegisterInstance",
        json!({
            "ServiceId": service_id,
            "InstanceId": instance_id,
            "Attributes": { "AWS_INSTANCE_IPV4": "10.0.0.1" },
        }),
    )
    .await;
}

/// Deregister `instance_id` from the service named by `reference`, returning the
/// `OperationId` the call answers with.
async fn deregister(router: &MultiServiceRouter, reference: &str, instance_id: &str) -> String {
    let answered = ok(
        router,
        "DeregisterInstance",
        json!({ "ServiceId": reference, "InstanceId": instance_id }),
    )
    .await;
    answered["OperationId"]
        .as_str()
        .unwrap_or_else(|| panic!("DeregisterInstance returns an OperationId, got {answered}"))
        .to_string()
}

/// The HTTP status, wire error code and message a refused deregistration answers with.
async fn refusal(
    router: &MultiServiceRouter,
    reference: &str,
    instance_id: &str,
) -> (u16, String, String) {
    let (status, json) = call(
        router,
        "DeregisterInstance",
        json!({ "ServiceId": reference, "InstanceId": instance_id }),
    )
    .await;
    let code = json["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    // The awsJson error envelope carries the message as `message` or `Message`.
    let message = json["message"]
        .as_str()
        .or_else(|| json["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    assert_ne!(status, 200, "the deregistration must fail, got {json}");
    (status, code, message)
}

/// The operation record, read again until it reaches `SUCCESS` or a generous deadline
/// passes.
///
/// The service stores the instant the operation is due to succeed and materialises the
/// transition on read rather than sleeping for it, so the test waits for the CONDITION:
/// the status ends the loop, and the deadline only keeps a test that will never see it
/// from hanging.
async fn poll_until_succeeded(router: &MultiServiceRouter, operation_id: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let answered = ok(
            router,
            "GetOperation",
            json!({ "OperationId": operation_id }),
        )
        .await;
        if answered["Operation"]["Status"] == json!("SUCCESS")
            || std::time::Instant::now() >= deadline
        {
            return answered;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The ARN `GetService` reports for a stored service.
async fn service_arn(router: &MultiServiceRouter, service_id: &str) -> String {
    ok(router, "GetService", json!({ "Id": service_id })).await["Service"]["Arn"]
        .as_str()
        .expect("GetService reports the ARN it minted")
        .to_string()
}

/// `InstanceCount` and `InstancesRevision` as the service reports them, which is what a
/// repeat deregistration must leave alone.
async fn instance_set(
    router: &MultiServiceRouter,
    service_id: &str,
    namespace_name: &str,
    service_name: &str,
) -> (Value, Value) {
    let count = ok(router, "GetService", json!({ "Id": service_id })).await["Service"]
        ["InstanceCount"]
        .clone();
    let revision = ok(
        router,
        "DiscoverInstances",
        json!({ "NamespaceName": namespace_name, "ServiceName": service_name }),
    )
    .await["InstancesRevision"]
        .clone();
    (count, revision)
}

/// Repeating a deregistration is served, however many times it is repeated, whichever
/// way the service is named and whatever form the instance id takes: each repeat files
/// its own `DEREGISTER_INSTANCE` operation that reaches `SUCCESS`, and none of them
/// changes the instance set.
///
/// The ids span the forms `ResourceId` admits (the model puts no pattern on it, only
/// `length 0..64`), so the rule is not asserted on one shape of id.
#[tokio::test]
async fn repeating_a_deregistration_is_served_and_changes_nothing() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "repeat-ns").await;

    // (case, the instance id, how many times the deregistration is REPEATED after the
    // first one, whether the service is named by its ARN rather than its id)
    let cases: Vec<(&str, String, usize, bool)> = vec![
        ("a short id, one repeat", "i-1".to_string(), 1, false),
        ("a short id, several repeats", "i-2".to_string(), 5, false),
        (
            "an id carrying the punctuation the registry allows",
            "host.example_1/9:0@x-y".to_string(),
            2,
            false,
        ),
        (
            "an id at the modelled maximum length",
            "i".repeat(64),
            1,
            false,
        ),
        (
            "the service named by its ARN",
            "i-by-arn".to_string(),
            3,
            true,
        ),
    ];

    for (index, (case, instance_id, repeats, by_arn)) in cases.iter().enumerate() {
        let service_name = format!("repeat-svc-{index}");
        let service_id = service(&router, &namespace_id, &service_name).await;
        let reference = if *by_arn {
            service_arn(&router, &service_id).await
        } else {
            service_id.clone()
        };
        register(&router, &service_id, instance_id).await;

        let first = deregister(&router, &reference, instance_id).await;
        let settled = instance_set(&router, &service_id, "repeat-ns", &service_name).await;
        assert_eq!(
            settled.0,
            json!(0),
            "case {case:?}: the first deregistration removed the instance, got {:?}",
            settled.0
        );

        let mut filed = vec![first];
        for repeat in 1..=*repeats {
            let operation_id = deregister(&router, &reference, instance_id).await;
            assert!(
                !filed.contains(&operation_id),
                "case {case:?}: repeat {repeat} files its own operation, got {operation_id}"
            );
            filed.push(operation_id);

            let after = instance_set(&router, &service_id, "repeat-ns", &service_name).await;
            assert_eq!(
                after, settled,
                "case {case:?}: repeat {repeat} must change nothing, got {after:?}"
            );
        }

        for operation_id in &filed {
            let answered = poll_until_succeeded(&router, operation_id).await;
            let operation = &answered["Operation"];
            assert_eq!(
                operation["Status"],
                json!("SUCCESS"),
                "case {case:?}: operation {operation_id} must reach SUCCESS, got {answered}"
            );
            assert_eq!(
                operation["Type"],
                json!("DEREGISTER_INSTANCE"),
                "case {case:?}: operation {operation_id} is a DEREGISTER_INSTANCE, got {answered}"
            );
            assert_eq!(
                operation["Targets"]["INSTANCE"],
                json!(instance_id),
                "case {case:?}: the operation targets the instance it was asked about, \
                 got {answered}"
            );
            assert_eq!(
                operation["Targets"]["SERVICE"],
                json!(service_id),
                "case {case:?}: the operation targets the service it was asked of, got {answered}"
            );
        }
    }
}

/// The rule holds in a DNS namespace too, and across a re-registration: an instance
/// registered again is held again, and deregistering it again — then repeating that — is
/// served exactly as the first cycle was.
#[tokio::test]
async fn a_re_registered_instance_can_be_deregistered_again() {
    let router = emulator();
    let namespace_id = dns_namespace(&router, "cycle-ns").await;
    let created = ok(
        &router,
        "CreateService",
        json!({
            "Name": "cycle-svc",
            "NamespaceId": namespace_id,
            "DnsConfig": { "DnsRecords": [{ "Type": "A", "TTL": 60 }] },
        }),
    )
    .await;
    let service_id = created["Service"]["Id"]
        .as_str()
        .expect("CreateService returns the service it stored")
        .to_string();

    for cycle in 1..=3 {
        register(&router, &service_id, "i-cycled").await;
        let read = ok(
            &router,
            "GetInstance",
            json!({ "ServiceId": service_id, "InstanceId": "i-cycled" }),
        )
        .await;
        assert_eq!(
            read["Instance"]["Id"],
            json!("i-cycled"),
            "cycle {cycle}: the re-registration is stored, got {read}"
        );

        for attempt in 1..=2 {
            let operation_id = deregister(&router, &service_id, "i-cycled").await;
            let answered = poll_until_succeeded(&router, &operation_id).await;
            assert_eq!(
                answered["Operation"]["Status"],
                json!("SUCCESS"),
                "cycle {cycle} attempt {attempt}: the operation must reach SUCCESS, got {answered}"
            );
        }
        let count = ok(&router, "GetService", json!({ "Id": service_id })).await["Service"]
            ["InstanceCount"]
            .clone();
        assert_eq!(
            count,
            json!(0),
            "cycle {cycle}: the service holds no instance again, got {count}"
        );
    }
}

/// An instance the service never held has nothing to deregister: the call is refused
/// with `InstanceNotFound` naming the instance it was asked about and the service it was
/// scoped to, whichever way that service was named.
///
/// The rows span an id nothing ever registered, one registered with ANOTHER service, and
/// one another service deregistered — so the memory a repeat deregistration relies on is
/// shown to be per service rather than per emulator.
#[tokio::test]
async fn an_instance_the_service_never_held_is_refused() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "never-ns").await;
    let service_id = service(&router, &namespace_id, "never-svc").await;
    let arn = service_arn(&router, &service_id).await;

    // A second service that DOES hold, and then removes, ids the service under test
    // never saw.
    let other_id = service(&router, &namespace_id, "other-svc").await;
    register(&router, &other_id, "i-elsewhere").await;
    register(&router, &other_id, "i-removed-elsewhere").await;
    deregister(&router, &other_id, "i-removed-elsewhere").await;

    // The service under test holds one instance of its own, so the refusals below are
    // about the instance named rather than about an empty service.
    register(&router, &service_id, "i-held").await;

    let cases: Vec<(&str, &str)> = vec![
        ("an id nothing ever registered", "i-never"),
        ("an id registered with another service", "i-elsewhere"),
        (
            "an id another service deregistered",
            "i-removed-elsewhere",
        ),
    ];

    for (case, instance_id) in cases {
        for (reference_case, reference) in
            [("by id", service_id.as_str()), ("by ARN", arn.as_str())]
        {
            let (status, code, message) = refusal(&router, reference, instance_id).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InstanceNotFound"),
                "case {case:?} named {reference_case}: expected 400 InstanceNotFound, \
                 got {status} {code}"
            );
            assert_eq!(
                message,
                format!("instanceId {instance_id} doesn't exists for service {arn}"),
                "case {case:?} named {reference_case}: unexpected message {message:?}"
            );
        }
    }

    // The refused calls removed nothing: the instance the service does hold is still
    // there, and deregistering IT is served.
    let read = ok(
        &router,
        "GetInstance",
        json!({ "ServiceId": service_id, "InstanceId": "i-held" }),
    )
    .await;
    assert_eq!(
        read["Instance"]["Id"],
        json!("i-held"),
        "the refusals must leave the held instance alone, got {read}"
    );
    deregister(&router, &service_id, "i-held").await;
}
