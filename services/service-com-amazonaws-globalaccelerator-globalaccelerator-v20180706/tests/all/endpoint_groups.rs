// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the endpoint-group slice, driven over the wire.
//!
//! Every call goes through the real router, using the harness in
//! [`super::accelerator_lifecycle`] (`POST /` with
//! `X-Amz-Target: GlobalAccelerator_V20180706.<Operation>`, `awsJson1_1` body), so these
//! exercise routing, deserialization, constraint validation, the handler and the response
//! envelope — not just the handler function.
//!
//! The rules under test are stated once each, as a table or an invariant over a SET of
//! inputs; a recorded request is one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::accelerator_lifecycle::{arn_of, call, create_accelerator, emulator, error_code, ok};
// The envelope's message, in either spelling, already has one reader in this crate.
use super::input_refusals::error_message;

/// One accelerator plus one listener, the parents every endpoint group needs.
/// Returns the listener ARN.
async fn fixture(router: &MultiServiceRouter, suffix: &str) -> String {
    let accelerator = create_accelerator(router, &format!("eg-{suffix}"), &format!("acc-{suffix}")).await;
    listener(router, &arn_of(&accelerator), 80, 80, suffix).await
}

/// One listener on `accelerator_arn`, over the inclusive port range given.
async fn listener(
    router: &MultiServiceRouter,
    accelerator_arn: &str,
    from_port: i32,
    to_port: i32,
    token: &str,
) -> String {
    ok(
        router,
        "CreateListener",
        json!({
            "AcceleratorArn": accelerator_arn,
            "PortRanges": [{ "FromPort": from_port, "ToPort": to_port }],
            "Protocol": "TCP",
            "IdempotencyToken": format!("lsn-{token}"),
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `CreateEndpointGroup` with the members a row names, plus the required ones.
async fn create_group(
    router: &MultiServiceRouter,
    listener_arn: &str,
    region: &str,
    token: &str,
    extra: Value,
) -> (u16, Value) {
    let mut body = json!({
        "ListenerArn": listener_arn,
        "EndpointGroupRegion": region,
        "IdempotencyToken": token,
    });
    if let (Some(target), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    call(router, "CreateEndpointGroup", body).await
}

/// `UpdateEndpointGroup` with the members a row names, over the group `arn`.
async fn update_group(router: &MultiServiceRouter, arn: &str, extra: Value) -> (u16, Value) {
    let mut body = json!({ "EndpointGroupArn": arn });
    if let (Some(target), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    call(router, "UpdateEndpointGroup", body).await
}

fn group_arn(group: &Value) -> String {
    group["EndpointGroupArn"]
        .as_str()
        .unwrap_or_else(|| panic!("EndpointGroupArn missing from {group}"))
        .to_string()
}

/// The ARNs a `ListEndpointGroups` page reports, in the order it reported them.
fn listed_arns(page: &Value) -> Vec<String> {
    page["EndpointGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(group_arn)
        .collect()
}

/// A created endpoint group, as `DescribeEndpointGroup` reports it.
async fn describe_group(router: &MultiServiceRouter, arn: &str) -> Value {
    ok(
        router,
        "DescribeEndpointGroup",
        json!({ "EndpointGroupArn": arn }),
    )
    .await["EndpointGroup"]
        .clone()
}

#[tokio::test]
async fn an_endpoint_group_round_trips_through_the_store() {
    let router = emulator();
    let listener_arn = fixture(&router, "roundtrip").await;

    // Rows differ along what the rule generalises over: which optional members the
    // caller names, so each row pins both the echoed values and the documented
    // defaults for the members it leaves out. The expectations are the member docs of
    // `CreateEndpointGroup`/`EndpointGroup`: TrafficDialPercentage 100, ThresholdCount 3,
    // HealthCheckIntervalSeconds 30, HealthCheckProtocol TCP, HealthCheckPort the
    // listener's first port, HealthCheckPath "/" for an HTTP/S check.
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "all-defaults",
            json!({}),
            json!({
                "TrafficDialPercentage": 100.0,
                "HealthCheckPort": 80,
                "HealthCheckProtocol": "TCP",
                "HealthCheckIntervalSeconds": 30,
                "ThresholdCount": 3,
                "EndpointDescriptions": [],
                "PortOverrides": [],
            }),
        ),
        (
            "http-health-check",
            json!({ "HealthCheckProtocol": "HTTP", "HealthCheckIntervalSeconds": 10 }),
            json!({
                "TrafficDialPercentage": 100.0,
                "HealthCheckPort": 80,
                "HealthCheckProtocol": "HTTP",
                "HealthCheckPath": "/",
                "HealthCheckIntervalSeconds": 10,
                "ThresholdCount": 3,
            }),
        ),
        (
            "every-member-named",
            json!({
                "HealthCheckProtocol": "HTTPS",
                "HealthCheckPath": "/healthz",
                "HealthCheckPort": 8443,
                "HealthCheckIntervalSeconds": 30,
                "ThresholdCount": 10,
                "TrafficDialPercentage": 42.5,
                "PortOverrides": [
                    { "ListenerPort": 80, "EndpointPort": 1080 },
                    { "ListenerPort": 443, "EndpointPort": 1443 },
                ],
                "EndpointConfigurations": [
                    // Client IP address preservation is not optional for an EC2 instance
                    // endpoint, so the instance here names it `true`, while an endpoint of
                    // another kind takes either value — the rule
                    // [`an_update_refuses_client_ip_preservation_false_for_an_ec2_instance_endpoint`]
                    // states, over every identifier form.
                    { "EndpointId": "i-0123456789abcdef0", "Weight": 7,
                      "ClientIPPreservationEnabled": true },
                    { "EndpointId": ALB_ENDPOINT, "ClientIPPreservationEnabled": false },
                    { "EndpointId": "198.51.100.7" },
                ],
            }),
            json!({
                "TrafficDialPercentage": 42.5,
                "HealthCheckPort": 8443,
                "HealthCheckProtocol": "HTTPS",
                "HealthCheckPath": "/healthz",
                "HealthCheckIntervalSeconds": 30,
                "ThresholdCount": 10,
                "PortOverrides": [
                    { "ListenerPort": 80, "EndpointPort": 1080 },
                    { "ListenerPort": 443, "EndpointPort": 1443 },
                ],
                "EndpointDescriptions": [
                    // The weight a caller names is kept; an endpoint without one takes
                    // the documented default of 128 (Endpoint weights, Global
                    // Accelerator Developer Guide). The emulator runs no health checks,
                    // so every endpoint is reported INITIAL.
                    { "EndpointId": "i-0123456789abcdef0", "Weight": 7,
                      "HealthState": "INITIAL", "ClientIPPreservationEnabled": true },
                    { "EndpointId": ALB_ENDPOINT, "Weight": 128,
                      "HealthState": "INITIAL", "ClientIPPreservationEnabled": false },
                    { "EndpointId": "198.51.100.7", "Weight": 128,
                      "HealthState": "INITIAL" },
                ],
            }),
        ),
    ];

    for (case, extra, expected) in rows {
        // Each row gets its own Region: one listener holds one group per Region.
        let region = format!("us-west-{}", case.len());
        let (status, body) = create_group(&router, &listener_arn, &region, case, extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let created = body["EndpointGroup"].clone();
        let arn = group_arn(&created);
        assert_eq!(
            arn,
            format!("{listener_arn}/endpoint-group/{}", arn.rsplit('/').next().unwrap_or_default()),
            "case {case:?}: an endpoint group ARN hangs off its listener, got {arn:?}"
        );
        assert_eq!(
            created["EndpointGroupRegion"], region.as_str(),
            "case {case:?}: expected Region {region:?}, got {created}"
        );
        for (member, want) in expected.as_object().expect("an object of expectations") {
            assert_eq!(
                created.get(member).unwrap_or(&Value::Null),
                want,
                "case {case:?}: expected {member} {want}, got {created}"
            );
        }

        // The read-back is the same record, not a second computation of the request.
        let read = describe_group(&router, &arn).await;
        assert_eq!(
            read, created,
            "case {case:?}: expected DescribeEndpointGroup to report the created group \
             {created}, got {read}"
        );
    }
}

#[tokio::test]
async fn the_health_check_interval_is_ten_or_thirty_seconds() {
    let router = emulator();
    let listener_arn = fixture(&router, "interval").await;

    // `HealthCheckIntervalSeconds`: "The time—10 seconds or 30 seconds—between each
    // health check for an endpoint." The model's own @range is 10..30, so every value
    // strictly between the two ends reaches the handler and must be refused there, and
    // the values outside the range are refused at the boundary. Each row is tried in a
    // fresh Region, and a refused create must leave that Region free.
    let rows: [(i32, bool); 8] = [
        (10, true),
        (30, true),
        (11, false),
        (15, false),
        (20, false),
        (29, false),
        // Past each published bound of the modelled range.
        (9, false),
        (31, false),
    ];

    for (interval, accepted) in rows {
        let case = format!("interval-{interval}");
        let region = format!("eu-west-{interval}");
        let (status, body) = create_group(
            &router,
            &listener_arn,
            &region,
            &case,
            json!({ "HealthCheckIntervalSeconds": interval }),
        )
        .await;

        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: {interval} is a supported interval, got {status} with {body}"
            );
            assert_eq!(
                body["EndpointGroup"]["HealthCheckIntervalSeconds"], interval,
                "case {case:?}: expected the stored interval {interval}, got {body}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: {interval} is not a supported interval, got {status} with {body}"
        );
        // Inside the modelled range the refusal is the operation's own modelled error;
        // outside it, the boundary validator refuses the request before the handler.
        if (10..=30).contains(&interval) {
            assert_eq!(
                error_code(&body),
                "InvalidArgumentException",
                "case {case:?}: expected the modelled InvalidArgumentException, got {body}"
            );
        }
        // The refused create persisted nothing: the Region it named is still free.
        let (retry_status, retry) = create_group(
            &router,
            &listener_arn,
            &region,
            &format!("{case}-retry"),
            json!({ "HealthCheckIntervalSeconds": 10 }),
        )
        .await;
        assert_eq!(
            retry_status, 200,
            "case {case:?}: a refused create must write nothing, got {retry_status} with {retry}"
        );
    }
}

#[tokio::test]
async fn a_listener_holds_one_endpoint_group_per_region() {
    let router = emulator();
    let accelerator = create_accelerator(&router, "eg-regions", "acc-regions").await;
    let accelerator_arn = arn_of(&accelerator);
    let first = listener(&router, &accelerator_arn, 80, 80, "regions-1").await;
    let second = listener(&router, &accelerator_arn, 443, 443, "regions-2").await;

    // Rows differ in the Region, the datum the rule is keyed on: each is free once per
    // listener, and the second create for the same Region is refused.
    let regions = [
        "us-west-2",
        "eu-central-1",
        "ap-southeast-1",
        "us-gov-west-1",
    ];

    for region in regions {
        let (status, body) =
            create_group(&router, &first, region, &format!("tok-{region}"), json!({})).await;
        assert_eq!(
            status, 200,
            "case {region:?}: the first group in a Region is created, got {status} with {body}"
        );
        let arn = group_arn(&body["EndpointGroup"]);

        // A second create for the same Region, with a DIFFERENT token, is refused.
        let (dup_status, dup) = create_group(
            &router,
            &first,
            region,
            &format!("tok-{region}-again"),
            json!({}),
        )
        .await;
        assert_eq!(
            dup_status, 400,
            "case {region:?}: a Region already covered is refused, got {dup_status} with {dup}"
        );
        assert_eq!(
            error_code(&dup),
            "EndpointGroupAlreadyExistsException",
            "case {region:?}: expected EndpointGroupAlreadyExistsException, got {dup}"
        );
        // The sentence this service states the conflict with is FIXED: it names neither
        // the Region, nor the listener, nor the group that already covers it, so every
        // duplicate — whatever its Region — gets exactly this text.
        assert_eq!(
            error_message(&dup),
            "Endpoint group already exists.",
            "case {region:?}: expected the fixed already-exists sentence, got {dup}"
        );

        // The SAME token is the same request: it replays the group it minted.
        let (replay_status, replay) =
            create_group(&router, &first, region, &format!("tok-{region}"), json!({})).await;
        assert_eq!(
            replay_status, 200,
            "case {region:?}: a repeated token replays, got {replay_status} with {replay}"
        );
        assert_eq!(
            group_arn(&replay["EndpointGroup"]),
            arn,
            "case {region:?}: a repeated token must mint once, got {replay}"
        );

        // The rule is per LISTENER: the other listener may cover the same Region.
        let (sibling_status, sibling) = create_group(
            &router,
            &second,
            region,
            &format!("tok-{region}-sibling"),
            json!({}),
        )
        .await;
        assert_eq!(
            sibling_status, 200,
            "case {region:?}: another listener may cover the Region, got {sibling_status} \
             with {sibling}"
        );
        assert_ne!(
            group_arn(&sibling["EndpointGroup"]),
            arn,
            "case {region:?}: a sibling listener's group is its own resource, got {sibling}"
        );
    }
}

#[tokio::test]
async fn a_listener_with_an_endpoint_group_cannot_be_deleted() {
    let router = emulator();
    let listener_arn = fixture(&router, "dependents").await;

    // Rows: a listener with 1 and with 2 endpoint groups is refused the same way; with
    // none it deletes.
    let mut arns: Vec<String> = Vec::new();
    for region in ["us-east-1", "sa-east-1"] {
        let (status, body) =
            create_group(&router, &listener_arn, region, &format!("dep-{region}"), json!({})).await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected HTTP 200, got {status} with {body}"
        );
        arns.push(group_arn(&body["EndpointGroup"]));

        let (delete_status, refusal) = call(
            &router,
            "DeleteListener",
            json!({ "ListenerArn": listener_arn }),
        )
        .await;
        assert_eq!(
            delete_status, 400,
            "case {} group(s): a listener with endpoint groups is not deletable, got \
             {delete_status} with {refusal}",
            arns.len()
        );
        assert_eq!(
            error_code(&refusal),
            "AssociatedEndpointGroupFoundException",
            "case {} group(s): expected AssociatedEndpointGroupFoundException, got {refusal}",
            arns.len()
        );
        // A refusal about a named resource identifies it by the ARN the request carried,
        // which is the template's only variable part.
        assert_eq!(
            error_message(&refusal),
            format!(
                "Cannot delete listener {listener_arn} because an associated endpoint group \
                 was found."
            ),
            "case {} group(s): expected the wording this service states a blocked delete \
             with, got {refusal}",
            arns.len()
        );
    }

    // The refused deletes were no-ops: the listener and both groups are still there.
    ok(
        &router,
        "DescribeListener",
        json!({ "ListenerArn": listener_arn }),
    )
    .await;
    for arn in &arns {
        let read = describe_group(&router, arn).await;
        assert_eq!(
            group_arn(&read),
            *arn,
            "case survivor: expected endpoint group {arn} to survive a refused listener \
             delete, got {read}"
        );
    }

    // Removing every dependent resource makes the listener deletable, and a deleted
    // group is gone from the store.
    for arn in &arns {
        ok(
            &router,
            "DeleteEndpointGroup",
            json!({ "EndpointGroupArn": arn }),
        )
        .await;
        let (status, body) = call(
            &router,
            "DescribeEndpointGroup",
            json!({ "EndpointGroupArn": arn }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case deleted {arn}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "EndpointGroupNotFoundException",
            "case deleted {arn}: expected EndpointGroupNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("EndpointGroup {arn} does not exist."),
            "case deleted {arn}: expected the refusal to name the endpoint group, got {body}"
        );
    }
    ok(
        &router,
        "DeleteListener",
        json!({ "ListenerArn": listener_arn }),
    )
    .await;
}

#[tokio::test]
async fn an_endpoint_group_needs_a_listener_the_store_holds() {
    let router = emulator();
    let listener_arn = fixture(&router, "missing").await;

    // Rows: an ARN of the right shape that was never minted, and one whose listener was
    // deleted. Both name no listener, so both are the modelled not-found error.
    let deleted = listener(
        &router,
        &arn_of(&create_accelerator(&router, "eg-gone", "acc-gone").await),
        90,
        90,
        "gone",
    )
    .await;
    ok(&router, "DeleteListener", json!({ "ListenerArn": deleted })).await;

    for (case, arn) in [
        ("never-minted", format!("{listener_arn}-not-a-listener")),
        ("deleted", deleted.clone()),
    ] {
        let (status, body) = create_group(&router, &arn, "us-west-2", case, json!({})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ListenerNotFoundException",
            "case {case:?}: expected ListenerNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Listener {arn} not found."),
            "case {case:?}: expected the refusal to name the listener, got {body}"
        );
    }

    // The same rule for the readers of a group that does not exist.
    for operation in ["DescribeEndpointGroup", "DeleteEndpointGroup"] {
        let (status, body) = call(
            &router,
            operation,
            json!({ "EndpointGroupArn": format!("{listener_arn}/endpoint-group/deadbeef") }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {operation}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "EndpointGroupNotFoundException",
            "case {operation}: expected EndpointGroupNotFoundException, got {body}"
        );
    }
}

#[tokio::test]
async fn a_malformed_endpoint_or_port_override_is_refused() {
    let router = emulator();
    let listener_arn = fixture(&router, "refusals").await;

    // Rows: each names ONE way a request describes no endpoint or no mapping, with the
    // modelled error it earns. `AttachmentArn` names a cross-account attachment, and the
    // store holds none (the attachment operations are not implemented), so no attachment
    // grants permission to add that endpoint.
    let rows: [(&str, Value, u16, &str); 6] = [
        (
            "endpoint-without-id",
            json!({ "EndpointConfigurations": [{ "Weight": 10 }] }),
            400,
            "InvalidArgumentException",
        ),
        (
            "endpoint-named-twice",
            json!({ "EndpointConfigurations": [
                { "EndpointId": "i-00000000000000001" },
                { "EndpointId": "i-00000000000000001", "Weight": 3 },
            ] }),
            400,
            "InvalidArgumentException",
        ),
        (
            "override-without-endpoint-port",
            json!({ "PortOverrides": [{ "ListenerPort": 80 }] }),
            400,
            "InvalidArgumentException",
        ),
        (
            "override-listener-port-twice",
            json!({ "PortOverrides": [
                { "ListenerPort": 80, "EndpointPort": 1080 },
                { "ListenerPort": 80, "EndpointPort": 1081 },
            ] }),
            400,
            "InvalidArgumentException",
        ),
        (
            "empty-region",
            json!({ "EndpointGroupRegion": "" }),
            400,
            "InvalidArgumentException",
        ),
        (
            "unknown-attachment",
            json!({ "EndpointConfigurations": [
                { "EndpointId": "i-00000000000000002",
                  "AttachmentArn": "arn:aws:globalaccelerator::000000000000:attachment/abc" },
            ] }),
            403,
            "AccessDeniedException",
        ),
    ];

    for (case, extra, expected_status, expected_code) in rows {
        let (status, body) = create_group(&router, &listener_arn, "ca-central-1", case, extra).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
    }

    // A list longer than the model's own `@length` (0..10) on `EndpointConfigurations`
    // and `PortOverrides` is refused too, so neither the work a create does nor what it
    // stores is proportional to a length the caller chose.
    for (case, extra) in [
        (
            "eleven-endpoints",
            json!({ "EndpointConfigurations": (0..11)
                .map(|index| json!({ "EndpointId": format!("i-{index:017}") }))
                .collect::<Vec<Value>>() }),
        ),
        (
            "eleven-port-overrides",
            json!({ "PortOverrides": (0..11)
                .map(|index| json!({ "ListenerPort": 80 + index, "EndpointPort": 1080 + index }))
                .collect::<Vec<Value>>() }),
        ),
    ] {
        let (status, body) = create_group(&router, &listener_arn, "ca-central-1", case, extra).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
    }

    // None of them wrote: the Region every row named is still free.
    let (status, body) =
        create_group(&router, &listener_arn, "ca-central-1", "after-refusals", json!({})).await;
    assert_eq!(
        status, 200,
        "case after-refusals: a refused create must write nothing, got {status} with {body}"
    );
}

#[tokio::test]
async fn a_create_past_the_endpoint_group_quota_is_refused_and_a_delete_frees_a_slot() {
    let router = emulator();
    let accelerator = create_accelerator(&router, "eg-quota", "acc-quota").await;
    let accelerator_arn = arn_of(&accelerator);
    // Two listeners, so the quota is tested as a PER-ACCELERATOR total rather than a
    // per-listener one: the groups below are split across both.
    let listeners = [
        listener(&router, &accelerator_arn, 80, 80, "quota-1").await,
        listener(&router, &accelerator_arn, 443, 443, "quota-2").await,
    ];

    // The quota as a boundary: every create up to it succeeds, the one past it is the
    // modelled LimitExceededException on EITHER listener, and deleting a group frees
    // exactly one slot.
    let quota = 10;
    let mut arns: Vec<String> = Vec::new();
    for index in 0..quota {
        let region = format!("quota-region-{index}");
        let parent = &listeners[index % listeners.len()];
        let (status, body) =
            create_group(&router, parent, &region, &format!("q-{index}"), json!({})).await;
        assert_eq!(
            status, 200,
            "case {region:?}: group {} of {quota} is within the quota, got {status} with {body}",
            index + 1
        );
        arns.push(group_arn(&body["EndpointGroup"]));
    }

    for parent in &listeners {
        let (status, body) =
            create_group(&router, parent, "quota-region-over", "q-over", json!({})).await;
        assert_eq!(
            status, 403,
            "case over-quota on {parent}: expected HTTP 403, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "LimitExceededException",
            "case over-quota on {parent}: expected LimitExceededException, got {body}"
        );
        assert!(
            error_message(&body).contains(&accelerator_arn),
            "case over-quota on {parent}: expected the refusal to name the accelerator, got {body}"
        );
    }

    ok(
        &router,
        "DeleteEndpointGroup",
        json!({ "EndpointGroupArn": arns[0] }),
    )
    .await;
    let (status, body) = create_group(
        &router,
        &listeners[1],
        "quota-region-over",
        "q-over-2",
        json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case freed-slot: a delete frees one slot for any listener of the accelerator, \
         got {status} with {body}"
    );
}

/// RULE: an update replaces every member the request names and changes nothing else —
/// the members it omits keep their stored values, and the group's own identity (its ARN
/// and its Region) is not configuration at all — and what it answers is what the store
/// now holds, so a `DescribeEndpointGroup` reports the same record.
#[tokio::test]
async fn an_update_replaces_the_members_it_names_and_leaves_the_others() {
    let router = emulator();
    let listener_arn = fixture(&router, "update").await;

    // The group every row starts from names every member, each at a value that is NOT the
    // documented default, so a member a row leaves out can be asserted to have kept it.
    let base = json!({
        "HealthCheckProtocol": "HTTP",
        "HealthCheckPath": "/health",
        "HealthCheckPort": 8080,
        "HealthCheckIntervalSeconds": 30,
        "ThresholdCount": 5,
        "TrafficDialPercentage": 70.0,
        "PortOverrides": [{ "ListenerPort": 80, "EndpointPort": 1080 }],
        "EndpointConfigurations": [{ "EndpointId": "i-00000000000000001", "Weight": 9 }],
    });

    // Rows differ along WHICH members the update names — one, several, all, none — and in
    // the kind of each: a float, an integer, an enumerated string, a list of structures.
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "interval-only",
            json!({ "HealthCheckIntervalSeconds": 10 }),
            json!({
                "HealthCheckIntervalSeconds": 10,
                "TrafficDialPercentage": 70.0,
                "ThresholdCount": 5,
                "HealthCheckProtocol": "HTTP",
                "HealthCheckPath": "/health",
                "HealthCheckPort": 8080,
                "PortOverrides": [{ "ListenerPort": 80, "EndpointPort": 1080 }],
            }),
        ),
        (
            "dial-only",
            json!({ "TrafficDialPercentage": 0.0 }),
            json!({
                "TrafficDialPercentage": 0.0,
                "HealthCheckIntervalSeconds": 30,
                "ThresholdCount": 5,
            }),
        ),
        (
            "endpoints-replaced",
            json!({ "EndpointConfigurations": [
                { "EndpointId": "198.51.100.9" },
                { "EndpointId": "i-00000000000000002", "Weight": 3 },
            ] }),
            json!({
                "EndpointDescriptions": [
                    { "EndpointId": "198.51.100.9", "Weight": 128, "HealthState": "INITIAL" },
                    { "EndpointId": "i-00000000000000002", "Weight": 3,
                      "HealthState": "INITIAL" },
                ],
                "HealthCheckIntervalSeconds": 30,
                "TrafficDialPercentage": 70.0,
            }),
        ),
        (
            "port-overrides-replaced-by-an-empty-list",
            json!({ "PortOverrides": [] }),
            json!({ "PortOverrides": [], "HealthCheckPort": 8080 }),
        ),
        (
            "every-member-named",
            json!({
                "HealthCheckProtocol": "TCP",
                "HealthCheckPort": 443,
                "HealthCheckIntervalSeconds": 10,
                "ThresholdCount": 1,
                "TrafficDialPercentage": 12.5,
                "PortOverrides": [{ "ListenerPort": 443, "EndpointPort": 1443 }],
                "EndpointConfigurations": [{ "EndpointId": "203.0.113.4" }],
            }),
            json!({
                "HealthCheckProtocol": "TCP",
                "HealthCheckPort": 443,
                "HealthCheckIntervalSeconds": 10,
                "ThresholdCount": 1,
                "TrafficDialPercentage": 12.5,
                "PortOverrides": [{ "ListenerPort": 443, "EndpointPort": 1443 }],
                "EndpointDescriptions": [
                    { "EndpointId": "203.0.113.4", "Weight": 128, "HealthState": "INITIAL" },
                ],
                // A path the caller set is the group's own, so it stays with it.
                "HealthCheckPath": "/health",
            }),
        ),
        (
            "no-member-named",
            json!({}),
            base.clone(),
        ),
    ];

    for (index, (case, update, expected)) in rows.into_iter().enumerate() {
        // Each row gets its own group: one listener holds one group per Region.
        let region = format!("us-east-{index}");
        let (status, created) =
            create_group(&router, &listener_arn, &region, case, base.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the fixture group, got {status} with {created}"
        );
        let arn = group_arn(&created["EndpointGroup"]);

        let (status, body) = update_group(&router, &arn, update).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let updated = body["EndpointGroup"].clone();
        assert_eq!(
            updated["EndpointGroupArn"],
            json!(arn),
            "case {case:?}: an update answers the group it updated, got {updated}"
        );
        assert_eq!(
            updated["EndpointGroupRegion"],
            json!(region),
            "case {case:?}: the Region identifies the group and is not updated, got {updated}"
        );
        for (member, want) in expected.as_object().expect("an object of expectations") {
            // `EndpointConfigurations` is the input member; the group reports its
            // endpoints as `EndpointDescriptions`, which the rows name directly.
            if member == "EndpointConfigurations" {
                continue;
            }
            assert_eq!(
                updated.get(member).unwrap_or(&Value::Null),
                want,
                "case {case:?}: expected {member} {want}, got {updated}"
            );
        }

        // The update persisted: the read-back is that record, not a second computation.
        let read = describe_group(&router, &arn).await;
        assert_eq!(
            read, updated,
            "case {case:?}: expected DescribeEndpointGroup to report the updated group \
             {updated}, got {read}"
        );
    }
}

/// RULE: a health-check path belongs to an HTTP/S check — "If the protocol is HTTP/S,
/// then this specifies the path that is the destination for health check targets. The
/// default value is slash (/)" — so the path a group ends up with follows the protocol it
/// now has, whether that protocol arrived with the create or with an update.
#[tokio::test]
async fn a_health_check_path_follows_the_protocol_the_group_now_has() {
    let router = emulator();
    let listener_arn = fixture(&router, "paths").await;

    // (case, what the create named, what the update named, the path afterwards)
    let rows: Vec<(&str, Value, Value, Value)> = vec![
        (
            "tcp-group-switched-to-http",
            json!({ "HealthCheckProtocol": "TCP" }),
            json!({ "HealthCheckProtocol": "HTTP" }),
            json!("/"),
        ),
        (
            "tcp-group-switched-to-https-with-a-path",
            json!({ "HealthCheckProtocol": "TCP" }),
            json!({ "HealthCheckProtocol": "HTTPS", "HealthCheckPath": "/ping" }),
            json!("/ping"),
        ),
        (
            "tcp-group-left-on-tcp",
            json!({ "HealthCheckProtocol": "TCP" }),
            json!({ "HealthCheckIntervalSeconds": 10 }),
            Value::Null,
        ),
        (
            "http-group-given-another-path",
            json!({ "HealthCheckProtocol": "HTTP", "HealthCheckPath": "/first" }),
            json!({ "HealthCheckPath": "/second" }),
            json!("/second"),
        ),
    ];

    for (index, (case, create, update, expected)) in rows.into_iter().enumerate() {
        let region = format!("me-south-{index}");
        let (status, created) = create_group(&router, &listener_arn, &region, case, create).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the fixture group, got {status} with {created}"
        );
        let arn = group_arn(&created["EndpointGroup"]);

        let (status, body) = update_group(&router, &arn, update).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let read = describe_group(&router, &arn).await;
        assert_eq!(
            read.get("HealthCheckPath").unwrap_or(&Value::Null),
            &expected,
            "case {case:?}: expected HealthCheckPath {expected}, got {read}"
        );
    }
}

/// RULE: only the two intervals the member documents are accepted — "The time—10 seconds
/// or 30 seconds—between each health check for an endpoint" — so every other value inside
/// the modelled `@range` (10..30) is refused with the sentence this service states for
/// that class, on the create leg and on the update leg alike, and a refused update leaves
/// the stored group exactly as it was.
#[tokio::test]
async fn an_unsupported_health_check_interval_is_refused_in_the_services_words() {
    let router = emulator();
    let listener_arn = fixture(&router, "interval-words").await;

    // Rows: every unsupported value strictly between the two published ones, so the rule
    // is tested over the class rather than on the one value a run recorded.
    for interval in [11, 15, 20, 29] {
        let case = format!("interval-{interval}");
        let region = format!("ap-northeast-{interval}");
        let (status, created) = create_group(
            &router,
            &listener_arn,
            &region,
            &case,
            json!({ "HealthCheckIntervalSeconds": 30, "TrafficDialPercentage": 90.0 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the fixture group, got {status} with {created}"
        );
        let arn = group_arn(&created["EndpointGroup"]);
        let expected = format!(
            "Invalid HealthCheck Interval Seconds: {interval} expected values are: 10 or 30"
        );

        // The update leg: refused, and the other member the same request carried is not
        // applied either.
        let (status, refusal) = update_group(
            &router,
            &arn,
            json!({ "HealthCheckIntervalSeconds": interval, "TrafficDialPercentage": 50.0 }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refusal).as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?}: expected the modelled 400 InvalidArgumentException, got \
             {status} with {refusal}"
        );
        assert_eq!(
            error_message(&refusal),
            expected,
            "case {case:?}: expected the wording this service states the class with, \
             got {refusal}"
        );
        let read = describe_group(&router, &arn).await;
        assert_eq!(
            (
                read["HealthCheckIntervalSeconds"].clone(),
                read["TrafficDialPercentage"].clone()
            ),
            (json!(30), json!(90.0)),
            "case {case:?}: a refused update must change nothing, got {read}"
        );

        // The create leg states the same sentence for the same value.
        let (status, refusal) = create_group(
            &router,
            &listener_arn,
            &format!("{region}-fresh"),
            &format!("{case}-create"),
            json!({ "HealthCheckIntervalSeconds": interval }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refusal).as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?}: expected the create leg to refuse it too, got {status} with \
             {refusal}"
        );
        assert_eq!(
            error_message(&refusal),
            expected,
            "case {case:?}: expected both legs to state one sentence, got {refusal}"
        );

        // Each supported value is accepted on the update leg and round-trips.
        for supported in [10, 30] {
            let (status, body) =
                update_group(&router, &arn, json!({ "HealthCheckIntervalSeconds": supported }))
                    .await;
            assert_eq!(
                status, 200,
                "case {case:?}/{supported}: a supported interval is accepted, got {status} \
                 with {body}"
            );
            let read = describe_group(&router, &arn).await;
            assert_eq!(
                read["HealthCheckIntervalSeconds"],
                json!(supported),
                "case {case:?}/{supported}: expected the stored interval {supported}, got {read}"
            );
        }
    }
}

/// RULE: a value outside a modelled bound is refused with the standard constraint report,
/// whose clause names the member path this service reports for that member and renders
/// the rejected value as the member's modelled type — a float with its fractional part.
/// `TrafficDialPercentage` is `@range` 0..100 and type `float`
/// (`smithy_query(query="shape", shape="TrafficDialPercentage")`), carried by both the
/// create and the update leg; and neither refusal writes anything.
#[tokio::test]
async fn a_rejected_traffic_dial_percentage_is_reported_as_this_service_reports_it() {
    let router = emulator();
    let listener_arn = fixture(&router, "dial-clause").await;
    let (status, created) = create_group(
        &router,
        &listener_arn,
        "eu-south-1",
        "dial-clause-base",
        json!({ "TrafficDialPercentage": 50.0 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case base: expected the fixture group, got {status} with {created}"
    );
    let arn = group_arn(&created["EndpointGroup"]);
    // The Region every refused create below names, left free by each of them.
    let refused_region = "eu-north-1";

    // (case, the value sent, how the clause renders it, the requirement it broke)
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "far-past-the-maximum",
            json!(150.0),
            "150.0",
            "Member must have value less than or equal to 100",
        ),
        (
            "just-past-the-maximum",
            json!(100.5),
            "100.5",
            "Member must have value less than or equal to 100",
        ),
        (
            "a-whole-number-past-the-maximum",
            json!(101),
            "101.0",
            "Member must have value less than or equal to 100",
        ),
        (
            "below-the-minimum",
            json!(-1.0),
            "-1.0",
            "Member must have value greater than or equal to 0",
        ),
        (
            "just-below-the-minimum",
            json!(-0.5),
            "-0.5",
            "Member must have value greater than or equal to 0",
        ),
    ];

    for (case, value, rendered, requirement) in rows {
        let expected = format!(
            "1 validation error detected: Value '{rendered}' at 'trafficRetentionPercentage' \
             failed to satisfy constraint: {requirement}"
        );
        let legs = [
            (
                "create",
                create_group(
                    &router,
                    &listener_arn,
                    refused_region,
                    case,
                    json!({ "TrafficDialPercentage": value }),
                )
                .await,
            ),
            (
                "update",
                update_group(&router, &arn, json!({ "TrafficDialPercentage": value })).await,
            ),
        ];
        for (leg, (status, body)) in legs {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "InvalidArgumentException"),
                "case {case:?} on the {leg} leg: expected the modelled 400 \
                 InvalidArgumentException, got {status} with {body}"
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?} on the {leg} leg: expected the constraint report this \
                 service states, got {body}"
            );
        }

        // Neither refusal wrote: the group still holds the percentage it was created
        // with.
        let read = describe_group(&router, &arn).await;
        assert_eq!(
            read["TrafficDialPercentage"],
            json!(50.0),
            "case {case:?}: a refused update must change nothing, got {read}"
        );
    }

    // Nor did any refused create: the Region they all named is still free.
    let (status, body) = create_group(
        &router,
        &listener_arn,
        refused_region,
        "after-the-refusals",
        json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case after-the-refusals: a refused create must write nothing, got {status} \
         with {body}"
    );
}

