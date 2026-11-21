// SPDX-License-Identifier: CC0-1.0

//! `GetDeployablePatchSnapshotForInstance` end to end: the patch snapshot a managed node
//! deploys its patch baseline from.
//!
//! RULE under test: this read is served to the managed node it NAMES and to nobody else — the
//! caller has to be that node, which it is when the principal it signed with is a session of
//! the node's registration role named for the node (the credentials that node's guest is
//! served), and every other principal, a client's own credentials included, is refused the
//! modelled `AccessDeniedException` carrying IAM's own sentence and writing nothing. The node
//! itself is answered the snapshot of the patch baseline in force for it — the id it asked
//! with, the product of its own operating system release, and a URL of this emulator the
//! snapshot is downloadable from — which is FROZEN at the instant it was first taken, so a
//! later read of the same snapshot id is answered what the first one was even after the
//! baseline in force has changed; an override naming another operating system than the node's
//! is refused `UnsupportedOperatingSystem`.
//!
//! Every case goes through the front door a client and a node's agent use — the shared harness
//! of `all/ec2_instance_managed_nodes.rs`, which mounts Systems Manager, Amazon EC2, IAM and
//! AWS STS on one engine — with the role, its policy and its trust written over IAM's own wire
//! and the session minted by STS's own `AssumeRole`, so the identity this service reads is the
//! one those services recorded.
//!
//! The rows differ along what each rule generalises over: which principal signs the call (the
//! account's own published credential, a session of another role named for the node, a session
//! of the node's role named for another node, the node's own session), and which release of
//! which operating system the node is of (a Linux node and a Windows node launched from the
//! images Amazon publishes, and nodes reporting their platform from inside the guest or
//! reporting none at all).

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE, RESOURCE_TYPE_MANAGED_INSTANCE,
};

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};

/// The name pattern a client resolving the current Windows Server 2022 AMI matches on, as the
/// Amazon EC2 catalogue publishes that image's name.
const WINDOWS_NAME_PATTERN: &str = "Windows_Server-2022-English-Full-Base*";

/// The access key id the account's own published credential presents: one neither STS nor IAM
/// holds a record for, which resolves to the account's own identity.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// A snapshot id of the form the model requires (`len 36..36`, the UUID pattern), as the
/// caller running `AWS-RunPatchBaseline` supplies one.
const SNAPSHOT: &str = "11111111-2222-3333-4444-555555555555";

/// A second one, for the case about a snapshot taken after the baseline in force changed.
const LATER_SNAPSHOT: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

/// A node id the account holds no node of — the shape the modelled `InstanceId` pattern
/// admits, so it reaches the handler.
const UNHELD_NODE: &str = "i-0123456789abcdef0";

/// When a node this suite SEEDS registered, as its record holds it.
const SEEDED_REGISTRATION_SECS: u64 = 1_700_000_000;

/// One row of the reported-platform table: a node seeded with the platform its own agent
/// reports from inside the guest, and the product a snapshot of that node names.
struct ReportedNode {
    /// What the case is.
    case: &'static str,
    /// The node's id.
    node_id: &'static str,
    /// `InstanceInformation$PlatformType`.
    platform_type: &'static str,
    /// `InstanceProperty$PlatformName`, for a node that reports one.
    platform_name: Option<&'static str>,
    /// `InstanceProperty$PlatformVersion`, for a node that reports one.
    platform_version: Option<&'static str>,
    /// The product a snapshot of that node must name.
    product: &'static str,
}

/// A trust policy that lets the account's own principals assume the role: what makes a
/// session of a node's own registration role obtainable in a test, the way that node's guest
/// is served one.
fn account_trust() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"arn:aws:iam::{}:root\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}",
        engine::emulator_account_id()
    )
}

/// The same, for the role an instance also RUNS AS: Amazon EC2 has to be able to assume it or
/// the node never registers, and the account has to be able to assume it or no case can
/// present the credentials that node's guest is served.
fn node_role_trust() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Principal\":{{\"Service\":\"ec2.amazonaws.com\"}},\
         \"Action\":\"sts:AssumeRole\"}},\
         {{\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{}:root\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}",
        engine::emulator_account_id()
    )
}

