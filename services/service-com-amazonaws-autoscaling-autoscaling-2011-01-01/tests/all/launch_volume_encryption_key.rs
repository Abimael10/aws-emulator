// SPDX-License-Identifier: CC0-1.0

//! The KMS key the ROOT VOLUME of an Auto Scaling group's instances is encrypted under
//! decides whether a capacity change can place an instance at all.
//!
//! RULE under test: a capacity change launches from the launch-template version its
//! specification names AT THE INSTANT OF THE LAUNCH, and an encrypted volume that version
//! describes is created by asking KMS for the volume's data key — so when a key the version
//! names is in any key state but `Enabled`, or names no key of the account and Region at
//! all, no volume is created, the instance never reaches `InService`, and the capacity
//! change's terminal scaling activity is `Cancelled` carrying the Amazon EC2 reason for a
//! key that will not serve. A key that WILL serve launches the instance and the activity is
//! `Successful`.
//!
//! Every customer managed key here is created with the group's service-linked role AUTHORIZED
//! on it ([`authorize_role_on_key`]), so what each row varies is the key's own state and
//! nothing about the authorities over it; whether those authorities let the launch's role use
//! the key is the other half of the same rule, held to its own table in
//! `launch_volume_key_authorization`.
//!
//! The rows span what the rule generalises over: the key state (a disabled key, a key
//! scheduled for deletion, a well-formed key ARN of no key, an enabled key), how the version
//! SPELLS the key (its key id, its key ARN, the Amazon Web Services managed alias of Amazon
//! EBS, no key at all), which block device mapping of the version names it (the root device
//! or another one), and how the group names the version (`$Default`, `$Latest`, a literal
//! number). The last test is the rebinding the rule turns on: the same group, launching
//! through `$Default`, is cancelled while that word names a version under a disabled key and
//! launches once the word names a version that names no customer key — the group's own
//! stored `Version` literal never changing.
//!
//! The three services are mounted on ONE engine and driven over their own wire protocols —
//! awsQuery for Auto Scaling, ec2Query for Amazon EC2, awsJson1.1 for KMS — so every key,
//! template and activity is written and read where a client writes and reads it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;
use service_com_amazonaws_kms_trentservice as kms_service;

use crate::support::{
    borrowed, default_service_linked_role_arn, ec2_query, json_rpc, list_members, p, query,
    tag_value, tag_values, xml_unescape, REGION,
};

/// The AMI every launch template in this suite names, in the long id form Amazon EC2 issues
/// one in. The account holds no image for it, so the root device facts a launch falls back to
/// are Amazon EC2's own.
const IMAGE_ID: &str = "ami-00000000000000091";

/// The instance type every launch template in this suite names.
const INSTANCE_TYPE: &str = "t3.micro";

/// The device a row's first block device mapping names, and the second device a row that
/// names its key on another volume uses.
pub(crate) const ROOT_DEVICE: &str = "/dev/xvda";
const SECOND_DEVICE: &str = "/dev/sdb";

/// How long a case keeps looking for the outcome it waits on before it gives up — generous
/// on purpose, so a loaded host is never the reason a case fails.
pub(crate) const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
pub(crate) const POLL_PAUSE: Duration = Duration::from_millis(25);

/// The status codes a scaling activity has FINISHED in, from the model's own
/// `ScalingActivityStatusCode`: a client polling for the outcome of a capacity change stops
/// at one of these.
const TERMINAL_STATUS_CODES: &[&str] = &["Successful", "Failed", "Cancelled"];

/// The alias of the Amazon Web Services managed key Amazon EBS encrypts a volume with when
/// the volume names none.
///
/// SOURCE: an Amazon Web Services alias has "the format `aws/<service-name>`"
/// (`smithy_query(query="doc",shape="ListAliases"` in the KMS model), under the name Amazon
/// EBS carries — the key `CreateVolumeRequest$KmsKeyId` calls "your KMS key for Amazon EBS".
/// Written out here rather than taken from the service's own constant: a case that re-used
/// the implementation's literal would assert nothing about it.
pub(crate) const EBS_MANAGED_KEY_ALIAS: &str = "alias/aws/ebs";