/// RULE: an update of an endpoint group the store does not hold is the operation's own
/// modelled not-found error, naming the ARN the request carried, and nothing is created
/// for it.
#[tokio::test]
async fn an_update_of_a_group_the_store_does_not_hold_is_the_modelled_not_found() {
    let router = emulator();
    let listener_arn = fixture(&router, "update-missing").await;

    // Rows: an ARN of the right shape that was never minted, and one whose group was
    // deleted — both name no endpoint group.
    let (status, created) = create_group(
        &router,
        &listener_arn,
        "af-south-1",
        "update-missing-base",
        json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case base: expected the fixture group, got {status} with {created}"
    );
    let deleted = group_arn(&created["EndpointGroup"]);
    ok(
        &router,
        "DeleteEndpointGroup",
        json!({ "EndpointGroupArn": deleted }),
    )
    .await;

    for (case, arn) in [
        (
            "never-minted",
            format!("{listener_arn}/endpoint-group/00000000cafe"),
        ),
        ("deleted", deleted.clone()),
    ] {
        let (status, body) =
            update_group(&router, &arn, json!({ "HealthCheckIntervalSeconds": 10 })).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "EndpointGroupNotFoundException"),
            "case {case:?}: expected the modelled not-found, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("EndpointGroup {arn} does not exist."),
            "case {case:?}: expected the refusal to name the endpoint group, got {body}"
        );

        // The refused update created nothing under that ARN.
        let (status, read) = call(
            &router,
            "DescribeEndpointGroup",
            json!({ "EndpointGroupArn": arn }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "EndpointGroupNotFoundException"),
            "case {case:?}: a refused update must create nothing, got {status} with {read}"
        );
    }
}