/// An identity-based policy that allows every action of this service, so a case's refusal is
/// never IAM's default deny: what is under test is WHO the caller is.
const ALLOW_SSM: &str = r#"{"Version":"2012-10-17","Statement":[
    {"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#;

/// What a principal a case signs as is: the access key id it presents, and the ARN a refusal
/// must blame it by — what `sts:GetCallerIdentity` reports for that key.
struct Principal {
    access_key_id: String,
    arn: String,
}

impl Principal {
    /// The account's own published credential, which is what a client's local credentials are
    /// here: the identity it speaks for is read LIVE from the same place a refusal blames it
    /// by (`sts:GetCallerIdentity`), rather than spelled out here.
    async fn account(emulator: &Emulator, case: &str) -> Self {
        let answered = emulator.sts(case, "GetCallerIdentity", &[]).await;
        Self {
            access_key_id: ACCOUNT_KEY.to_string(),
            arn: Emulator::answered(&answered, "Arn").unwrap_or_else(|| {
                panic!("case {case:?}: GetCallerIdentity answers an Arn: {answered}")
            }),
        }
    }
}

/// Create a role assumable by the account and allowed every action of this service.
async fn role(emulator: &Emulator, case: &str, role_name: &str) {
    emulator
        .iam(
            case,
            "CreateRole",
            &[
                ("RoleName", role_name),
                ("AssumeRolePolicyDocument", &account_trust()),
            ],
        )
        .await;
    emulator
        .iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", "inline"),
                ("PolicyDocument", ALLOW_SSM),
            ],
        )
        .await;
}

/// A session of `role_name` under `session_name`: the credential a call is signed with, and
/// the identity it speaks for.
///
/// An Amazon EC2 node's agent holds exactly this — "a session of that role named for the
/// instance" (`all/guest_instance_profile_credentials.rs`) — so a session whose name is a node
/// id is that node's own identity.
async fn session_of(
    emulator: &Emulator,
    case: &str,
    role_name: &str,
    session_name: &str,
) -> Principal {
    let assumed = emulator
        .sts(
            case,
            "AssumeRole",
            &[
                (
                    "RoleArn",
                    &format!(
                        "arn:aws:iam::{}:role/{role_name}",
                        engine::emulator_account_id()
                    ),
                ),
                ("RoleSessionName", session_name),
            ],
        )
        .await;
    Principal {
        access_key_id: Emulator::answered(&assumed, "AccessKeyId").unwrap_or_else(|| {
            panic!("case {case:?}: AssumeRole answers an AccessKeyId: {assumed}")
        }),
        arn: Emulator::answered(&assumed, "Arn").unwrap_or_else(|| {
            panic!("case {case:?}: AssumeRole answers the session Arn: {assumed}")
        }),
    }
}

/// Launch an instance from the image `pattern` resolves, running as an instance profile whose
/// role Systems Manager may manage, and answer the node it registered as.
///
/// The role is also made assumable by the account, so a case can present the credentials that
/// node's guest is served: a session of the role it registered as, named for it.
async fn registered_node(emulator: &Emulator, case: &str, name: &str, pattern: &str) -> String {
    let image = emulator.image(case, pattern).await;
    let profile = emulator
        .profile(case, name, &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    emulator
        .iam(
            case,
            "UpdateAssumeRolePolicy",
            &[("RoleName", name), ("PolicyDocument", &node_role_trust())],
        )
        .await;
    let instance = emulator.launch(case, &image, Some(&profile)).await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    assert!(
        reported.contains_key(&instance),
        "case {case:?}: the launched instance must become a managed node first, got {:?}",
        reported.keys().collect::<Vec<_>>()
    );
    instance
}

/// One `GetDeployablePatchSnapshotForInstance`, signed with `principal`'s key.
async fn snapshot_as(
    emulator: &Emulator,
    principal: &Principal,
    instance_id: &str,
    snapshot_id: &str,
    baseline_override: Option<Value>,
) -> (u16, Value) {
    let mut body = json!({ "InstanceId": instance_id, "SnapshotId": snapshot_id });
    if let Some(overridden) = baseline_override {
        body["BaselineOverride"] = overridden;
    }
    emulator
        .ssm_as(
            &principal.access_key_id,
            REGION,
            "GetDeployablePatchSnapshotForInstance",
            &body,
        )
        .await
}

/// The wire error code of a failure body, as a client branches on it.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .map(|named| named.rsplit('#').next().unwrap_or(named).to_string())
        .unwrap_or_default()
}

