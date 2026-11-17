// SPDX-License-Identifier: CC0-1.0

//! A volume WorkSpaces Instances creates IS an Amazon EBS volume of the caller's account.
//!
//! RULE: `CreateVolume` of WorkSpaces Instances carries Amazon EBS's own configuration
//! surface — the `AvailabilityZone` the volume is created in, `SizeInGB`, the `VolumeType`
//! enum AWS publishes for EBS, `Iops`, `Throughput`, `Encrypted`/`KmsKeyId` and
//! `TagSpecifications` whose `ResourceType` is `volume` — and the id it answers is an EBS
//! `VolumeId`, so the volume a successful create reports is a volume the service that OWNS
//! EBS volumes holds: `ec2:DescribeVolumes` with that id resolves a volume of the requested
//! zone, size, type, performance, encryption state and tags, EC2's own write and read paths
//! act on it (`ec2:CreateTags`, `ec2:CreateSnapshot`), and `DeleteVolume` makes it disappear
//! from there. A create WorkSpaces Instances refuses creates no volume at all, so the
//! account's EBS inventory is untouched by a refusal.
//!
//! This suite lives beside the router because the volume is EC2's: both services are mounted
//! on ONE engine exactly as `core/router/src/lib.rs` mounts them, and nothing here reaches
//! into either store — a volume is believed to exist only because EC2 reported it over its
//! own wire, and its configuration only because EC2 reported that too. That is the
//! cross-service observation the WorkSpaces Instances crate's own suites cannot make on the
//! assembled emulator.
//!
//! The rows differ along what the rule generalises over: the volume type (each of the seven
//! the model admits), the size (the published bottom of a type's range and sizes above it),
//! the performance members a type takes (`Iops`, `Throughput`), the encryption state and
//! key, the zone letter, and where the tags come from (none, one specification, two).
//!
//! Sources: `smithy_query(query="operation",operation="CreateVolume,DeleteVolume",
//! model_path="services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/\
//! smithy-model/com-amazonaws-workspacesinstances-eucmifrontendapiservice.json")` for the
//! configuration surface, the `^vol-[0-9a-zA-Z]{1,63}$` id form and the modelled
//! `ValidationException`; the published per-type Size/Iops/Throughput ranges quoted by
//! `crate::state::VOLUME_TYPE_SPECS` in that crate for the values each row names.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{block_of, element, elements, form_encode, items_of, opt_element};

/// The Region every request below is signed for. A regional resource belongs to the Region
/// its create was signed for, and each zone named below is one that Region publishes.
const REGION: &str = "us-east-1";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of this service's `awsJson1_0` operations.
const WSI_TARGET_PREFIX: &str = "EUCMIFrontendAPIService";

/// The signing name that routes a WorkSpaces Instances request.
const WSI_SIGNING_NAME: &str = "workspaces-instances";

/// The `VolumeState` values this suite names: a volume is answered `creating` and the
/// owning service's clock brings it to `available`.
const STATE_CREATING: &str = "creating";
const STATE_AVAILABLE: &str = "available";

/// Amazon EC2's own code for a volume the account holds none of.
const VOLUME_NOT_FOUND: &str = "InvalidVolume.NotFound";