/// RULE: a listener's endpoint groups are reported from the store — every group it holds,
/// in the order they were created, one page at a time — so a group a refused create never
/// made is not among them, a deleted one is gone, and another listener's groups are not
/// listed with them.
#[tokio::test]
async fn a_listeners_endpoint_groups_are_listed_from_the_store() {
    let router = emulator();
    let accelerator = create_accelerator(&router, "eg-list", "acc-list").await;
    let accelerator_arn = arn_of(&accelerator);
    let first = listener(&router, &accelerator_arn, 80, 80, "list-1").await;
    let second = listener(&router, &accelerator_arn, 443, 443, "list-2").await;

    // Nothing created yet: no group, and no page after this one.
    let page = ok(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": first }),
    )
    .await;
    assert_eq!(
        page["EndpointGroups"],
        json!([]),
        "case empty: a listener with no endpoint group lists none, got {page}"
    );
    assert_eq!(
        page["NextToken"],
        Value::Null,
        "case empty: a single page carries no continuation token, got {page}"
    );

    // A refused create made no group, so there is still nothing to list.
    let (status, refusal) = create_group(
        &router,
        &first,
        "us-west-2",
        "list-refused",
        json!({ "TrafficDialPercentage": 150.0 }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case refused: expected the refusal, got {status} with {refusal}"
    );
    let page = ok(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": first }),
    )
    .await;
    assert_eq!(
        page["EndpointGroups"],
        json!([]),
        "case refused: a refused create is not a group, got {page}"
    );

    // Three groups on the first listener, one on the second: each listener lists its own,
    // in creation order.
    let mut arns: Vec<String> = Vec::new();
    for region in ["us-west-2", "eu-west-1", "ap-south-1"] {
        let (status, body) =
            create_group(&router, &first, region, &format!("list-{region}"), json!({})).await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected HTTP 200, got {status} with {body}"
        );
        arns.push(group_arn(&body["EndpointGroup"]));
    }
    let (status, body) = create_group(&router, &second, "us-west-2", "list-sibling", json!({})).await;
    assert_eq!(
        status, 200,
        "case sibling: expected HTTP 200, got {status} with {body}"
    );
    let sibling = group_arn(&body["EndpointGroup"]);

    let page = ok(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": first }),
    )
    .await;
    assert_eq!(
        listed_arns(&page),
        arns,
        "case three: expected the groups in creation order, got {page}"
    );
    let page = ok(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": second }),
    )
    .await;
    assert_eq!(
        listed_arns(&page),
        vec![sibling.clone()],
        "case sibling: a listener lists only its own groups, got {page}"
    );

    // A page the caller bounded, resumed by the token it answered: the two pages together
    // are the collection, in order, and the last page carries no token.
    let page = ok(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": first, "MaxResults": 2 }),
    )
    .await;
    assert_eq!(
        listed_arns(&page),
        arns[..2].to_vec(),
        "case page-1: expected the first two groups, got {page}"
    );
    let token = page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case page-1: expected a continuation token, got {page}"))
        .to_string();
    let page = ok(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": first, "MaxResults": 2, "NextToken": token }),
    )
    .await;
    assert_eq!(
        listed_arns(&page),
        arns[2..].to_vec(),
        "case page-2: expected the rest of the collection, got {page}"
    );
    assert_eq!(
        page["NextToken"],
        Value::Null,
        "case page-2: the last page carries no continuation token, got {page}"
    );

    // A token this service never issued is the modelled refusal.
    let (status, body) = call(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": first, "NextToken": "not-a-token" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidNextTokenException"),
        "case bad-token: expected the modelled InvalidNextTokenException, got {status} \
         with {body}"
    );

    // A delete removes the group from the list, and the list of a listener the store does
    // not hold is the modelled not-found.
    ok(
        &router,
        "DeleteEndpointGroup",
        json!({ "EndpointGroupArn": arns[1] }),
    )
    .await;
    let page = ok(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": first }),
    )
    .await;
    assert_eq!(
        listed_arns(&page),
        vec![arns[0].clone(), arns[2].clone()],
        "case deleted: a deleted group is not listed, got {page}"
    );

    let missing = format!("{first}-not-a-listener");
    let (status, body) = call(
        &router,
        "ListEndpointGroups",
        json!({ "ListenerArn": missing }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ListenerNotFoundException"),
        "case missing-listener: expected the modelled not-found, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("Listener {missing} not found."),
        "case missing-listener: expected the refusal to name the listener, got {body}"
    );
}