/// The ARN of the EC2 instance `instance_id` names — the resource a refusal of a call about
/// that node names.
fn instance_arn(instance_id: &str) -> String {
    format!(
        "arn:aws:ec2:{REGION}:{}:instance/{instance_id}",
        engine::emulator_account_id()
    )
}

/// The sentence a caller that is not the node it named is refused with: IAM's own, naming the
/// principal that signed, the action and the node it asked about.
fn not_authorized(principal: &Principal, instance_id: &str) -> String {
    format!(
        "User: {} is not authorized to perform: \
         ssm:GetDeployablePatchSnapshotForInstance on resource: {}",
        principal.arn,
        instance_arn(instance_id)
    )
}

/// The same address with its last path segment — the key component this endpoint minted for
/// the snapshot — replaced by `other`: what a request that guessed the Region, the node and
/// the snapshot id, and was never handed the URL, addresses.
fn with_another_object_key(url: &str, other: &str) -> String {
    let (address, query) = url.split_once('?').unwrap_or((url, ""));
    let base = address.rsplit_once('/').map_or(address, |(head, _)| head);
    format!("{base}/{other}?{query}")
}

/// How many snapshots this emulator holds — what shows that a refused read took none.
fn snapshots_held(emulator: &Emulator) -> usize {
    emulator.ssm_state().lock().patch_snapshots.len()
}

/// RULE: the snapshot is served to the node that asks for its own, and refused to every other
/// principal, which takes no snapshot at all.
#[tokio::test]
async fn a_snapshot_is_served_to_the_node_itself_and_refused_to_every_other_principal() {
    let case = "the principal table";
    let emulator = Emulator::new();
    let node = registered_node(&emulator, case, "snapshot-node", AL2023_NAME_PATTERN).await;
    role(&emulator, case, "snapshot-other-role").await;

    // (what the case is, the principal, whether the read is served)
    let principals: Vec<(&str, Principal, bool)> = vec![
        (
            "the account's own published credential, as a client's local credentials are",
            Principal::account(&emulator, case).await,
            false,
        ),
        (
            "a session of ANOTHER role, named for the node",
            session_of(&emulator, case, "snapshot-other-role", &node).await,
            false,
        ),
        (
            "a session of the node's own role, named for another node",
            session_of(&emulator, case, "snapshot-node", "some-other-session").await,
            false,
        ),
        (
            "the node's own credentials: a session of its role, named for it",
            session_of(&emulator, case, "snapshot-node", &node).await,
            true,
        ),
    ];

    for (why, principal, served) in principals {
        let before = snapshots_held(&emulator);
        let (status, answered) =
            snapshot_as(&emulator, &principal, &node, SNAPSHOT, None).await;
        let after = snapshots_held(&emulator);
        if served {
            assert_eq!(
                status, 200,
                "case {why:?}: the node's own read must be served, got {status}: {answered}"
            );
            assert_eq!(
                answered["InstanceId"].as_str(),
                Some(node.as_str()),
                "case {why:?}: the answer names the node it was taken for, got {answered}"
            );
            assert_eq!(
                answered["SnapshotId"].as_str(),
                Some(SNAPSHOT),
                "case {why:?}: the answer names the snapshot id asked for, got {answered}"
            );
            assert_eq!(
                after,
                before + 1,
                "case {why:?}: the served read holds the snapshot it took, got {after} after \
                 {before}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "AccessDeniedException"),
                "case {why:?}: a caller that is not the node must be refused, got {status}: \
                 {answered}"
            );
            assert_eq!(
                answered["Message"].as_str().unwrap_or_default(),
                not_authorized(&principal, &node),
                "case {why:?}: the refusal names the principal, the action and the node, got \
                 {answered}"
            );
            assert_eq!(
                after, before,
                "case {why:?}: a refused read takes no snapshot, got {after} after {before}"
            );
        }
    }
}