/// A key ARN of this account and Region whose key id names no key at all: well formed, so it
/// is a reference KMS resolves rather than refuses as malformed, and resolving it finds
/// nothing.
fn absent_key_arn() -> String {
    format!(
        "arn:aws:kms:{REGION}:{}:key/00000000-1111-2222-3333-444444444444",
        ::engine::emulator_account_id()
    )
}

/// The emulator with the three services this suite exercises on ONE engine: Auto Scaling
/// holds the group, Amazon EC2 the launch template and the instances, KMS the key.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kms_service::EmulatedService::new(engine));
    router
}

/// The zone every group here places its instances in: the first of the Region's own catalog,
/// never written out.
pub(crate) fn zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has Availability Zones")
}

/// Call one KMS operation over its awsJson1.1 wire and answer the parsed body of a call that
/// must have succeeded.
pub(crate) async fn kms(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answered) = json_rpc(router, "kms", "TrentService", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} expected 200, got {status}: {answered}"
    );
    answered
}

/// Create one symmetric customer managed key and answer its `(key id, key ARN)`, as KMS
/// minted them.
pub(crate) async fn create_key(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let answered = kms(router, case, "CreateKey", json!({})).await;
    let metadata = answered.get("KeyMetadata").cloned().unwrap_or(Value::Null);
    let key_id = metadata
        .get("KeyId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let arn = metadata
        .get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !key_id.is_empty() && !arn.is_empty(),
        "case {case:?}: CreateKey must report the key's id and ARN, got {answered}"
    );
    (key_id, arn)
}

/// The key state KMS reports for `key_id`, read where a client reads it.
async fn key_state(router: &MultiServiceRouter, case: &str, key_id: &str) -> String {
    let answered = kms(router, case, "DescribeKey", json!({ "KeyId": key_id })).await;
    answered
        .get("KeyMetadata")
        .and_then(|metadata| metadata.get("KeyState"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The one key policy a KMS key carries: "the only valid name is `default`"
/// (`smithy_query(query="doc",shape="PutKeyPolicyRequest",member="PolicyName")`).
pub(crate) const DEFAULT_KEY_POLICY_NAME: &str = "default";

/// The actions AWS documents for an Auto Scaling group's service-linked role on a customer
/// managed key its instances' volumes are encrypted under: the cryptographic set Amazon EBS
/// performs through the role, beside the grant it creates for the volume.
///
/// SOURCE: the Amazon EC2 Auto Scaling User Guide's required key policy for an encrypted
/// volume — "kms:Decrypt", "kms:GenerateDataKey*", "kms:DescribeKey", "kms:ReEncrypt*" in one
/// statement and "kms:CreateGrant" in another, which
/// [`service_linked_role_key_policy`] writes out.
pub(crate) const SERVICE_LINKED_ROLE_KEY_ACTIONS: &[&str] = &[
    "kms:Decrypt",
    "kms:GenerateDataKey*",
    "kms:DescribeKey",
    "kms:ReEncrypt*",
];

/// The key policy that authorizes `role_arn` to use the key for an encrypted volume, beside
/// the account's own root: the default statement a key is created with, and the two statements
/// AWS documents for an Auto Scaling group's service-linked role.
pub(crate) fn service_linked_role_key_policy(role_arn: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "Enable IAM User Permissions",
                "Effect": "Allow",
                "Principal": {
                    "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id())
                },
                "Action": "kms:*",
                "Resource": "*"
            },
            {
                "Sid": "Allow use of the key",
                "Effect": "Allow",
                "Principal": { "AWS": role_arn },
                "Action": SERVICE_LINKED_ROLE_KEY_ACTIONS,
                "Resource": "*"
            },
            {
                "Sid": "Allow attachment of persistent resources",
                "Effect": "Allow",
                "Principal": { "AWS": role_arn },
                "Action": "kms:CreateGrant",
                "Resource": "*",
                "Condition": { "Bool": { "kms:GrantIsForAWSResource": "true" } }
            }
        ]
    })
}