// ---------------------------------------------------------------------------
// A group's endpoint MEMBERSHIP: `AddEndpoints` and `RemoveEndpoints`.
// ---------------------------------------------------------------------------

/// Four identifier forms `EndpointConfiguration$EndpointId` names — "If the endpoint is
/// a Network Load Balancer or Application Load Balancer, this is the Amazon Resource
/// Name (ARN) of the resource. If the endpoint is an Elastic IP address, this is the
/// Elastic IP address allocation ID" (and an EC2 instance id) — so the rules below are
/// tested on more than one shape of identifier.
const EIP_ENDPOINT: &str = "eipalloc-0123456789abcdef0";
/// See [`EIP_ENDPOINT`].
const INSTANCE_ENDPOINT: &str = "i-0123456789abcdef0";
/// See [`EIP_ENDPOINT`].
const ALB_ENDPOINT: &str =
    "arn:aws:elasticloadbalancing:us-west-2:000000000000:loadbalancer/app/web/50dc6c495c0c9188";
/// See [`EIP_ENDPOINT`].
const NLB_ENDPOINT: &str =
    "arn:aws:elasticloadbalancing:us-west-2:000000000000:loadbalancer/net/edge/3dc1e2f5a9b74c10";

/// `AddEndpoints` over the group `arn`, with the configurations a row names.
async fn add_endpoints(
    router: &MultiServiceRouter,
    arn: &str,
    configurations: Value,
) -> (u16, Value) {
    call(
        router,
        "AddEndpoints",
        json!({ "EndpointGroupArn": arn, "EndpointConfigurations": configurations }),
    )
    .await
}