/// RULE: a node asking about a node the account does not hold is refused the same way, and
/// learns nothing else about what the account holds.
#[tokio::test]
async fn a_read_of_a_node_the_account_does_not_hold_is_refused() {
    let case = "a node that is not registered";
    let emulator = Emulator::new();
    role(&emulator, case, "snapshot-unheld-role").await;
    let principal = session_of(&emulator, case, "snapshot-unheld-role", UNHELD_NODE).await;

    let (status, answered) =
        snapshot_as(&emulator, &principal, UNHELD_NODE, SNAPSHOT, None).await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "AccessDeniedException"),
        "case {case:?}: expected the read to be refused, got {status}: {answered}"
    );
    assert_eq!(
        answered["Message"].as_str().unwrap_or_default(),
        not_authorized(&principal, UNHELD_NODE),
        "case {case:?}: the refusal names the principal, the action and the node, got {answered}"
    );
    assert_eq!(
        snapshots_held(&emulator),
        0,
        "case {case:?}: a refused read takes no snapshot"
    );
}

/// RULE: the snapshot the node is answered is DOWNLOADABLE from the URL it carries, holds the
/// baseline in force for that node, and is frozen at the instant it was taken — a snapshot
/// taken after the default baseline changed holds the new one, and the earlier snapshot id is
/// still answered and still serves the earlier one.
#[tokio::test]
async fn a_snapshot_is_downloadable_and_frozen_at_the_instant_it_was_taken() {
    let case = "the snapshot a node downloads";
    let emulator = Emulator::new();
    let node = registered_node(&emulator, case, "snapshot-download", AL2023_NAME_PATTERN).await;
    let agent = session_of(&emulator, case, "snapshot-download", &node).await;

    let (status, answered) = snapshot_as(&emulator, &agent, &node, SNAPSHOT, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the node's own read must be served, got {status}: {answered}"
    );
    let url = answered["SnapshotDownloadUrl"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    // The URL leads back to this emulator and is presigned: it carries the SigV4 query fields
    // a presigned URL carries, and the path the snapshot of this node is addressed at.
    for expected in [
        "X-Amz-Signature",
        &format!("patch-baseline-snapshot-{REGION}"),
        node.as_str(),
        SNAPSHOT,
    ] {
        assert!(
            url.contains(expected),
            "case {case:?}: expected the download URL to carry {expected:?}, got {url:?}"
        );
    }

    let (status, document) = emulator.fetch(&url).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the announced URL must serve the snapshot, got {status}: {document}"
    );
    let first: Value = serde_json::from_str(&document).unwrap_or_else(|error| {
        panic!("case {case:?}: the snapshot is a JSON document ({error}): {document}")
    });
    assert_eq!(
        (
            first["snapshotId"].as_str(),
            first["instanceId"].as_str(),
            first["product"].as_str()
        ),
        (
            Some(SNAPSHOT),
            Some(node.as_str()),
            answered["Product"].as_str()
        ),
        "case {case:?}: the snapshot names what the read reported, got {first}"
    );
    assert_eq!(
        first["patchBaseline"]["operatingSystem"].as_str(),
        Some("AMAZON_LINUX_2023"),
        "case {case:?}: the snapshot holds the baseline of the node's operating system, got \
         {first}"
    );
    let patches = first["patches"].as_array().map_or(0, Vec::len);
    assert!(
        patches > 0,
        "case {case:?}: the snapshot holds the patches its baseline makes effective, got {first}"
    );

    // The object is reachable through the key this endpoint minted for it and handed to the
    // node, and through nothing else: a request that guessed the node and the snapshot id
    // addresses no object.
    let guessed = with_another_object_key(&url, "9f8e7d6c-5b4a-3928-1706-0f1e2d3c4b5a");
    let (status, refused) = emulator.fetch(&guessed).await;
    assert_eq!(
        status, 404,
        "case {case:?}: an address this endpoint never handed out must serve no snapshot, got \
         {status}: {refused}"
    );
    assert!(
        !refused.contains(SNAPSHOT) && refused.contains("NoSuchKey"),
        "case {case:?}: expected the no-such-key refusal and nothing of the snapshot, got \
         {refused}"
    );

    // The account replaces the baseline in force for that operating system.
    let (status, created) = emulator
        .ssm(
            REGION,
            "CreatePatchBaseline",
            &json!({
                "Name": "snapshot-replacement",
                "OperatingSystem": "AMAZON_LINUX_2023",
                "ApprovedPatches": ["kernel-5.10.184-175.731.amzn2023"],
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePatchBaseline must succeed, got {status}: {created}"
    );
    let baseline_id = created["BaselineId"].as_str().unwrap_or_default().to_string();
    let (status, registered) = emulator
        .ssm(
            REGION,
            "RegisterDefaultPatchBaseline",
            &json!({ "BaselineId": baseline_id }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RegisterDefaultPatchBaseline must succeed, got {status}: {registered}"
    );

    // The snapshot already taken is unchanged — it is frozen — and the same read of it is
    // answered the same thing.
    let (status, again) = snapshot_as(&emulator, &agent, &node, SNAPSHOT, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a second read of the same snapshot must be served, got {status}: {again}"
    );
    let (_, document) = emulator.fetch(&url).await;
    let held: Value = serde_json::from_str(&document).unwrap_or(Value::Null);
    assert_eq!(
        held, first,
        "case {case:?}: a snapshot is frozen at the instant it was taken, got {held} after \
         {first}"
    );

    // A snapshot taken NOW is of the baseline that is in force now.
    let (status, answered) = snapshot_as(&emulator, &agent, &node, LATER_SNAPSHOT, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a later snapshot must be served, got {status}: {answered}"
    );
    let (status, document) = emulator
        .fetch(answered["SnapshotDownloadUrl"].as_str().unwrap_or_default())
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the later snapshot must be served at its URL, got {status}: {document}"
    );
    let later: Value = serde_json::from_str(&document).unwrap_or(Value::Null);
    assert_eq!(
        later["patchBaseline"]["baselineId"].as_str(),
        Some(baseline_id.as_str()),
        "case {case:?}: a snapshot taken now is of the baseline in force now, got {later}"
    );
    assert_eq!(
        later["patchBaseline"]["approvedPatches"],
        json!(["kernel-5.10.184-175.731.amzn2023"]),
        "case {case:?}: the snapshot holds what the baseline approves, got {later}"
    );
}

/// RULE: a `BaselineOverride` is the baseline the snapshot is taken of, and one that applies
/// to another operating system than the node's cannot be deployed on that node at all.
#[tokio::test]
async fn an_override_decides_the_baseline_and_one_of_another_operating_system_is_refused() {
    let case = "the override";
    let emulator = Emulator::new();
    let node = registered_node(&emulator, case, "snapshot-override", AL2023_NAME_PATTERN).await;
    let agent = session_of(&emulator, case, "snapshot-override", &node).await;

    // An override of the node's OWN operating system: the snapshot is of it rather than of the
    // baseline in force.
    let (status, answered) = snapshot_as(
        &emulator,
        &agent,
        &node,
        SNAPSHOT,
        Some(json!({
            "OperatingSystem": "AMAZON_LINUX_2023",
            "ApprovedPatches": ["kernel-6.1.55-75.123.amzn2023"],
            "ApprovedPatchesComplianceLevel": "HIGH",
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: an override of the node's own operating system must be served, got \
         {status}: {answered}"
    );
    let (status, document) = emulator
        .fetch(answered["SnapshotDownloadUrl"].as_str().unwrap_or_default())
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the overridden snapshot must be served at its URL, got {status}: \
         {document}"
    );
    let overridden: Value = serde_json::from_str(&document).unwrap_or(Value::Null);
    assert_eq!(
        (
            overridden["patchBaseline"]["approvedPatches"].clone(),
            overridden["patchBaseline"]["approvedPatchesComplianceLevel"].as_str(),
        ),
        (
            json!(["kernel-6.1.55-75.123.amzn2023"]),
            Some("HIGH")
        ),
        "case {case:?}: the snapshot is of the baseline the request overrode with, got \
         {overridden}"
    );

    // An override of ANOTHER operating system: nothing of it is deployable on this node.
    let (status, answered) = snapshot_as(
        &emulator,
        &agent,
        &node,
        LATER_SNAPSHOT,
        Some(json!({
            "OperatingSystem": "WINDOWS",
            "ApprovedPatches": ["KB2124261"],
        })),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "UnsupportedOperatingSystem"),
        "case {case:?}: an override of another operating system must be refused, got {status}: \
         {answered}"
    );
    assert_eq!(
        answered["Message"].as_str().unwrap_or_default(),
        "The operation isn't supported for the operating system WINDOWS.",
        "case {case:?}: the refusal names the operating system it was asked about, got \
         {answered}"
    );
}

/// RULE: the `Product` a snapshot names is the published product of the node's own operating
/// system that the node's release names — read off the platform the node reports from inside
/// its guest, or off the image it was launched from — and the newest product published for
/// that operating system where the node's release names none.
///
/// The published products come from Patch Manager's own catalogue
/// (`state::patch_properties::PUBLISHED_PATCH_METADATA`, the operating-system patch sets).
#[tokio::test]
async fn a_snapshot_names_the_product_of_the_node_it_was_taken_for() {
    // (what the case is, the image a node is launched from, the product the snapshot names)
    let launched: &[(&str, &str, &str)] = &[
        (
            "a node launched from the published Amazon Linux 2023 image",
            AL2023_NAME_PATTERN,
            "AmazonLinux2023",
        ),
        (
            "a node launched from the published Windows Server 2022 image",
            WINDOWS_NAME_PATTERN,
            "WindowsServer2022",
        ),
    ];
    for (case, pattern, product) in launched {
        let emulator = Emulator::new();
        let node = registered_node(&emulator, case, "snapshot-product", pattern).await;
        let agent = session_of(&emulator, case, "snapshot-product", &node).await;
        let (status, answered) = snapshot_as(&emulator, &agent, &node, SNAPSHOT, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the node's own read must be served, got {status}: {answered}"
        );
        assert_eq!(
            answered["Product"].as_str(),
            Some(*product),
            "case {case:?}: expected the product {product:?}, got {answered}"
        );
    }

    // Nodes whose release is what their AGENT reports from inside the guest, and nodes that
    // report none at all. Seeded through the service's own node store, because a node enters
    // it only by registering and no operation of this service registers a hybrid one.
    let reported: &[ReportedNode] = &[
        ReportedNode {
            case: "a Windows node reporting its own release",
            node_id: "i-0aaaaaaa",
            platform_type: "Windows",
            platform_name: Some("Microsoft Windows Server"),
            platform_version: Some("2019"),
            product: "WindowsServer2019",
        },
        ReportedNode {
            case: "a Windows node reporting no release at all",
            node_id: "i-0bbbbbbb",
            platform_type: "Windows",
            platform_name: None,
            platform_version: None,
            product: "WindowsServer2025",
        },
        ReportedNode {
            case: "a hybrid macOS node reporting its own release",
            node_id: "mi-01234567890123456",
            platform_type: "MacOS",
            platform_name: Some("macOS"),
            platform_version: Some("14.0"),
            product: "MacOS14.0",
        },
        ReportedNode {
            case: "a Linux node with no image this endpoint can read",
            node_id: "i-0ccccccc",
            platform_type: "Linux",
            platform_name: Some("Amazon Linux"),
            platform_version: Some("2023"),
            product: "AmazonLinux2023",
        },
    ];
    let emulator = Emulator::new();
    let case = "the reported-platform table";
    role(&emulator, case, "snapshot-reported-role").await;
    for row in reported {
        let resource_type = if row.node_id.starts_with("mi-") {
            RESOURCE_TYPE_MANAGED_INSTANCE
        } else {
            RESOURCE_TYPE_EC2_INSTANCE
        };
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(SEEDED_REGISTRATION_SECS);
        let mut record = ManagedNodeRecord::registering(row.node_id, resource_type, at);
        record.properties.platform_type = Some(row.platform_type.to_string());
        record.properties.platform_name = row.platform_name.map(str::to_string);
        record.properties.platform_version = row.platform_version.map(str::to_string);
        assert!(
            emulator
                .ssm_state()
                .lock()
                .insert_managed_node(REGION, record),
            "case {:?}: a seeded node must carry the node id it is stored under",
            row.case
        );
    }
    for row in reported {
        let agent = session_of(&emulator, row.case, "snapshot-reported-role", row.node_id).await;
        let (status, answered) =
            snapshot_as(&emulator, &agent, row.node_id, SNAPSHOT, None).await;
        assert_eq!(
            status,
            200,
            "case {:?}: the node's own read must be served, got {status}: {answered}",
            row.case
        );
        assert_eq!(
            answered["Product"].as_str(),
            Some(row.product),
            "case {:?}: expected the product {:?}, got {answered}",
            row.case,
            row.product
        );
    }

    // The nodes are the account's own records, so nothing above invented one.
    assert_eq!(
        emulator
            .ssm_state()
            .lock()
            .managed_nodes_in_region(REGION)
            .count(),
        reported.len(),
        "case {case:?}: the seeded nodes are the ones this Region holds"
    );
}