/// Attach `policy` to the key `key_id` as its key policy, through the service that owns the
/// key.
pub(crate) async fn put_key_policy(
    router: &MultiServiceRouter,
    case: &str,
    key_id: &str,
    policy: &Value,
) {
    kms(
        router,
        case,
        "PutKeyPolicy",
        json!({
            "KeyId": key_id,
            "PolicyName": DEFAULT_KEY_POLICY_NAME,
            "Policy": policy.to_string(),
        }),
    )
    .await;
}

/// Authorize the Auto Scaling service-linked role `role_arn` on the key `key_id`, so that the
/// authorities over the key are not what a case about the key's STATE is varying.
pub(crate) async fn authorize_role_on_key(
    router: &MultiServiceRouter,
    case: &str,
    key_id: &str,
    role_arn: &str,
) {
    put_key_policy(
        router,
        case,
        key_id,
        &service_linked_role_key_policy(role_arn),
    )
    .await;
}

/// What a row's version names as the `KmsKeyId` of its encrypted volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// A customer managed key disabled through `kms:DisableKey`, named by its key id.
    DisabledById,
    /// The same key, named by the key ARN KMS minted for it.
    DisabledByArn,
    /// A customer managed key scheduled for deletion through `kms:ScheduleKeyDeletion`.
    PendingDeletion,
    /// A well-formed key ARN of this account and Region that names no key.
    Absent,
    /// A customer managed key left as `kms:CreateKey` made it, named by its key id.
    EnabledById,
    /// The same key, named by its key ARN.
    EnabledByArn,
    /// The Amazon Web Services managed key of Amazon EBS, named by its alias: the key an
    /// encrypted volume that names none is encrypted under.
    EbsManagedAlias,
    /// No key at all — an encrypted volume naming none.
    Unnamed,
}

impl Key {
    /// The key state a client must be able to read before the launch, for a row whose key is
    /// one KMS holds: the precondition the case rests on, asserted from the emulator's own
    /// state rather than assumed.
    fn observable_state(self) -> Option<&'static str> {
        match self {
            Self::DisabledById | Self::DisabledByArn => Some("Disabled"),
            Self::PendingDeletion => Some("PendingDeletion"),
            Self::EnabledById | Self::EnabledByArn => Some("Enabled"),
            Self::Absent | Self::EbsManagedAlias | Self::Unnamed => None,
        }
    }

    /// Make the key this row names in KMS and answer the reference the launch template
    /// carries for it, or `None` for a row that names no key.
    ///
    /// A customer managed key is created with the Auto Scaling service-linked role
    /// `role_arn` authorized on it ([`authorize_role_on_key`]), so that what a row varies is
    /// the key's own STATE and not whether the launch's role may use it.
    async fn reference(
        self,
        router: &MultiServiceRouter,
        case: &str,
        role_arn: &str,
    ) -> Option<String> {
        let (reference, key_id) = match self {
            Self::Unnamed => return None,
            Self::EbsManagedAlias => return Some(EBS_MANAGED_KEY_ALIAS.to_string()),
            Self::Absent => return Some(absent_key_arn()),
            Self::DisabledById | Self::DisabledByArn => {
                let (key_id, arn) = create_key(router, case).await;
                authorize_role_on_key(router, case, &key_id, role_arn).await;
                kms(router, case, "DisableKey", json!({ "KeyId": &key_id })).await;
                let reference = if self == Self::DisabledById {
                    key_id.clone()
                } else {
                    arn
                };
                (reference, key_id)
            }
            Self::PendingDeletion => {
                let (key_id, _) = create_key(router, case).await;
                authorize_role_on_key(router, case, &key_id, role_arn).await;
                kms(
                    router,
                    case,
                    "ScheduleKeyDeletion",
                    json!({ "KeyId": &key_id, "PendingWindowInDays": 7 }),
                )
                .await;
                (key_id.clone(), key_id)
            }
            Self::EnabledById | Self::EnabledByArn => {
                let (key_id, arn) = create_key(router, case).await;
                authorize_role_on_key(router, case, &key_id, role_arn).await;
                let reference = if self == Self::EnabledById {
                    key_id.clone()
                } else {
                    arn
                };
                (reference, key_id)
            }
        };
        if let Some(expected) = self.observable_state() {
            let reported = key_state(router, case, &key_id).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: the key's state must be observable as {expected} before the \
                 launch, got {reported:?}"
            );
        }
        Some(reference)
    }
}