/// `RemoveEndpoints` over the group `arn`, with the identifiers a row names.
async fn remove_endpoints(
    router: &MultiServiceRouter,
    arn: &str,
    identifiers: Value,
) -> (u16, Value) {
    call(
        router,
        "RemoveEndpoints",
        json!({ "EndpointGroupArn": arn, "EndpointIdentifiers": identifiers }),
    )
    .await
}

/// The group's stored membership, as `DescribeEndpointGroup` reports it.
async fn membership(router: &MultiServiceRouter, arn: &str) -> Value {
    describe_group(router, arn).await["EndpointDescriptions"].clone()
}

/// The `EndpointId`s of that membership, in the order it reports them.
async fn member_ids(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    membership(router, arn)
        .await
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|endpoint| {
            endpoint["EndpointId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One endpoint group, on an accelerator and listener of its own, holding
/// `configurations` to begin with. Returns the group's ARN.
async fn group_holding(
    router: &MultiServiceRouter,
    suffix: &str,
    configurations: Value,
) -> String {
    let listener_arn = fixture(router, suffix).await;
    let (status, body) = create_group(
        router,
        &listener_arn,
        "us-west-2",
        &format!("grp-{suffix}"),
        json!({ "EndpointConfigurations": configurations }),
    )
    .await;
    assert_eq!(
        status, 200,
        "fixture {suffix:?}: expected HTTP 200, got {status} with {body}"
    );
    group_arn(&body["EndpointGroup"])
}

/// RULE: an add appends the endpoints the request names to the group's stored membership,
/// defaulting the members the caller leaves out, and answers the endpoints THAT call
/// added together with the group's ARN; the endpoints already in the group are neither
/// re-stated by the caller nor disturbed, so a read reports the whole membership in the
/// order it was built.
#[tokio::test]
async fn an_add_appends_the_endpoints_it_names_to_the_stored_membership() {
    let router = emulator();
    let arn = group_holding(&router, "add", json!([])).await;

    // Rows differ along what the rule generalises over: how many endpoints one call
    // adds, which optional members they carry, and which identifier form names them.
    // The expectations are the member docs of `EndpointDescription` — a weight the
    // caller omits is the default 128, `HealthState` is the state before any health
    // check has completed, `HealthReason` "Returns a null result", and
    // `ClientIPPreservationEnabled` is reported only where the caller set it.
    // (case, the call's configurations, the endpoints it answers, the membership after)
    let rows: Vec<(&str, Value, Value, Value)> = vec![
        (
            "one-endpoint-defaulted-weight",
            json!([{ "EndpointId": EIP_ENDPOINT }]),
            json!([{ "EndpointId": EIP_ENDPOINT, "Weight": 128, "HealthState": "INITIAL" }]),
            json!([{ "EndpointId": EIP_ENDPOINT, "Weight": 128, "HealthState": "INITIAL" }]),
        ),
        (
            "two-endpoints-in-one-call",
            json!([
                { "EndpointId": INSTANCE_ENDPOINT, "Weight": 0 },
                { "EndpointId": ALB_ENDPOINT, "Weight": 255 },
            ]),
            json!([
                { "EndpointId": INSTANCE_ENDPOINT, "Weight": 0, "HealthState": "INITIAL" },
                { "EndpointId": ALB_ENDPOINT, "Weight": 255, "HealthState": "INITIAL" },
            ]),
            json!([
                { "EndpointId": EIP_ENDPOINT, "Weight": 128, "HealthState": "INITIAL" },
                { "EndpointId": INSTANCE_ENDPOINT, "Weight": 0, "HealthState": "INITIAL" },
                { "EndpointId": ALB_ENDPOINT, "Weight": 255, "HealthState": "INITIAL" },
            ]),
        ),
        (
            "client-ip-preservation-named",
            json!([
                { "EndpointId": NLB_ENDPOINT, "Weight": 7,
                  "ClientIPPreservationEnabled": false },
            ]),
            json!([
                { "EndpointId": NLB_ENDPOINT, "Weight": 7, "HealthState": "INITIAL",
                  "ClientIPPreservationEnabled": false },
            ]),
            json!([
                { "EndpointId": EIP_ENDPOINT, "Weight": 128, "HealthState": "INITIAL" },
                { "EndpointId": INSTANCE_ENDPOINT, "Weight": 0, "HealthState": "INITIAL" },
                { "EndpointId": ALB_ENDPOINT, "Weight": 255, "HealthState": "INITIAL" },
                { "EndpointId": NLB_ENDPOINT, "Weight": 7, "HealthState": "INITIAL",
                  "ClientIPPreservationEnabled": false },
            ]),
        ),
        (
            // `EndpointConfigurations` is `@length 0..10`, so a list naming no endpoint
            // is a valid argument: it adds nothing and takes nothing away.
            "nothing-to-add",
            json!([]),
            json!([]),
            json!([
                { "EndpointId": EIP_ENDPOINT, "Weight": 128, "HealthState": "INITIAL" },
                { "EndpointId": INSTANCE_ENDPOINT, "Weight": 0, "HealthState": "INITIAL" },
                { "EndpointId": ALB_ENDPOINT, "Weight": 255, "HealthState": "INITIAL" },
                { "EndpointId": NLB_ENDPOINT, "Weight": 7, "HealthState": "INITIAL",
                  "ClientIPPreservationEnabled": false },
            ]),
        ),
    ];

    for (case, configurations, added, after) in rows {
        let (status, body) = add_endpoints(&router, &arn, configurations).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            body["EndpointDescriptions"], added,
            "case {case:?}: expected the endpoints this call added, got {body}"
        );
        assert_eq!(
            body["EndpointGroupArn"],
            json!(arn),
            "case {case:?}: expected the group's own ARN, got {body}"
        );
        let stored = membership(&router, &arn).await;
        assert_eq!(
            stored, after,
            "case {case:?}: expected the accumulated membership, got {stored}"
        );
    }
}

/// RULE: an add is an ADD — a group holds each endpoint once, so an endpoint the group
/// already holds, or one a single request names twice, is refused with the modelled
/// `InvalidArgumentException` and leaves the stored membership exactly as it was.
#[tokio::test]
async fn an_endpoint_the_group_already_holds_cannot_be_added_again() {
    let router = emulator();
    let arn = group_holding(
        &router,
        "dup",
        json!([
            { "EndpointId": EIP_ENDPOINT, "Weight": 10 },
            { "EndpointId": INSTANCE_ENDPOINT },
        ]),
    )
    .await;
    let before = membership(&router, &arn).await;

    // (case, the call's configurations, the message the refusal states if it names one)
    let rows: Vec<(&str, Value, Option<String>)> = vec![
        (
            "already-a-member",
            json!([{ "EndpointId": EIP_ENDPOINT }]),
            Some(format!("Cannot update an existing endpoint: {EIP_ENDPOINT}.")),
        ),
        (
            "already-a-member-under-another-weight",
            json!([{ "EndpointId": INSTANCE_ENDPOINT, "Weight": 200 }]),
            Some(format!(
                "Cannot update an existing endpoint: {INSTANCE_ENDPOINT}."
            )),
        ),
        (
            // The refusal is the whole call's: the endpoint that WOULD have been new is
            // not added either.
            "one-new-endpoint-beside-a-member",
            json!([{ "EndpointId": ALB_ENDPOINT }, { "EndpointId": EIP_ENDPOINT }]),
            Some(format!("Cannot update an existing endpoint: {EIP_ENDPOINT}.")),
        ),
        (
            // The same rule inside ONE request, stated by the configuration reader the
            // create and the update legs share, so its wording is pinned there.
            "named-twice-in-one-call",
            json!([{ "EndpointId": ALB_ENDPOINT }, { "EndpointId": ALB_ENDPOINT }]),
            None,
        ),
        (
            "an-entry-naming-no-endpoint",
            json!([{ "Weight": 5 }]),
            None,
        ),
    ];

    for (case, configurations, message) in rows {
        let (status, body) = add_endpoints(&router, &arn, configurations).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?}: expected the modelled refusal, got {status} with {body}"
        );
        if let Some(expected) = message {
            let actual = error_message(&body);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
        let stored = membership(&router, &arn).await;
        assert_eq!(
            stored, before,
            "case {case:?}: a refused add must leave the membership untouched, got {stored}"
        );
    }
}

/// RULE: a remove takes away exactly the endpoints the request names, by `EndpointId`,
/// and nothing else: an endpoint the group does not hold is already absent, so naming it
/// takes nothing away and refuses nothing, the endpoints that remain keep their order,
/// and an endpoint removed can be added again.
#[tokio::test]
async fn a_remove_takes_away_only_the_endpoints_it_names() {
    let router = emulator();
    let arn = group_holding(
        &router,
        "remove",
        json!([
            { "EndpointId": EIP_ENDPOINT },
            { "EndpointId": INSTANCE_ENDPOINT },
            { "EndpointId": ALB_ENDPOINT },
            { "EndpointId": NLB_ENDPOINT, "ClientIPPreservationEnabled": false },
        ]),
    )
    .await;
    let absent = "eipalloc-ffffffffffffffff0";

    // Rows differ along what the rule generalises over: how many endpoints one call
    // names, whether the group holds them, and which other members the identifier
    // carries. (case, the call's identifiers, the membership left behind)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "an-endpoint-the-group-never-held",
            json!([{ "EndpointId": absent }]),
            vec![
                EIP_ENDPOINT,
                INSTANCE_ENDPOINT,
                ALB_ENDPOINT,
                NLB_ENDPOINT,
            ],
        ),
        (
            "one-member",
            json!([{ "EndpointId": INSTANCE_ENDPOINT }]),
            vec![EIP_ENDPOINT, ALB_ENDPOINT, NLB_ENDPOINT],
        ),
        (
            "the-same-one-again",
            json!([{ "EndpointId": INSTANCE_ENDPOINT }]),
            vec![EIP_ENDPOINT, ALB_ENDPOINT, NLB_ENDPOINT],
        ),
        (
            "a-member-beside-an-absent-endpoint",
            json!([{ "EndpointId": ALB_ENDPOINT }, { "EndpointId": absent }]),
            vec![EIP_ENDPOINT, NLB_ENDPOINT],
        ),
        (
            // `EndpointIdentifier$ClientIPPreservationEnabled` describes the endpoint's
            // own setting; the endpoint is named by its id, so a flag that disagrees with
            // the stored one still names it.
            "an-identifier-carrying-the-preservation-flag",
            json!([{ "EndpointId": NLB_ENDPOINT, "ClientIPPreservationEnabled": true }]),
            vec![EIP_ENDPOINT],
        ),
        (
            "every-endpoint-left",
            json!([{ "EndpointId": EIP_ENDPOINT }]),
            vec![],
        ),
        (
            "from-a-group-that-holds-none",
            json!([{ "EndpointId": EIP_ENDPOINT }, { "EndpointId": absent }]),
            vec![],
        ),
    ];

    for (case, identifiers, left) in rows {
        let (status, body) = remove_endpoints(&router, &arn, identifiers).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            body,
            json!({}),
            "case {case:?}: expected the modelled empty output, got {body}"
        );
        let ids = member_ids(&router, &arn).await;
        let expected: Vec<String> = left.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            ids, expected,
            "case {case:?}: expected the membership left behind, got {ids:?}"
        );
    }

    // A removed endpoint is free to be added again, with a configuration of its own.
    let (status, body) = add_endpoints(
        &router,
        &arn,
        json!([{ "EndpointId": EIP_ENDPOINT, "Weight": 3 }]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case re-add: expected HTTP 200, got {status} with {body}"
    );
    let stored = membership(&router, &arn).await;
    assert_eq!(
        stored,
        json!([{ "EndpointId": EIP_ENDPOINT, "Weight": 3, "HealthState": "INITIAL" }]),
        "case re-add: expected the re-added endpoint to be the group's membership, \
         got {stored}"
    );
}

