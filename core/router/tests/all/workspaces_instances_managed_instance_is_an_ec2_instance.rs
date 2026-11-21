// SPDX-License-Identifier: CC0-1.0

//! The managed instance a WorkSpace Instance reports IS an Amazon EC2 instance of the
//! caller's account.
//!
//! RULE: the id `GetWorkspaceInstance` reports under `EC2ManagedInstance$InstanceId` —
//! "Unique identifier of the managed EC2 instance" — names an EC2 instance of the caller's
//! own account in the Region the create was signed for, launched with the configuration
//! `ManagedInstance` carried. So the service that OWNS instances holds it: `ec2`'s
//! `DescribeInstances` naming that id is SERVED, with a reservation holding exactly that
//! instance, reporting the image it boots, the type it runs as, where it is placed and the
//! tags the create asked for, and listing it among the account's instances; and because
//! deleting the WorkSpace Instance shuts that instance down, its `instanceState.name` reads
//! `terminated` there afterwards. One WorkSpace Instance adds exactly ONE instance to the
//! account's EC2 inventory — the one whose id it reports — so no id is reported that EC2
//! does not hold.
//!
//! This suite lives beside the router because the instance is EC2's: both services are
//! mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them, and nothing here
//! reaches into either store — an instance is believed to exist only because EC2 reported it
//! over its own wire, and its configuration and state only because EC2 reported those too.
//! That is the cross-service observation neither crate's own suites can make.
//!
//! The rows differ along what the rule generalises over: the instance type (burstable,
//! general purpose, compute optimised, memory optimised under dedicated tenancy, graphics),
//! the billing mode (absent, so the documented default, and each value the model admits),
//! whether the create places the instance in a named zone, where its tags come from (none,
//! the top-level `Tags`, a `TagSpecifications` entry, both) and the Region the whole flow is
//! signed for.
//!
//! Sources: `smithy_query(query="operation",operation="CreateWorkspaceInstance,\
//! GetWorkspaceInstance,DeleteWorkspaceInstance",model_path="services/\
//! service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/smithy-model/\
//! com-amazonaws-workspacesinstances-eucmifrontendapiservice.json")` for the launch surface,
//! the `ProvisionStateEnum` values and `EC2ManagedInstance`;
//! `smithy_query(query="operation",operation="DescribeInstances",model_path="services/\
//! service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")` for
//! the `reservationSet`/`instancesSet`/`instanceState` wire path the answers are read along.
//! Every instance type named below is one WorkSpaces Instances publishes through
//! `ListInstanceTypes` (`crate::state::INSTANCE_TYPE_FAMILIES` of that crate), and a row that
//! names a zone names one its Region publishes AND offers that type in (`ec2`'s
//! `DescribeInstanceTypeOfferings`), so a row is about the rule rather than about a launch
//! EC2 could not place.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{block_of, element, elements, form_encode, items_of, opt_element};

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of this service's `awsJson1_0` operations.
const WSI_TARGET_PREFIX: &str = "EUCMIFrontendAPIService";

/// The signing name that routes a WorkSpaces Instances request.
const WSI_SIGNING_NAME: &str = "workspaces-instances";

/// The `ProvisionStateEnum` values this suite waits on: a create answers before the launch
/// has finished, and a delete before the teardown has.
const ALLOCATED: &str = "ALLOCATED";
const DEALLOCATED: &str = "DEALLOCATED";

/// Every `ProvisionStateEnum` value a WorkSpace Instance STAYS in: the two a lifecycle ends
/// in and the two a failed launch or teardown ends in. A record in one of them will not move
/// again, so a poll that is waiting for a different one is already lost.
const TERMINAL_PROVISION_STATES: &[&str] = &[
    ALLOCATED,
    DEALLOCATED,
    "ERROR_ALLOCATING",
    "ERROR_DEALLOCATING",
];