/// How a group names the version of the launch template it launches from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    /// `$Default`, the word a specification carrying no version also means.
    Default,
    /// `$Latest`.
    Latest,
    /// The version number written out.
    Number,
}

impl Version {
    /// The literal the group's `LaunchTemplate.Version` carries, for a template whose only
    /// version is `latest`.
    fn literal(self, latest: &str) -> String {
        match self {
            Self::Default => "$Default".to_string(),
            Self::Latest => "$Latest".to_string(),
            Self::Number => latest.to_string(),
        }
    }
}

/// One encrypted volume a launch template's data describes: the device it is attached at and
/// the key it names, where it names one.
pub(crate) struct Volume<'a> {
    pub(crate) device: &'a str,
    pub(crate) kms_key_id: Option<&'a str>,
}

/// Create one launch template whose data describes `volumes`, and answer the
/// `(id, latest version number)` Amazon EC2 assigned it.
pub(crate) async fn create_launch_template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    volumes: &[Volume<'_>],
) -> (String, String) {
    let params = template_params(volumes);
    let (status, body) = ec2_query(router, "CreateLaunchTemplate", &launch_data(name, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate for {name:?} expected 200, got {status}: {body}"
    );
    let id = tag_value(&body, "launchTemplateId");
    let latest = tag_value(&body, "latestVersionNumber");
    assert!(
        !id.is_empty() && !latest.is_empty(),
        "case {case:?}: CreateLaunchTemplate must report the template's id and latest version, \
         got {body}"
    );
    (id, latest)
}

/// Add one version to the launch template `name`, describing `volumes`, and answer the
/// version number Amazon EC2 gave it.
async fn create_launch_template_version(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    volumes: &[Volume<'_>],
) -> String {
    let params = template_params(volumes);
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplateVersion",
        &launch_data(name, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplateVersion for {name:?} expected 200, got {status}: \
         {body}"
    );
    let version = tag_value(&body, "versionNumber");
    assert!(
        !version.is_empty(),
        "case {case:?}: CreateLaunchTemplateVersion must report the version number, got {body}"
    );
    version
}

/// The `LaunchTemplateData` parameters one row's volumes make, each volume encrypted and
/// carrying the key the row names for it.
fn template_params(volumes: &[Volume<'_>]) -> Vec<(String, String)> {
    let mut params = Vec::new();
    for (index, volume) in volumes.iter().enumerate() {
        let entry = index + 1;
        let prefix = format!("LaunchTemplateData.BlockDeviceMapping.{entry}");
        params.push(p(&format!("{prefix}.DeviceName"), volume.device));
        params.push(p(&format!("{prefix}.Ebs.VolumeSize"), "8"));
        params.push(p(&format!("{prefix}.Ebs.VolumeType"), "gp3"));
        params.push(p(&format!("{prefix}.Ebs.Encrypted"), "true"));
        if let Some(key_id) = volume.kms_key_id {
            params.push(p(&format!("{prefix}.Ebs.KmsKeyId"), key_id));
        }
    }
    params
}

/// The whole parameter list of a launch-template write: the template's name, the image and
/// instance type every row carries, and the mappings the row describes.
fn launch_data(name: &str, mappings: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut params = vec![
        p("LaunchTemplateName", name),
        p("LaunchTemplateData.ImageId", IMAGE_ID),
        p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
    ];
    params.extend(mappings);
    params
}

/// Move the launch template `name`'s default version to `version`, through the service that
/// owns launch templates.
async fn set_default_version(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    version: &str,
) {
    let (status, body) = ec2_query(
        router,
        "ModifyLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("SetDefaultVersion", version),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ModifyLaunchTemplate for {name:?} expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "defaultVersionNumber"),
        version,
        "case {case:?}: the template's default version must be {version}, got {body}"
    );
}

/// Create one group of maximum size one, holding nothing yet, that launches from the
/// launch-template version `version` of the template `template_id` names.
pub(crate) async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    template_id: &str,
    version: &str,
) {
    let params = [
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateId", template_id),
        p("LaunchTemplate.Version", version),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", zone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {group:?} expected 200, got {status}: {body}"
    );
}

/// Ask the group for one unit of capacity.
pub(crate) async fn set_desired_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    capacity: &str,
) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", capacity),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity {capacity} for {group:?} expected 200, got {status}: \
         {body}"
    );
}