/// RULE: both membership operations are refused with the group's own modelled not-found
/// error — in the wording this service states that class with, naming the ARN the request
/// carried — when the store holds no such endpoint group, and an identifier that names no
/// endpoint is the modelled `InvalidArgumentException`.
#[tokio::test]
async fn a_membership_call_against_a_group_the_store_does_not_hold_is_refused() {
    let router = emulator();
    let live = group_holding(&router, "missing", json!([{ "EndpointId": EIP_ENDPOINT }])).await;
    let before = membership(&router, &live).await;

    // Two ARNs, so the message is tested as a function of the request rather than on one
    // value: one never minted, one this emulator minted and then deleted.
    let unminted = format!("{live}-not-a-group");
    let deleted = group_holding(&router, "deleted", json!([])).await;
    ok(
        &router,
        "DeleteEndpointGroup",
        json!({ "EndpointGroupArn": deleted }),
    )
    .await;

    for arn in [unminted, deleted] {
        for (case, body) in [
            (
                "AddEndpoints",
                json!({
                    "EndpointGroupArn": arn,
                    "EndpointConfigurations": [{ "EndpointId": EIP_ENDPOINT }],
                }),
            ),
            (
                "RemoveEndpoints",
                json!({
                    "EndpointGroupArn": arn,
                    "EndpointIdentifiers": [{ "EndpointId": EIP_ENDPOINT }],
                }),
            ),
        ] {
            let (status, answer) = call(&router, case, body).await;
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (400, "EndpointGroupNotFoundException"),
                "case {case}/{arn}: expected the modelled not-found, got {status} \
                 with {answer}"
            );
            let actual = error_message(&answer);
            assert_eq!(
                actual,
                format!("EndpointGroup {arn} does not exist."),
                "case {case}/{arn}: expected the refusal to name the group, got {actual:?}"
            );
        }
    }

    // An identifier that carries no endpoint id names no endpoint, and refusing it
    // changes nothing.
    let (status, body) = remove_endpoints(&router, &live, json!([{ "EndpointId": "" }])).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidArgumentException"),
        "case empty-identifier: expected the modelled refusal, got {status} with {body}"
    );
    let stored = membership(&router, &live).await;
    assert_eq!(
        stored, before,
        "case empty-identifier: a refused remove must change nothing, got {stored}"
    );
}

