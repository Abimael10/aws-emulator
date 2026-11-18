// SPDX-License-Identifier: CC0-1.0

//! An instance launched ONTO a Dedicated Host, driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a Dedicated Host reports the instances that are running on it, and
//! that is a dependency a release has to honour.** "The IDs and instance type that are
//! currently running on the Dedicated Host" (`Host$Instances`) follows from where the
//! account's instances were launched — `Placement$HostId` — so a launch onto a host makes the
//! host report that instance, `Instance$Placement.HostId` reports the host, and `ReleaseHosts`
//! reports the host under `Unsuccessful` with `Client.InvalidHost.Occupied` instead of
//! releasing it: "you
//! must stop or terminate all instances on a host before it can be released"
//! (`ReleaseHosts`). Once the instances are gone the same release succeeds.
//!
//! The rows differ along what the rule generalises over: how many instances one launch puts on
//! the host, and whether the host was allocated for one instance type or for a whole instance
//! family. A `HostId` the account holds no host for is the refusal half of the same rule.

use ec2::state::{
    availability_zones_in_region, HOST_OCCUPIED, HOST_STATE_AVAILABLE, HOST_STATE_RELEASED,
    INVALID_HOST_ID_NOT_FOUND, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, indexed};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch names. `RunInstances` only has to accept the id; which image an
/// instance came from has no bearing on the host it runs on.
const AMI: &str = "ami-0123456789abcdef0";

/// The instance type the hosts here are allocated for and the instances launched as, and the
/// family it belongs to.
const INSTANCE_TYPE: &str = "m5.large";

/// The family of [`INSTANCE_TYPE`], for the host allocated for a whole family.
const INSTANCE_FAMILY: &str = "m5";

/// A well-formed host id the account holds no host for.
const UNKNOWN_HOST: &str = "h-00000000000000000";

/// The emulator the tests drive, with the engine behind it.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The name of the first Availability Zone of the signed Region.
fn zone_name() -> String {
    availability_zones_in_region(REGION)[0].zone_name.clone()
}

/// A Dedicated Host of the account, as its `HostId`. `by_family` allocates it for the whole
/// instance family rather than for one instance type.
async fn allocate(case: &str, router: &MultiServiceRouter, by_family: bool) -> String {
    let mut params = vec![
        param("AvailabilityZone", &zone_name()),
        param("Quantity", "1"),
        param("AutoPlacement", "off"),
    ];
    if by_family {
        params.push(param("InstanceFamily", INSTANCE_FAMILY));
    } else {
        params.push(param("InstanceType", INSTANCE_TYPE));
    }
    let body = ok(case, router, "AllocateHosts", &params).await;
    let allocated = element(&body, "hostIdSet")
        .map(|set| elements(&set, "item"))
        .unwrap_or_default();
    assert_eq!(
        allocated.len(),
        1,
        "case {case:?}: expected one allocated host, got {allocated:?} from {body}"
    );
    allocated[0].clone()
}

/// The instance ids a launch of `count` instances onto the host `host_id` minted.
async fn launch_onto(
    case: &str,
    router: &MultiServiceRouter,
    host_id: &str,
    count: usize,
) -> Vec<String> {
    let body = ok(
        case,
        router,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("InstanceType", INSTANCE_TYPE),
            param("MinCount", &count.to_string()),
            param("MaxCount", &count.to_string()),
            param("Placement.HostId", host_id),
            param("Placement.AvailabilityZone", &zone_name()),
            param("Placement.Tenancy", "host"),
        ],
    )
    .await;
    let ids = elements(&body, "instanceId");
    assert_eq!(
        ids.len(),
        count,
        "case {case:?}: expected {count} instance(s) launched onto {host_id}, got {ids:?}"
    );
    ids
}

/// The `(instanceId, instanceType)` pairs the host `host_id` reports as running on it, in the
/// order `DescribeHosts` reports them.
async fn host_instances(
    case: &str,
    router: &MultiServiceRouter,
    host_id: &str,
) -> Vec<(String, String)> {
    let body = ok(
        case,
        router,
        "DescribeHosts",
        &indexed("HostId", &[host_id]),
    )
    .await;
    let parsed = document(case, &body);
    let host = parsed
        .descendants()
        .find(|node| node.is_element() && child_text(*node, "hostId").as_deref() == Some(host_id))
        .unwrap_or_else(|| panic!("case {case:?}: no host {host_id} in {body}"));
    set_items(host, "instances")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "instanceId").unwrap_or_default(),
                child_text(item, "instanceType").unwrap_or_default(),
            )
        })
        .collect()
}

/// The `Placement.HostId` the instance `instance_id` reports, or `None` when it reports none.
async fn placement_host(case: &str, router: &MultiServiceRouter, instance_id: &str) -> Option<String> {
    let body = ok(
        case,
        router,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed.descendants().find(|node| {
        node.is_element() && child_text(*node, "instanceId").as_deref() == Some(instance_id)
    })?;
    child(instance, "placement").and_then(|placement| child_text(placement, "hostId"))
}

/// The `(state, releaseTime)` `DescribeHosts` reports for one host.
async fn host_state(case: &str, router: &MultiServiceRouter, host_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeHosts",
        &indexed("HostId", &[host_id]),
    )
    .await;
    element(&body, "state")
        .unwrap_or_else(|| panic!("case {case:?}: no <state> for {host_id} in {body}"))
}