/// What one scaling activity reported: the three members a client reads the outcome of a
/// capacity change off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) status_code: String,
    pub(crate) status_message: String,
    pub(crate) progress: String,
}

/// The group's NEWEST scaling activity once it has reached a terminal status code — the
/// activity a client polling `DescribeScalingActivities` for the outcome of the capacity
/// change it just asked for reads.
///
/// Looked for again until it is terminal or [`DEADLINE`] passes: an activity the clock still
/// owes is reported first and in progress, so this waits for the OUTCOME rather than for any
/// length of time.
pub(crate) async fn settled_activity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Outcome {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeScalingActivities",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeScalingActivities expected 200, got {status}: {body}"
        );
        let members = list_members(&body, "Activities");
        let newest = members.first().copied().unwrap_or_default();
        let status_code = tag_value(newest, "StatusCode");
        if TERMINAL_STATUS_CODES.contains(&status_code.as_str()) {
            return Outcome {
                status_code,
                status_message: xml_unescape(&tag_value(newest, "StatusMessage")),
                progress: tag_value(newest, "Progress"),
            };
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the capacity change must record a terminal activity, got {body}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The `(instance id, lifecycle state, launch-template version)` of every instance the group
/// holds, as the group's own read reports them.
pub(crate) async fn group_instances(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    list_members(&body, "Instances")
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "LifecycleState"),
                tag_value(member, "Version"),
            )
        })
        .collect()
}

/// The whole group read, for the assertions about the group itself.
pub(crate) async fn described_group(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    body
}

/// One row of the cancelled table: the key the version names, where it names it, and how the
/// group names the version.
struct Blocked {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    key: Key,
    /// Whether the key is named on a SECOND volume of the version rather than on its first.
    on_second_volume: bool,
    version: Version,
}