/// RULE: the per-group endpoint quota bounds the membership an add LEAVES BEHIND, not the
/// length of one request, and exceeding a quota is the modelled `LimitExceededException`
/// rather than an invalid argument; a call refused by it leaves the membership untouched,
/// while a call that lands exactly on the bound is accepted.
#[tokio::test]
async fn the_per_group_endpoint_quota_bounds_the_membership_an_add_leaves_behind() {
    let router = emulator();
    // The bound is the model's own `@length` on `EndpointConfigurations` (0..10), which
    // is also the documented quota ("Endpoints per endpoint group: 10").
    let quota = 10;
    let endpoint = |n: usize| json!({ "EndpointId": format!("eipalloc-{n:017}") });
    let arn = group_holding(
        &router,
        "quota",
        json!((1..=8).map(endpoint).collect::<Vec<Value>>()),
    )
    .await;

    // One past the bound: eight held plus three asked for.
    let (status, body) = add_endpoints(&router, &arn, json!([endpoint(9), endpoint(10), endpoint(11)])).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (403, "LimitExceededException"),
        "case past-the-bound: expected the modelled quota refusal, got {status} with {body}"
    );
    let actual = error_message(&body);
    assert_eq!(
        actual,
        format!(
            "Endpoint group {arn} already holds 8 of the maximum of {quota} endpoints; adding \
             3 more would exceed it."
        ),
        "case past-the-bound: expected the quota refusal to state the group and the limit, \
         got {actual:?}"
    );
    let ids = member_ids(&router, &arn).await;
    assert_eq!(
        ids.len(),
        8,
        "case past-the-bound: a refused add must leave the membership untouched, got {ids:?}"
    );

    // Exactly on the bound: the add that fills the group is accepted.
    let (status, body) = add_endpoints(&router, &arn, json!([endpoint(9), endpoint(10)])).await;
    assert_eq!(
        status, 200,
        "case on-the-bound: expected HTTP 200, got {status} with {body}"
    );
    let ids = member_ids(&router, &arn).await;
    assert_eq!(
        ids.len(),
        quota,
        "case on-the-bound: expected a full group, got {ids:?}"
    );

    // One endpoint past a full group.
    let (status, body) = add_endpoints(&router, &arn, json!([endpoint(11)])).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (403, "LimitExceededException"),
        "case full-group: expected the modelled quota refusal, got {status} with {body}"
    );
    let actual = error_message(&body);
    assert_eq!(
        actual,
        format!(
            "Endpoint group {arn} already holds {quota} of the maximum of {quota} endpoints; \
             adding 1 more would exceed it."
        ),
        "case full-group: expected the quota refusal to state the group and the limit, \
         got {actual:?}"
    );
    let ids = member_ids(&router, &arn).await;
    assert_eq!(
        ids.len(),
        quota,
        "case full-group: a refused add must leave the membership untouched, got {ids:?}"
    );

    // The quota is not a cap on the request: a group with room takes a full-length list.
    let roomy = group_holding(&router, "roomy", json!([])).await;
    let (status, body) = add_endpoints(
        &router,
        &roomy,
        json!((1..=quota).map(endpoint).collect::<Vec<Value>>()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case fill-in-one-call: expected HTTP 200, got {status} with {body}"
    );
    let ids = member_ids(&router, &roomy).await;
    assert_eq!(
        ids.len(),
        quota,
        "case fill-in-one-call: expected the whole list to be stored, got {ids:?}"
    );
}

/// RULE: an endpoint whose `AttachmentArn` claims a cross-account attachment the store
/// does not hold is refused with the modelled `AccessDeniedException` — the permission it
/// claims cannot be established — on the add leg exactly as on the create and the update
/// legs, and the refusal adds nothing.
#[tokio::test]
async fn an_add_claiming_an_attachment_that_grants_nothing_is_refused() {
    let router = emulator();
    let arn = group_holding(&router, "attachment", json!([{ "EndpointId": EIP_ENDPOINT }])).await;
    let before = membership(&router, &arn).await;
    let attachment = "arn:aws:globalaccelerator::000000000000:attachment/0a0a0a0a";

    let (status, body) = add_endpoints(
        &router,
        &arn,
        json!([{ "EndpointId": ALB_ENDPOINT, "AttachmentArn": attachment }]),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (403, "AccessDeniedException"),
        "case claimed-attachment: expected the modelled refusal, got {status} with {body}"
    );
    let actual = error_message(&body);
    assert_eq!(
        actual,
        format!(
            "cross-account attachment {attachment} does not exist, so it grants no permission \
             to add endpoint {ALB_ENDPOINT}"
        ),
        "case claimed-attachment: expected the refusal to name the attachment and the \
         endpoint, got {actual:?}"
    );
    let stored = membership(&router, &arn).await;
    assert_eq!(
        stored, before,
        "case claimed-attachment: a refused add must add nothing, got {stored}"
    );
}