/// One `ReleaseHosts` call, as the ids it reported successful and the `(id, code)` pairs it
/// reported unsuccessful.
async fn release(
    case: &str,
    router: &MultiServiceRouter,
    host_id: &str,
) -> (Vec<String>, Vec<(String, String)>) {
    let body = ok(
        case,
        router,
        "ReleaseHosts",
        &indexed("HostId", &[host_id]),
    )
    .await;
    let successful = element(&body, "successful")
        .map(|set| elements(&set, "item"))
        .unwrap_or_default();
    let unsuccessful = element(&body, "unsuccessful")
        .map(|set| {
            elements(&set, "resourceId")
                .into_iter()
                .zip(elements(&set, "code"))
                .collect()
        })
        .unwrap_or_default();
    (successful, unsuccessful)
}

/// Look again until the host `host_id` reports no instance running on it, or a generous
/// deadline passes.
///
/// The condition is looked at AGAIN in a loop rather than waited out, so nothing here depends
/// on how fast the host runs; the deadline is a multiple of the service's own transition span,
/// far enough out that a loaded machine is never the reason it fails.
async fn poll_until_host_is_empty(case: &str, router: &MultiServiceRouter, host_id: &str) {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let running = host_instances(case, router, host_id).await;
        if running.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the host {host_id} must stop reporting instances, last read \
             {running:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    }
}

/// The rule: a launch onto a host makes the host report the instances, which blocks the
/// release until they are gone.
#[tokio::test]
async fn a_host_reports_its_instances_and_cannot_be_released_under_them() {
    // Rows: one instance and two on a host allocated for one instance type, and one instance
    // on a host allocated for the whole family.
    for (count, by_family) in [(1usize, false), (2, false), (1, true)] {
        let case = format!(
            "{count} instance(s) on a host allocated by {}",
            if by_family { "family" } else { "type" }
        );
        let case = case.as_str();
        let (router, _engine) = emulator();
        let host_id = allocate(case, &router, by_family).await;
        assert!(
            host_instances(case, &router, &host_id).await.is_empty(),
            "case {case:?}: a freshly allocated host runs nothing"
        );

        let instances = launch_onto(case, &router, &host_id, count).await;

        // The instance names the host it was placed on.
        for instance_id in &instances {
            assert_eq!(
                placement_host(case, &router, instance_id).await.as_deref(),
                Some(host_id.as_str()),
                "case {case:?}: the instance {instance_id} must report the host it runs on"
            );
        }
        // And the host names the instances, with the type each runs as.
        let mut reported = host_instances(case, &router, &host_id).await;
        reported.sort();
        let mut expected: Vec<(String, String)> = instances
            .iter()
            .map(|instance_id| (instance_id.clone(), INSTANCE_TYPE.to_string()))
            .collect();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the host to report its instances, got {reported:?}"
        );

        // A host with instances on it is not released.
        let (successful, unsuccessful) = release(case, &router, &host_id).await;
        assert_eq!(
            (successful.as_slice(), unsuccessful.as_slice()),
            (
                &[][..],
                &[(host_id.clone(), HOST_OCCUPIED.to_string())][..]
            ),
            "case {case:?}: expected the occupied host reported unsuccessful and nothing \
             released"
        );
        assert_eq!(
            host_state(case, &router, &host_id).await,
            HOST_STATE_AVAILABLE,
            "case {case:?}: the occupied host must stay as it was"
        );

        // Terminated instances leave the host, and then the same release gives it back.
        let mut termination: Vec<Param> = Vec::new();
        for (index, instance_id) in instances.iter().enumerate() {
            termination.push(param(&format!("InstanceId.{}", index + 1), instance_id));
        }
        ok(case, &router, "TerminateInstances", &termination).await;
        poll_until_host_is_empty(case, &router, &host_id).await;

        let (successful, unsuccessful) = release(case, &router, &host_id).await;
        assert_eq!(
            (successful.as_slice(), unsuccessful.as_slice()),
            (&[host_id.clone()][..], &[][..]),
            "case {case:?}: expected the empty host released"
        );
        assert_eq!(
            host_state(case, &router, &host_id).await,
            HOST_STATE_RELEASED,
            "case {case:?}: the released host reports the released state"
        );
    }
}

/// The refusal half: a launch onto a `HostId` the account holds no host for is refused rather
/// than placed on a host nothing holds — and so is one onto a host already released.
#[tokio::test]
async fn a_launch_onto_a_host_the_account_does_not_hold_is_refused() {
    let case = "an unknown host";
    let (router, _engine) = emulator();
    let released = allocate(case, &router, false).await;
    let (successful, _) = release(case, &router, &released).await;
    assert_eq!(
        successful,
        vec![released.clone()],
        "case {case:?}: the host must be released for the row below"
    );

    // Rows: an id the account never held, and one it held and gave back.
    for (row, host_id) in [
        ("an id nothing minted", UNKNOWN_HOST.to_string()),
        ("a host already released", released.clone()),
    ] {
        let (status, body) = call(
            &router,
            "RunInstances",
            &[
                param("ImageId", AMI),
                param("InstanceType", INSTANCE_TYPE),
                param("MinCount", "1"),
                param("MaxCount", "1"),
                param("Placement.HostId", &host_id),
            ],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, INVALID_HOST_ID_NOT_FOUND),
            "case {row:?}: expected 400 {INVALID_HOST_ID_NOT_FOUND} for the host {host_id}, got \
             {status} with body {body}"
        );
    }
}