/// The `InstanceStateName` values this suite waits on: an instance is launched `pending` and
/// reaches `running`, and a terminated one reaches `terminated`.
const RUNNING: &str = "running";
const TERMINATED: &str = "terminated";

/// The billing mode the model documents for a create that supplies none: "Defaults to hourly
/// billing if not specified" (`CreateWorkspaceInstance$BillingConfiguration`).
const DEFAULT_BILLING_MODE: &str = "HOURLY";

/// How long the emulator is given to reach a state its clock owes. Nothing here sleeps for a
/// span: the suite looks again until the condition holds, with generous room so a loaded host
/// cannot fail it.
const SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One emulator carrying WorkSpaces Instances and Amazon EC2 over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are the emulator's concern, not a service's; the credential scope below is
    // what routes each request and what each handler reads its Region from.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_workspacesinstances_eucmifrontendapiservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`,
/// which is what routes the request and what the handler reads its Region from; the signature
/// is never checked.
fn authorization(region: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one WorkSpaces Instances operation over its `awsJson1_0` wire, as the HTTP status
/// and the parsed body.
async fn wsi(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{WSI_SIGNING_NAME}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{WSI_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(region, WSI_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("serialize the request body"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let payload = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// One WorkSpaces Instances operation that must be served, answered with its parsed body.
async fn wsi_ok(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, payload) = wsi(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must be served, got HTTP {status} with {payload}"
    );
    payload
}

/// Call one Amazon EC2 action over its ec2Query wire, as the HTTP status and the XML body.
async fn ec2(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(region, "ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One EC2 action that must be served, answered with its XML body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = ec2(router, region, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ec2:{action} must be served, got HTTP {status} with {body}"
    );
    body
}

/// An AMI the account can see in `region`, read the way a client chooses one — so a launch
/// naming it boots an image Amazon EC2 really holds.
async fn catalog_image(router: &MultiServiceRouter, region: &str, case: &str) -> String {
    let body = ec2_ok(router, region, case, "DescribeImages", &[]).await;
    let image_id = element(&body, "imageId");
    assert!(
        image_id.starts_with("ami-"),
        "case {case:?}: the image catalog must publish an AMI, got {image_id:?}"
    );
    image_id
}

/// Every instance id Amazon EC2 holds for the account in `region`.
async fn held_instance_ids(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
) -> BTreeSet<String> {
    let body = ec2_ok(router, region, case, "DescribeInstances", &[]).await;
    elements(&body, "instanceId").into_iter().collect()
}

/// Whether `id` is inside the instance-id form Amazon EC2 ISSUES: `i-` and 17 lowercase
/// hexadecimal digits under a leading `0`, the long resource-id form AWS names in its
/// `InvalidInstanceID.Malformed` entry ("the form `i-xxxxxxxxxxxxxxxxx`").
fn is_issued_instance_id(id: &str) -> bool {
    let Some(digits) = id.strip_prefix("i-") else {
        return false;
    };
    digits.len() == 17
        && digits.starts_with('0')
        && digits
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The one instance Amazon EC2 reports for `instance_id`, as the `instancesSet` block of the
/// reservation holding it.
///
/// The describe MUST be served: a client that was handed the id of a managed instance reads
/// the instance back with it, so a refusal is a failure of the rule and not a state to poll
/// through. The reservation is checked here too — a described instance is reported inside the
/// reservation its launch minted.
async fn described(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    instance_id: &str,
) -> String {
    let body = ec2_ok(
        router,
        region,
        case,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    )
    .await;
    let reservations = block_of(&body, "reservationSet").unwrap_or_else(|| {
        panic!("case {case:?}: expected a reservation holding {instance_id}, got {body}")
    });
    let reservation_id = element(reservations, "reservationId");
    assert!(
        reservation_id.starts_with("r-"),
        "case {case:?}: expected {instance_id} inside a reservation, got \
         reservationId {reservation_id:?}"
    );
    let instances = block_of(reservations, "instancesSet").unwrap_or_else(|| {
        panic!("case {case:?}: the reservation holds no instancesSet: {body}")
    });
    let reported: Vec<String> = elements(instances, "instanceId");
    assert_eq!(
        reported,
        vec![instance_id.to_string()],
        "case {case:?}: expected the reservation to hold {instance_id} alone, it holds \
         {reported:?}"
    );
    instances.to_string()
}

/// The state Amazon EC2 reports an instance in, read out of that instance's own state element.
fn reported_state(instance: &str) -> String {
    let state = block_of(instance, "instanceState").unwrap_or_default();
    opt_element(state, "name").unwrap_or_default()
}

/// Look again until Amazon EC2 reports `instance_id` in `state`, and answer the block it
/// reported. Waits for the CONDITION, not for a span: the deadline is only the point past
/// which the condition is taken to be unreachable.
async fn settled_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    instance_id: &str,
    state: &str,
) -> String {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let instance = described(router, region, case, instance_id).await;
        let reported = reported_state(&instance);
        if reported == state {
            return instance;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected EC2 to report {instance_id} {state}, it reports \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `GetWorkspaceInstance` reports `workspace_instance_id` in
/// `provision_state`, and answer what it reported then.
async fn provisioned(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    workspace_instance_id: &str,
    provision_state: &str,
) -> Value {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let reported = wsi_ok(
            router,
            region,
            case,
            "GetWorkspaceInstance",
            json!({ "WorkspaceInstanceId": workspace_instance_id }),
        )
        .await;
        let state = reported["ProvisionState"].as_str().unwrap_or_default();
        if state == provision_state {
            return reported;
        }
        // A record that has reached one of the lifecycle's OTHER terminal states will never
        // reach this one, so the failure is reported at once, with what the service said went
        // wrong, rather than after the whole budget.
        assert!(
            !TERMINAL_PROVISION_STATES.contains(&state),
            "case {case:?}: expected {workspace_instance_id} in {provision_state}, it settled \
             in {state}: {reported}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {workspace_instance_id} in {provision_state}, it reports \
             {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The tags Amazon EC2 reports on one instance block, as the map a client reads them as.
fn instance_tags(instance: &str) -> BTreeMap<String, String> {
    let Some(set) = block_of(instance, "tagSet") else {
        return BTreeMap::new();
    };
    items_of(set)
        .into_iter()
        .filter_map(|entry| {
            Some((
                opt_element(entry, "key")?,
                opt_element(entry, "value").unwrap_or_default(),
            ))
        })
        .collect()
}

/// One WorkSpace Instance the rule is asserted over.
struct Row {
    case: &'static str,
    /// The Region the whole flow is signed for.
    region: &'static str,
    /// The `CreateWorkspaceInstance` body, over the AMI the account holds.
    body: fn(&str) -> Value,
    /// The instance type Amazon EC2 must report the instance as.
    instance_type: &'static str,
    /// The zone it must report, for a row whose placement named one; `None` leaves the zone
    /// to the service, and then only the Region it falls in is asserted.
    zone: Option<&'static str>,
    /// The `Placement.Tenancy` it must report, for a row that named one.
    tenancy: Option<&'static str>,
    /// The billing mode `GetWorkspaceInstance` must report back.
    billing_mode: &'static str,
    /// The tags EC2 must report on the instance.
    tags: &'static [(&'static str, &'static str)],
}

/// The rows: one per thing the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "the documented default billing mode on a burstable type, placed by the service",
        region: "us-east-1",
        body: |image| {
            json!({
                "ManagedInstance": { "ImageId": image, "InstanceType": "t3.micro" },
            })
        },
        instance_type: "t3.micro",
        zone: None,
        tenancy: None,
        billing_mode: DEFAULT_BILLING_MODE,
        tags: &[],
    },
    Row {
        case: "a monthly billing configuration on a general-purpose type in a named zone",
        region: "us-east-1",
        body: |image| {
            json!({
                "ManagedInstance": {
                    "ImageId": image,
                    "InstanceType": "m5.large",
                    "Placement": { "AvailabilityZone": "us-east-1b" },
                },
                "BillingConfiguration": { "BillingMode": "MONTHLY" },
                "Tags": [{ "Key": "owner", "Value": "alice" }],
            })
        },
        instance_type: "m5.large",
        zone: Some("us-east-1b"),
        tenancy: None,
        billing_mode: "MONTHLY",
        tags: &[("owner", "alice")],
    },
    Row {
        case: "an explicitly hourly graphics instance tagged through both members",
        region: "us-east-1",
        body: |image| {
            json!({
                "ManagedInstance": {
                    "ImageId": image,
                    "InstanceType": "g4dn.xlarge",
                    "TagSpecifications": [
                        { "ResourceType": "instance", "Tags": [{ "Key": "team", "Value": "eu" }] },
                    ],
                },
                "BillingConfiguration": { "BillingMode": "HOURLY" },
                "Tags": [{ "Key": "stage", "Value": "dev" }],
            })
        },
        instance_type: "g4dn.xlarge",
        zone: None,
        tenancy: None,
        billing_mode: "HOURLY",
        tags: &[("stage", "dev"), ("team", "eu")],
    },
    Row {
        case: "a memory-optimised instance on dedicated hardware",
        region: "us-east-1",
        body: |image| {
            json!({
                "ManagedInstance": {
                    "ImageId": image,
                    "InstanceType": "r5.xlarge",
                    "Placement": {
                        "AvailabilityZone": "us-east-1a",
                        "Tenancy": "dedicated",
                    },
                },
            })
        },
        instance_type: "r5.xlarge",
        zone: Some("us-east-1a"),
        tenancy: Some("dedicated"),
        billing_mode: DEFAULT_BILLING_MODE,
        tags: &[],
    },
    Row {
        case: "a compute-optimised instance of another Region",
        region: "us-west-2",
        body: |image| {
            json!({
                "ManagedInstance": {
                    "ImageId": image,
                    "InstanceType": "c5.large",
                    "Placement": { "AvailabilityZone": "us-west-2b" },
                },
                "BillingConfiguration": { "BillingMode": "MONTHLY" },
                "Tags": [
                    { "Key": "owner", "Value": "bob" },
                    { "Key": "stage", "Value": "prod" },
                ],
            })
        },
        instance_type: "c5.large",
        zone: Some("us-west-2b"),
        tenancy: None,
        billing_mode: "MONTHLY",
        tags: &[("owner", "bob"), ("stage", "prod")],
    },
];

#[tokio::test]
async fn the_managed_instance_a_workspace_instance_reports_is_an_ec2_instance_of_the_account() {
    let router = emulator();

    let mut reported: Vec<String> = Vec::new();
    for row in ROWS {
        let case = row.case;
        let region = row.region;
        let image = catalog_image(&router, region, case).await;
        // What the account's EC2 inventory holds BEFORE the launch, so the instances the
        // WorkSpace Instance adds to it can be counted exactly.
        let held = held_instance_ids(&router, region, case).await;

        let created = wsi_ok(
            &router,
            region,
            case,
            "CreateWorkspaceInstance",
            (row.body)(&image),
        )
        .await;
        let workspace_instance_id = created["WorkspaceInstanceId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateWorkspaceInstance reported no id: {created}")
            })
            .to_string();

        // The managed instance is reported once the launch has finished, which is what
        // reaching the terminal `ALLOCATED` means.
        let allocated = provisioned(
            &router,
            region,
            case,
            &workspace_instance_id,
            ALLOCATED,
        )
        .await;
        let instance_id = allocated["EC2ManagedInstance"]["InstanceId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: an allocated WorkSpace Instance reports no managed \
                        instance: {allocated}")
            })
            .to_string();
        assert!(
            is_issued_instance_id(&instance_id),
            "case {case:?}: expected an instance id EC2 issues, got {instance_id:?}"
        );
        assert!(
            !reported.contains(&instance_id),
            "case {case:?}: expected an instance of its own, got the already-reported \
             {instance_id:?}"
        );
        assert_eq!(
            allocated["BillingConfiguration"]["BillingMode"].as_str(),
            Some(row.billing_mode),
            "case {case:?}: expected the billing mode {:?}, got {allocated}",
            row.billing_mode
        );

        // The service that OWNS instances holds it: this is the whole point of answering its
        // id. The instance is launched `pending` and reaches `running` without anyone asking.
        let instance = settled_in(&router, region, case, &instance_id, RUNNING).await;
        assert_eq!(
            element(&instance, "imageId"),
            image,
            "case {case:?}: expected EC2 to report {instance_id} booting {image}, got {instance}"
        );
        assert_eq!(
            element(&instance, "instanceType"),
            row.instance_type,
            "case {case:?}: expected EC2 to report {instance_id} as {}, got {instance}",
            row.instance_type
        );
        let placement = block_of(&instance, "placement").unwrap_or_else(|| {
            panic!("case {case:?}: EC2 reports no placement for {instance_id}: {instance}")
        });
        let zone = element(placement, "availabilityZone");
        match row.zone {
            Some(named) => assert_eq!(
                zone, named,
                "case {case:?}: expected {instance_id} in {named}, EC2 reports {zone:?}"
            ),
            // A create that named no zone leaves the choice to the service, which places the
            // instance in the Region the call was signed for.
            None => assert!(
                zone.starts_with(region),
                "case {case:?}: expected {instance_id} in a zone of {region}, EC2 reports \
                 {zone:?}"
            ),
        }
        if let Some(tenancy) = row.tenancy {
            let reported_tenancy = opt_element(placement, "tenancy");
            assert_eq!(
                reported_tenancy.as_deref(),
                Some(tenancy),
                "case {case:?}: expected {instance_id} on {tenancy} hardware, EC2 reports \
                 {reported_tenancy:?}"
            );
        }
        let expected_tags: BTreeMap<String, String> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            instance_tags(&instance),
            expected_tags,
            "case {case:?}: expected EC2 to report the tags {expected_tags:?} on \
             {instance_id}, got {instance}"
        );

        // ONE WorkSpace Instance is ONE instance of the account's inventory, and it is the
        // one whose id was reported: an id reported for an instance EC2 does not hold, or a
        // second instance launched beside it, both fail here.
        let now_held = held_instance_ids(&router, region, case).await;
        let added: Vec<String> = now_held.difference(&held).cloned().collect();
        assert_eq!(
            added,
            vec![instance_id.clone()],
            "case {case:?}: expected the launch to add {instance_id} alone to the account's \
             instances, it added {added:?}"
        );

        // TEARDOWN. Deleting the WorkSpace Instance shuts the instance it IS down, so the
        // instance reads `terminated` in EC2 — the assertion a client polls for to confirm a
        // deprovision finished — and the record reaches its own terminal state.
        wsi_ok(
            &router,
            region,
            case,
            "DeleteWorkspaceInstance",
            json!({ "WorkspaceInstanceId": workspace_instance_id }),
        )
        .await;
        let terminated = settled_in(&router, region, case, &instance_id, TERMINATED).await;
        assert_eq!(
            element(&terminated, "instanceId"),
            instance_id,
            "case {case:?}: expected the terminated instance to still be {instance_id}, got \
             {terminated}"
        );
        let deallocated = provisioned(
            &router,
            region,
            case,
            &workspace_instance_id,
            DEALLOCATED,
        )
        .await;
        assert_eq!(
            deallocated["EC2ManagedInstance"]["InstanceId"].as_str(),
            Some(instance_id.as_str()),
            "case {case:?}: expected the torn-down record to keep naming {instance_id}, got \
             {deallocated}"
        );

        reported.push(instance_id);
    }
}