// ---------------------------------------------------------------------------
// Client IP address preservation
// ---------------------------------------------------------------------------

/// The sentence the service states when a registration turns client IP address
/// preservation off for an endpoint that is an EC2 instance: fixed text, with no instance
/// id, ARN or Region interpolated.
const PRESERVATION_NOT_OPTIONAL: &str =
    "Client IP Preservation cannot be set to false for EC2 instances";

/// One `EndpointConfiguration`, naming `ClientIPPreservationEnabled` only where a row
/// does.
fn configuration(endpoint_id: &str, preservation: Option<bool>) -> Value {
    let mut configuration = json!({ "EndpointId": endpoint_id });
    if let Some(preservation) = preservation {
        configuration["ClientIPPreservationEnabled"] = json!(preservation);
    }
    configuration
}

/// The `EndpointDescription` a group reports for an endpoint registered that way: a weight
/// no row names takes the documented default of 128, the emulator runs no health checks so
/// every endpoint is `INITIAL`, and the preservation flag is reported only where the caller
/// set it.
fn described(endpoint_id: &str, preservation: Option<bool>) -> Value {
    let mut described = json!({
        "EndpointId": endpoint_id,
        "Weight": 128,
        "HealthState": "INITIAL",
    });
    if let Some(preservation) = preservation {
        described["ClientIPPreservationEnabled"] = json!(preservation);
    }
    described
}

/// The identifier forms a registration names an endpoint with, each with the
/// `ClientIPPreservationEnabled` the row asks for and whether registering it is refused.
///
/// The forms are the ones `EndpointConfiguration$EndpointId` names — a load balancer ARN,
/// an Elastic IP address allocation ID, an EC2 instance ID, an Elastic IP address — and an
/// EC2 instance ID is `i-` followed by either eight or seventeen hexadecimal characters
/// (Amazon EC2 resource IDs), so both published lengths are rows here, and one digit past
/// each of them, and a character that is not a hexadecimal digit, name no instance at all.
///
/// (case, EndpointId, the flag the row names, whether the registration is refused)
fn preservation_rows() -> Vec<(&'static str, &'static str, Option<bool>, bool)> {
    vec![
        ("instance-long-id-off", INSTANCE_ENDPOINT, Some(false), true),
        ("instance-short-id-off", "i-1a2b3c4d", Some(false), true),
        ("instance-long-id-on", INSTANCE_ENDPOINT, Some(true), false),
        ("instance-short-id-flag-unset", "i-1a2b3c4d", None, false),
        ("alb-off", ALB_ENDPOINT, Some(false), false),
        ("nlb-off", NLB_ENDPOINT, Some(false), false),
        ("eip-allocation-off", EIP_ENDPOINT, Some(false), false),
        ("elastic-ip-address-off", "198.51.100.7", Some(false), false),
        ("nine-hex-digits-off", "i-1a2b3c4d5", Some(false), false),
        (
            "eighteen-hex-digits-off",
            "i-0123456789abcdef01",
            Some(false),
            false,
        ),
        (
            "a-non-hex-digit-off",
            "i-0123456789abcdefg",
            Some(false),
            false,
        ),
    ]
}

/// RULE: an endpoint that is an EC2 instance keeps client IP address preservation on, so a
/// registration setting `ClientIPPreservationEnabled` to false for an `EndpointId` of the
/// EC2 instance-id form is refused with the modelled `InvalidArgumentException` and
/// registers nothing, while an endpoint named any other way takes either value. On the
/// update leg: the group it refused keeps the membership it had, an accepted list replaces
/// that membership and round-trips through a read, and an empty list deregisters it.
#[tokio::test]
async fn an_update_refuses_client_ip_preservation_false_for_an_ec2_instance_endpoint() {
    let router = emulator();
    let arn = group_holding(&router, "cipp-update", json!([])).await;

    for (case, endpoint_id, preservation, refused) in preservation_rows() {
        let before = membership(&router, &arn).await;
        let (status, body) = update_group(
            &router,
            &arn,
            json!({ "EndpointConfigurations": [configuration(endpoint_id, preservation)] }),
        )
        .await;
        if refused {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "InvalidArgumentException"),
                "case {case:?}: expected the modelled refusal, got {status} with {body}"
            );
            let actual = error_message(&body);
            assert_eq!(
                actual, PRESERVATION_NOT_OPTIONAL,
                "case {case:?}: expected {PRESERVATION_NOT_OPTIONAL:?}, got {actual:?}"
            );
            let stored = membership(&router, &arn).await;
            assert_eq!(
                stored, before,
                "case {case:?}: a refused update must register nothing, got {stored}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let stored = membership(&router, &arn).await;
        assert_eq!(
            stored,
            json!([described(endpoint_id, preservation)]),
            "case {case:?}: expected the endpoint to round-trip through the store, \
             got {stored}"
        );
    }

    // `EndpointConfigurations` is `@length 0..10`, so a list naming no endpoint is a valid
    // argument: it deregisters every endpoint the group held.
    let (status, body) = update_group(&router, &arn, json!({ "EndpointConfigurations": [] })).await;
    assert_eq!(
        status, 200,
        "case deregister-all: expected HTTP 200, got {status} with {body}"
    );
    let stored = membership(&router, &arn).await;
    assert_eq!(
        stored,
        json!([]),
        "case deregister-all: expected an empty membership, got {stored}"
    );
}

/// RULE: the create leg states the same rule in the same words as the update leg, and a
/// create it refuses stores no endpoint group at all — the Region it named is still free.
#[tokio::test]
async fn a_create_refuses_client_ip_preservation_false_for_an_ec2_instance_endpoint() {
    let router = emulator();

    for (index, (case, endpoint_id, preservation, refused)) in
        preservation_rows().into_iter().enumerate()
    {
        // One listener per row: a listener holds one endpoint group per Region, and a
        // refused row asks again in the Region it was refused in.
        let listener_arn = fixture(&router, &format!("cipp-create-{index}")).await;
        let (status, body) = create_group(
            &router,
            &listener_arn,
            "us-west-2",
            case,
            json!({ "EndpointConfigurations": [configuration(endpoint_id, preservation)] }),
        )
        .await;
        if refused {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "InvalidArgumentException"),
                "case {case:?}: expected the modelled refusal, got {status} with {body}"
            );
            let actual = error_message(&body);
            assert_eq!(
                actual, PRESERVATION_NOT_OPTIONAL,
                "case {case:?}: expected {PRESERVATION_NOT_OPTIONAL:?}, got {actual:?}"
            );
            let (status, body) = create_group(
                &router,
                &listener_arn,
                "us-west-2",
                &format!("{case}-again"),
                json!({
                    "EndpointConfigurations": [configuration(endpoint_id, Some(true))],
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: a refused create must store no group, so the Region is \
                 still free, got {status} with {body}"
            );
            let arn = group_arn(&body["EndpointGroup"]);
            let stored = membership(&router, &arn).await;
            assert_eq!(
                stored,
                json!([described(endpoint_id, Some(true))]),
                "case {case:?}: expected the endpoint to round-trip with preservation on, \
                 got {stored}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let arn = group_arn(&body["EndpointGroup"]);
        let stored = membership(&router, &arn).await;
        assert_eq!(
            stored,
            json!([described(endpoint_id, preservation)]),
            "case {case:?}: expected the endpoint to round-trip through the store, \
             got {stored}"
        );
    }
}

/// RULE: the add leg states the same rule in the same words as the create and the update
/// legs, and an add it refuses leaves the group's membership untouched.
#[tokio::test]
async fn an_add_refuses_client_ip_preservation_false_for_an_ec2_instance_endpoint() {
    let router = emulator();
    let arn = group_holding(&router, "cipp-add", json!([])).await;

    for (case, endpoint_id, preservation, refused) in preservation_rows() {
        let (status, body) = add_endpoints(
            &router,
            &arn,
            json!([configuration(endpoint_id, preservation)]),
        )
        .await;
        if refused {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "InvalidArgumentException"),
                "case {case:?}: expected the modelled refusal, got {status} with {body}"
            );
            let actual = error_message(&body);
            assert_eq!(
                actual, PRESERVATION_NOT_OPTIONAL,
                "case {case:?}: expected {PRESERVATION_NOT_OPTIONAL:?}, got {actual:?}"
            );
            let stored = membership(&router, &arn).await;
            assert_eq!(
                stored,
                json!([]),
                "case {case:?}: a refused add must register nothing, got {stored}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            body["EndpointDescriptions"],
            json!([described(endpoint_id, preservation)]),
            "case {case:?}: expected the add to answer the endpoint it registered, \
             got {body}"
        );
        // Back to an empty group, so every row starts from the same membership.
        let (status, body) =
            update_group(&router, &arn, json!({ "EndpointConfigurations": [] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the group to be emptied, got {status} with {body}"
        );
    }
}