/// How long the emulator is given to bring a volume to the state its clock owes it. Nothing
/// here sleeps for a span: the suite looks again until the condition holds, with generous
/// room so a loaded host cannot fail it.
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one WorkSpaces Instances operation over its `awsJson1_0` wire, as the HTTP status
/// and the parsed body.
async fn wsi(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{WSI_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{WSI_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(WSI_SIGNING_NAME))
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

/// The `awsJson` error code of a refusal: the `__type` discriminator, without its namespace.
fn wsi_error_code(payload: &Value) -> String {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Call one Amazon EC2 action over its ec2Query wire, as the HTTP status and the XML body.
async fn ec2(
    router: &MultiServiceRouter,
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
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One EC2 action that must be served, answered with its XML body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = ec2(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ec2:{action} must be served, got HTTP {status} with {body}"
    );
    body
}

/// `CreateVolume` with `body`, asserting it is served, and answering the minted id.
async fn create_volume(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let (status, payload) = wsi(router, "CreateVolume", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVolume must be served, got HTTP {status} with {payload}"
    );
    let id = payload["VolumeId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume reported no VolumeId: {payload}"))
        .to_string();
    assert!(
        id.starts_with("vol-"),
        "case {case:?}: expected an EBS volume id, got {id:?}"
    );
    id
}

/// The block Amazon EC2 reports for `volume_id`, or `None` when it holds no such volume.
///
/// The request names ONE volume, so the answer's `volumeSet` is that volume's own block: it
/// is read whole rather than split into entries, because a volume carrying tags nests its
/// own `<item>` entries inside it.
async fn described(router: &MultiServiceRouter, volume_id: &str) -> Option<String> {
    let (status, body) = ec2(router, "DescribeVolumes", &[("VolumeId.1", volume_id)]).await;
    if status != 200 {
        return None;
    }
    let set = block_of(&body, "volumeSet")?;
    if opt_element(set, "volumeId").as_deref() != Some(volume_id) {
        return None;
    }
    Some(set.to_string())
}

/// Look again until Amazon EC2 reports `volume_id` in one of `states`, and answer the block
/// it reported. Waits for the CONDITION, not for a span: the deadline is only the point past
/// which the condition is taken to be unreachable.
async fn settled_in(
    router: &MultiServiceRouter,
    case: &str,
    volume_id: &str,
    states: &[&str],
) -> String {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let reported = described(router, volume_id).await;
        if let Some(item) = reported.as_ref() {
            let state = opt_element(item, "status").unwrap_or_default();
            if states.contains(&state.as_str()) {
                return item.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected EC2 to report {volume_id} in one of {states:?}, it reports \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The tags Amazon EC2 reports on one volume block, as the map a client reads them as.
fn volume_tags(item: &str) -> BTreeMap<String, String> {
    let Some(set) = block_of(item, "tagSet") else {
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

/// Every volume id Amazon EC2 holds for the account, which is what a refused create must
/// leave untouched.
async fn held_volume_ids(router: &MultiServiceRouter, case: &str) -> BTreeSet<String> {
    let body = ec2_ok(router, case, "DescribeVolumes", &[]).await;
    elements(&body, "volumeId").into_iter().collect()
}

/// One create the rule is asserted over.
struct Row {
    case: &'static str,
    /// The `CreateVolume` body.
    body: fn() -> Value,
    /// The zone EC2 must report the volume in.
    zone: &'static str,
    /// The size in GiB it must report.
    size: &'static str,
    /// The `VolumeType` it must report.
    volume_type: &'static str,
    /// The performance members the request NAMED, which the volume provisions exactly: the
    /// member as EC2's wire spells it, and its value.
    named: &'static [(&'static str, &'static str)],
    /// The tags it must carry.
    tags: &'static [(&'static str, &'static str)],
}

/// The rows: one per thing the rule generalises over. Every size is inside the published
/// range of its type and every `Iops`/`Throughput` inside the published range of that member
/// for that type.
const ROWS: &[Row] = &[
    Row {
        case: "the smallest volume the default type takes",
        body: || json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 1 }),
        zone: "us-east-1a",
        size: "1",
        // "Default: gp2" (the Amazon EBS `CreateVolume$VolumeType`).
        volume_type: "gp2",
        named: &[],
        tags: &[],
    },
    Row {
        case: "a gp3 volume with the IOPS and throughput it provisions",
        body: || {
            json!({
                "AvailabilityZone": "us-east-1b",
                "SizeInGB": 20,
                "VolumeType": "gp3",
                "Iops": 4_000,
                "Throughput": 250,
            })
        },
        zone: "us-east-1b",
        size: "20",
        volume_type: "gp3",
        named: &[("iops", "4000"), ("throughput", "250")],
        tags: &[],
    },
    Row {
        case: "an io1 volume at the bottom of its published size and IOPS ranges",
        body: || {
            json!({
                "AvailabilityZone": "us-east-1c",
                "SizeInGB": 4,
                "VolumeType": "io1",
                "Iops": 100,
            })
        },
        zone: "us-east-1c",
        size: "4",
        volume_type: "io1",
        named: &[("iops", "100")],
        tags: &[],
    },
    Row {
        case: "an io2 volume carrying one tag specification",
        body: || {
            json!({
                "AvailabilityZone": "us-east-1d",
                "SizeInGB": 500,
                "VolumeType": "io2",
                "Iops": 1_000,
                "TagSpecifications": [
                    { "ResourceType": "volume", "Tags": [{ "Key": "env", "Value": "dev" }] },
                ],
            })
        },
        zone: "us-east-1d",
        size: "500",
        volume_type: "io2",
        named: &[("iops", "1000")],
        tags: &[("env", "dev")],
    },
    Row {
        case: "an st1 volume at the bottom of its published size range, with two tags",
        body: || {
            json!({
                "AvailabilityZone": "us-east-1a",
                "SizeInGB": 125,
                "VolumeType": "st1",
                "TagSpecifications": [
                    { "ResourceType": "volume", "Tags": [{ "Key": "team", "Value": "eu" }] },
                    { "Tags": [{ "Key": "owner", "Value": "alice" }] },
                ],
            })
        },
        zone: "us-east-1a",
        size: "125",
        volume_type: "st1",
        named: &[],
        tags: &[("owner", "alice"), ("team", "eu")],
    },
    Row {
        case: "an sc1 volume well above the bottom of its size range",
        body: || {
            json!({
                "AvailabilityZone": "us-east-1b",
                "SizeInGB": 1_000,
                "VolumeType": "sc1",
            })
        },
        zone: "us-east-1b",
        size: "1000",
        volume_type: "sc1",
        named: &[],
        tags: &[],
    },
    Row {
        case: "an encrypted standard volume under a named KMS key",
        body: || {
            json!({
                "AvailabilityZone": "us-east-1f",
                "SizeInGB": 1_024,
                "VolumeType": "standard",
                "Encrypted": true,
                "KmsKeyId": "alias/aws/ebs",
            })
        },
        zone: "us-east-1f",
        size: "1024",
        volume_type: "standard",
        named: &[("encrypted", "true")],
        tags: &[],
    },
];

#[tokio::test]
async fn every_volume_workspaces_instances_creates_is_an_ebs_volume_of_the_account() {
    let router = emulator();

    let mut reported: Vec<String> = Vec::new();
    for row in ROWS {
        let case = row.case;
        let volume_id = create_volume(&router, case, (row.body)()).await;
        assert!(
            !reported.contains(&volume_id),
            "case {case:?}: expected a volume of its own, got the already-reported {volume_id:?}"
        );

        // The service that OWNS EBS volumes holds it: this is the whole point of answering
        // its id. A volume is answered `creating` and reaches `available` without anyone
        // asking.
        let item = settled_in(
            &router,
            case,
            &volume_id,
            &[STATE_CREATING, STATE_AVAILABLE],
        )
        .await;
        for (member, expected) in [
            ("availabilityZone", row.zone),
            ("size", row.size),
            ("volumeType", row.volume_type),
        ]
        .into_iter()
        .chain(row.named.iter().copied())
        {
            let read = opt_element(&item, member).unwrap_or_default();
            assert_eq!(
                read, expected,
                "case {case:?}: expected EC2 to report {member} {expected:?} for {volume_id}, got \
                 {read:?} from {item}"
            );
        }
        let carried = volume_tags(&item);
        let expected: BTreeMap<String, String> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            carried, expected,
            "case {case:?}: expected the requested tags on {volume_id}, got {carried:?}"
        );

        // EC2's own clock brings it to `available`, which is what makes it a volume of the
        // account rather than a record.
        let item = settled_in(&router, case, &volume_id, &[STATE_AVAILABLE]).await;
        assert_eq!(
            opt_element(&item, "status").unwrap_or_default(),
            STATE_AVAILABLE,
            "case {case:?}: expected {volume_id} to settle {STATE_AVAILABLE}, got {item}"
        );

        // A volume of the account is one EC2 will WRITE: the id resolves on the write path
        // too, and the tag it adds lands on the volume this service created.
        ec2_ok(
            &router,
            case,
            "CreateTags",
            &[
                ("ResourceId.1", volume_id.as_str()),
                ("Tag.1.Key", "added-by-ec2"),
                ("Tag.1.Value", "1"),
            ],
        )
        .await;
        let tagged = described(&router, &volume_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: EC2 resolves no volume {volume_id:?}"));
        assert_eq!(
            volume_tags(&tagged).get("added-by-ec2").map(String::as_str),
            Some("1"),
            "case {case:?}: expected the tag EC2 added on {volume_id}, got {tagged}"
        );

        reported.push(volume_id);
    }

    // Every volume created above is still one EC2 holds, so no row displaced another.
    let held = held_volume_ids(&router, "the whole table").await;
    for volume_id in &reported {
        assert!(
            held.contains(volume_id),
            "case \"the whole table\": expected EC2 to still hold {volume_id}, it holds {held:?}"
        );
    }
}

#[tokio::test]
async fn an_ebs_workflow_runs_on_the_volume_and_the_delete_removes_it() {
    let router = emulator();

    // Rows differing in how the volume was configured and how big it is, so the snapshot
    // EC2 takes cannot pass by reporting one fixed size.
    let configurations: [(&str, Value, &str); 3] = [
        (
            "a volume of the default type",
            json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 8 }),
            "8",
        ),
        (
            "a tagged gp3 volume",
            json!({
                "AvailabilityZone": "us-east-1b",
                "SizeInGB": 30,
                "VolumeType": "gp3",
                "TagSpecifications": [
                    { "ResourceType": "volume", "Tags": [{ "Key": "k", "Value": "v" }] },
                ],
            }),
            "30",
        ),
        (
            "an io2 volume",
            json!({
                "AvailabilityZone": "us-east-1c",
                "SizeInGB": 4,
                "VolumeType": "io2",
                "Iops": 200,
            }),
            "4",
        ),
    ];

    for (case, body, size) in configurations {
        let volume_id = create_volume(&router, case, body).await;
        settled_in(&router, case, &volume_id, &[STATE_AVAILABLE]).await;

        // The cross-service workflow the id exists for: EC2 snapshots the volume this
        // service created, and the snapshot it takes is of that volume's own capacity.
        let snapshot = ec2_ok(
            &router,
            case,
            "CreateSnapshot",
            &[("VolumeId", volume_id.as_str())],
        )
        .await;
        assert_eq!(
            opt_element(&snapshot, "volumeId").as_deref(),
            Some(volume_id.as_str()),
            "case {case:?}: expected the snapshot to name the volume it was taken of, got \
             {snapshot}"
        );
        assert_eq!(
            opt_element(&snapshot, "volumeSize").unwrap_or_default(),
            size,
            "case {case:?}: expected the snapshot to carry the volume's own size, got {snapshot}"
        );

        // The delete reaches the service that owns the volume: EBS's own read resolves
        // nothing afterwards, which is what makes it a delete rather than a forgotten
        // record.
        let (status, payload) = wsi(&router, "DeleteVolume", json!({ "VolumeId": &volume_id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteVolume must be served, got HTTP {status} with {payload}"
        );
        assert!(
            described(&router, &volume_id).await.is_none(),
            "case {case:?}: expected EC2 to hold no volume {volume_id} after the delete"
        );
        let (status, refused) = ec2(&router, "DescribeVolumes", &[("VolumeId.1", &volume_id)]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected EC2 to refuse the deleted {volume_id}, got HTTP {status} with \
             {refused}"
        );
        assert_eq!(
            element(&refused, "Code"),
            VOLUME_NOT_FOUND,
            "case {case:?}: expected EC2's own code for a volume it holds none of, got {refused}"
        );

        // And this service answers its own not-found for the same id, so neither service
        // still reports the volume.
        let (status, payload) = wsi(&router, "DeleteVolume", json!({ "VolumeId": &volume_id })).await;
        assert_eq!(
            (status, wsi_error_code(&payload).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: expected a second delete to be refused as not found, got HTTP {status} \
             with {payload}"
        );
    }
}

#[tokio::test]
async fn a_create_this_service_refuses_puts_no_volume_in_the_account() {
    let router = emulator();

    // One volume the account really holds, so each refusal below is checked against an
    // inventory that is not empty: a refusal must neither add to it nor take from it.
    let live = create_volume(
        &router,
        "the volume the refusals are checked against",
        json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 10, "VolumeType": "gp3" }),
    )
    .await;
    settled_in(
        &router,
        "the volume the refusals are checked against",
        &live,
        &[STATE_AVAILABLE],
    )
    .await;
    let before = held_volume_ids(&router, "before the refusals").await;

    // Rows: one per cross-field rule the backing service applies to a create, each naming a
    // value outside the published range or table for the type the request names.
    let refusals: [(&str, Value); 7] = [
        (
            "no capacity source at all",
            json!({ "AvailabilityZone": "us-east-1a" }),
        ),
        (
            "IOPS below the published gp3 range",
            json!({
                "AvailabilityZone": "us-east-1a",
                "SizeInGB": 10,
                "VolumeType": "gp3",
                "Iops": 100,
            }),
        ),
        (
            "throughput above the published gp3 range",
            json!({
                "AvailabilityZone": "us-east-1a",
                "SizeInGB": 10,
                "VolumeType": "gp3",
                "Throughput": 2_001,
            }),
        ),
        (
            "an io1 volume with no provisioned IOPS",
            json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 10, "VolumeType": "io1" }),
        ),
        (
            "a size below the published sc1 range",
            json!({ "AvailabilityZone": "us-east-1a", "SizeInGB": 124, "VolumeType": "sc1" }),
        ),
        (
            "a size above the published standard range",
            json!({
                "AvailabilityZone": "us-east-1a",
                "SizeInGB": 1_025,
                "VolumeType": "standard",
            }),
        ),
        (
            "a KMS key on an unencrypted volume",
            json!({
                "AvailabilityZone": "us-east-1a",
                "SizeInGB": 10,
                "VolumeType": "gp3",
                "KmsKeyId": "alias/aws/ebs",
            }),
        ),
    ];

    for (case, body) in refusals {
        let (status, payload) = wsi(&router, "CreateVolume", body).await;
        assert_eq!(
            (status, wsi_error_code(&payload).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a refused create, got HTTP {status} with {payload}"
        );
        assert_eq!(
            payload.get("VolumeId"),
            None,
            "case {case:?}: a refused create must report no volume id, got {payload}"
        );
        let after = held_volume_ids(&router, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused create must leave the account's volumes exactly as they \
             were"
        );
    }
}