/// A capacity change whose launch-template version encrypts a volume under a key that
/// produces no data key places no instance and records a terminal `Cancelled` activity
/// carrying the Amazon EC2 reason for that key.
#[tokio::test]
async fn a_launch_under_a_key_that_serves_no_volume_is_cancelled() {
    let rows = [
        Blocked {
            case: "a disabled key named by its key id, through $Default",
            group: "key-cancelled-disabled-id",
            template: "lt-key-cancelled-disabled-id",
            key: Key::DisabledById,
            on_second_volume: false,
            version: Version::Default,
        },
        Blocked {
            case: "a disabled key named by its key ARN, through $Latest",
            group: "key-cancelled-disabled-arn",
            template: "lt-key-cancelled-disabled-arn",
            key: Key::DisabledByArn,
            on_second_volume: false,
            version: Version::Latest,
        },
        Blocked {
            case: "a key scheduled for deletion, at a literal version",
            group: "key-cancelled-pending-deletion",
            template: "lt-key-cancelled-pending-deletion",
            key: Key::PendingDeletion,
            on_second_volume: false,
            version: Version::Number,
        },
        Blocked {
            case: "a well-formed key ARN naming no key of the account, through $Default",
            group: "key-cancelled-absent",
            template: "lt-key-cancelled-absent",
            key: Key::Absent,
            on_second_volume: false,
            version: Version::Default,
        },
        Blocked {
            case: "a disabled key on a second volume of the version, through $Default",
            group: "key-cancelled-second-volume",
            template: "lt-key-cancelled-second-volume",
            key: Key::DisabledById,
            on_second_volume: true,
            version: Version::Default,
        },
    ];

    // The one reason Amazon EC2 Auto Scaling records for this class, written out here rather
    // than taken from the implementation: a test that re-used the service's own literal would
    // assert nothing.
    let expected_message = "Instance became unhealthy while waiting for instance to be in \
                            InService state. Termination Reason: \
                            Client.InvalidKMSKey.InvalidState: The KMS key provided is in an \
                            incorrect state";

    for row in &rows {
        let case = row.case;
        let router = emulator();
        let reference = row
            .key
            .reference(&router, case, &default_service_linked_role_arn())
            .await;
        let volumes = if row.on_second_volume {
            vec![
                Volume {
                    device: ROOT_DEVICE,
                    kms_key_id: None,
                },
                Volume {
                    device: SECOND_DEVICE,
                    kms_key_id: reference.as_deref(),
                },
            ]
        } else {
            vec![Volume {
                device: ROOT_DEVICE,
                kms_key_id: reference.as_deref(),
            }]
        };
        let (template_id, latest) =
            create_launch_template(&router, case, row.template, &volumes).await;
        create_group(
            &router,
            case,
            row.group,
            &template_id,
            &row.version.literal(&latest),
        )
        .await;
        set_desired_capacity(&router, case, row.group, "1").await;

        let outcome = settled_activity(&router, case, row.group).await;
        assert_eq!(
            (
                outcome.status_code.as_str(),
                outcome.status_message.as_str(),
                outcome.progress.as_str()
            ),
            ("Cancelled", expected_message, "100"),
            "case {case:?}: expected a cancelled activity carrying the key's refusal, got \
             {outcome:?}"
        );
        let instances = group_instances(&router, case, row.group).await;
        assert!(
            instances.is_empty(),
            "case {case:?}: the group must hold no instance when its volume's key serves \
             nothing, got {instances:?}"
        );
        // The capacity the caller asked for STANDS: the group keeps it and launches again, so
        // repairing the key is all it takes to reach it.
        let described = described_group(&router, case, row.group).await;
        assert_eq!(
            tag_value(&described, "DesiredCapacity"),
            "1",
            "case {case:?}: the group must keep the capacity that was asked for, got {described}"
        );
    }
}

/// One row of the served table: the key the version names, and how the group names the
/// version.
struct Served {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    key: Key,
    version: Version,
}

/// A capacity change whose launch-template version names a key that DOES produce a data key
/// launches its instance and records a `Successful` activity.
#[tokio::test]
async fn a_launch_under_a_key_that_serves_a_volume_places_its_instance() {
    let rows = [
        Served {
            case: "an enabled key named by its key id, through $Default",
            group: "key-served-enabled-id",
            template: "lt-key-served-enabled-id",
            key: Key::EnabledById,
            version: Version::Default,
        },
        Served {
            case: "an enabled key named by its key ARN, at a literal version",
            group: "key-served-enabled-arn",
            template: "lt-key-served-enabled-arn",
            key: Key::EnabledByArn,
            version: Version::Number,
        },
        Served {
            case: "the Amazon Web Services managed key of Amazon EBS, through $Latest",
            group: "key-served-managed-alias",
            template: "lt-key-served-managed-alias",
            key: Key::EbsManagedAlias,
            version: Version::Latest,
        },
        Served {
            case: "an encrypted volume naming no key at all, through $Default",
            group: "key-served-unnamed",
            template: "lt-key-served-unnamed",
            key: Key::Unnamed,
            version: Version::Default,
        },
    ];

    for row in &rows {
        let case = row.case;
        let router = emulator();
        let reference = row
            .key
            .reference(&router, case, &default_service_linked_role_arn())
            .await;
        let volumes = vec![Volume {
            device: ROOT_DEVICE,
            kms_key_id: reference.as_deref(),
        }];
        let (template_id, latest) =
            create_launch_template(&router, case, row.template, &volumes).await;
        create_group(
            &router,
            case,
            row.group,
            &template_id,
            &row.version.literal(&latest),
        )
        .await;
        set_desired_capacity(&router, case, row.group, "1").await;

        let outcome = settled_activity(&router, case, row.group).await;
        assert_eq!(
            (outcome.status_code.as_str(), outcome.status_message.as_str()),
            ("Successful", ""),
            "case {case:?}: expected a successful activity carrying no status message, got \
             {outcome:?}"
        );
        let instances = group_instances(&router, case, row.group).await;
        assert_eq!(
            instances.len(),
            1,
            "case {case:?}: the group must hold the one instance it launched, got {instances:?}"
        );
        assert_eq!(
            instances[0].2, latest,
            "case {case:?}: the instance must report the version it was launched from \
             ({latest}), got {instances:?}"
        );
    }
}

/// The group's own `Version` literal, which never changes.
const FLOATING_VERSION: &str = "$Default";

/// The `$Default` literal is resolved at EACH launch: the same group is cancelled while that
/// word names a version under a disabled key, and launches once the word names a version that
/// names no customer key.
#[tokio::test]
async fn the_default_version_literal_is_resolved_at_every_launch() {
    let case = "a group floating on $Default while the default version moves";
    let group = "key-default-rebinds";
    let template = "lt-key-default-rebinds";
    let router = emulator();

    let (key_id, _) = create_key(&router, case).await;
    authorize_role_on_key(&router, case, &key_id, &default_service_linked_role_arn()).await;
    kms(&router, case, "DisableKey", json!({ "KeyId": &key_id })).await;
    let reported = key_state(&router, case, &key_id).await;
    assert_eq!(
        reported, "Disabled",
        "case {case:?}: the key must be observably disabled before the group is created, got \
         {reported:?}"
    );

    // Version 1 — the template's default — encrypts the root volume under that key.
    let (template_id, first) = create_launch_template(
        &router,
        case,
        template,
        &[Volume {
            device: ROOT_DEVICE,
            kms_key_id: Some(&key_id),
        }],
    )
    .await;
    create_group(&router, case, group, &template_id, FLOATING_VERSION).await;
    set_desired_capacity(&router, case, group, "1").await;

    let blocked = settled_activity(&router, case, group).await;
    assert_eq!(
        blocked.status_code, "Cancelled",
        "case {case:?}: while $Default names version {first} the launch must be cancelled, got \
         {blocked:?}"
    );
    assert!(
        group_instances(&router, case, group).await.is_empty(),
        "case {case:?}: no instance may be placed while $Default names the disabled key's \
         version"
    );

    // Version 2 describes the same volume with NO key of its own, and the default moves to it.
    let second = create_launch_template_version(
        &router,
        case,
        template,
        &[Volume {
            device: ROOT_DEVICE,
            kms_key_id: None,
        }],
    )
    .await;
    assert_ne!(
        second, first,
        "case {case:?}: the second version must be a version of its own, got {second}"
    );
    set_default_version(&router, case, template, &second).await;
    set_desired_capacity(&router, case, group, "1").await;

    let served = settled_activity(&router, case, group).await;
    assert_eq!(
        served.status_code, "Successful",
        "case {case:?}: once $Default names version {second} the same group must launch, got \
         {served:?}"
    );
    let instances = group_instances(&router, case, group).await;
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: the group must hold the one instance version {second} launched, got \
         {instances:?}"
    );
    assert_eq!(
        instances[0].2, second,
        "case {case:?}: the instance must report the version the launch resolved ({second}), got \
         {instances:?}"
    );
    // The group's own specification still floats: what moved is the word's target, not the
    // literal the group stores.
    let described = described_group(&router, case, group).await;
    assert!(
        tag_values(&described, "Version").contains(&FLOATING_VERSION.to_string()),
        "case {case:?}: the group must still report the {FLOATING_VERSION} literal it was \
         created with, got {described}"
    );
}
