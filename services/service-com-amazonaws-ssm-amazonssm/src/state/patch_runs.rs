// SPDX-License-Identifier: CC0-1.0

//! The patch-baseline run Patch Manager performs ON a managed node, and the compliance
//! that run reports back.
//!
//! # What starts one
//!
//! "To scan managed nodes for missing patches, or to scan and install missing patches,
//! Patch Manager uses the `AWS-RunPatchBaseline` SSM document" — a Command document
//! Systems Manager publishes in every account and Region
//! ([`RUN_PATCH_BASELINE_DOCUMENT`], in [`super::PUBLISHED_DOCUMENTS`]), so a send of it
//! resolves without any create and is what starts a patching operation here. The
//! document's `Operation` parameter says which one: `Scan` "assesses the patch compliance
//! state" and `Install` "installs missing patches" (`InstancePatchState$Operation`).
//!
//! # What one does
//!
//! A run is performed against the patch baseline the node is patched with — the baseline
//! the node's patch group is registered with
//! ([`ComAmazonawsSsmAmazonssmState::patch_baseline_in_force`]), and the default baseline of
//! the node's operating system when its group has none — and reports, patch by patch, what
//! it found on the node: the patches that baseline
//! approves and the node does not hold yet are MISSING, an `Install` run installs them,
//! and a later run of either kind reports them installed. The report is the one the patch
//! reads of this service project ([`super::instance_patch_states`],
//! [`super::instance_patches`]), so what a run found is what a describe reports, and the
//! high-level counts are counted from the patch rows rather than stated twice.
//!
//! Nothing here holds a package database of its own: the patches a node holds are the ones
//! the previous report of that node says Patch Manager installed, and the catalogue of what
//! exists to install is Patch Manager's own published catalogue
//! ([`super::patch_catalog`]), narrowed by the approval configuration of the baseline in
//! force — the account's own for a baseline it created, and the configuration published
//! with the baseline for one Systems Manager predefines
//! ([`super::patch_baselines::predefined_baseline_record`]).
//!
//! # What a run takes from the send, and what it only records
//!
//! The document's `Operation` decides WHICH operation runs and its `RebootOption` whether a
//! patch this run installs is left waiting for the reboot that completes it, so both steer
//! what is reported. Three of its parameters are ACCEPTED AND RECORDED ONLY: `SnapshotId`
//! and `InstallOverrideList` are the two the report has members of
//! (`InstancePatchState$SnapshotId`, `$InstallOverrideList`), so a send that names one has
//! it reported back, and `BaselineOverride` is read by nothing at all. What the run is
//! performed against is the baseline in force for the node — a patch set this emulator
//! holds, where an override names one it does not: a list of patches at a URL this emulator
//! never fetches, or a baseline document the send carries that no store here holds.

use std::collections::BTreeMap;
use std::time::SystemTime;

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::ops::types::DocumentParameter;

use super::instance_patch_states::{InstancePatchStateRecord, OPERATION_INSTALL, OPERATION_SCAN};
use super::instance_patches::{
    PatchComplianceRow, STATE_INSTALLED, STATE_INSTALLED_PENDING_REBOOT, STATE_INSTALLED_REJECTED,
    STATE_MISSING, STATE_NOT_APPLICABLE,
};
use super::node_registration::{PLATFORM_TYPE_LINUX, PLATFORM_TYPE_WINDOWS};
use super::patch_catalog::{
    DEPLOYMENT_STATUS_APPROVED, DEPLOYMENT_STATUS_EXPLICIT_APPROVED,
    DEPLOYMENT_STATUS_EXPLICIT_REJECTED,
};
use super::ComAmazonawsSsmAmazonssmState;

/// The name of the Command document Patch Manager publishes to run a patch baseline on a
/// node: "Patch Manager uses the `AWS-RunPatchBaseline` SSM document" (AWS Systems Manager
/// User Guide, "About the `AWS-RunPatchBaseline` SSM document").
pub const RUN_PATCH_BASELINE_DOCUMENT: &str = "AWS-RunPatchBaseline";

/// The step action of [`RUN_PATCH_BASELINE`] that performs the patch-baseline operation.
///
/// The published body of that document downloads and runs Patch Manager's own payload on
/// the node, which this emulator holds no copy of; the body below is its stand-in — the
/// same stand-in the published runbooks of [`super::PUBLISHED_DOCUMENTS`] are — and names
/// the operation as its one step's action, which is what a send resolves the run from
/// ([`requested`]).
pub const ACTION_RUN_PATCH_BASELINE: &str = "aws:runPatchBaseline";

/// The document parameter that says which patching operation to perform: "`Scan`: Patch
/// Manager scans the node and reports compliance … `Install`: Patch Manager installs
/// missing patches" (same guide section). Required — the document declares no default for
/// it, so a send has to name one.
pub const PARAMETER_OPERATION: &str = "Operation";

/// The document parameter that says whether the node may be rebooted after patches are
/// installed (same section), whose values are the modelled `RebootOption` ones
/// (`crate::ops::types::REBOOT_OPTION_VALUES`).
pub const PARAMETER_REBOOT_OPTION: &str = "RebootOption";

/// The document parameter naming the patch-baseline snapshot a run uses, which Patch
/// Manager passes when a maintenance window task runs the document on several nodes (same
/// section).
pub const PARAMETER_SNAPSHOT_ID: &str = "SnapshotId";

/// The document parameter naming "an https URL or an Amazon S3 path-style URL to a list of
/// patches to be installed" (`InstancePatchState$InstallOverrideList`).
pub const PARAMETER_INSTALL_OVERRIDE_LIST: &str = "InstallOverrideList";

/// The reboot option a run takes when the send names none: "`RebootIfNeeded`" is the
/// document's own default (same section), one of the modelled `RebootOption` values.
pub const REBOOT_OPTION_REBOOT_IF_NEEDED: &str = "RebootIfNeeded";

/// The other modelled `RebootOption` value: the node is not rebooted, so a patch this run
/// installed is installed and still waiting for the reboot that completes it.
pub const REBOOT_OPTION_NO_REBOOT: &str = "NoReboot";

/// The body of [`RUN_PATCH_BASELINE_DOCUMENT`]: the parameters the "About the
/// `AWS-RunPatchBaseline` SSM document" guide section documents for it, and the one step
/// that performs the operation ([`ACTION_RUN_PATCH_BASELINE`]).
///
/// `Operation` declares no default, so it is the one parameter a send must supply; every
/// other parameter is optional there and declares the default that section documents.
pub const RUN_PATCH_BASELINE: &str = r#"{
  "schemaVersion": "2.2",
  "description": "Scan or install patches on a managed node using the node's patch baseline.",
  "parameters": {
    "Operation": {
      "type": "String",
      "description": "(Required) The update or configuration to perform on the node. Scan reports the compliance state; Install installs the missing patches the baseline approves.",
      "allowedValues": ["Scan", "Install"]
    },
    "SnapshotId": {
      "type": "String",
      "description": "(Optional) The snapshot ID of the patch baseline to use.",
      "default": ""
    },
    "InstallOverrideList": {
      "type": "String",
      "description": "(Optional) An https URL or an Amazon S3 path-style URL to the list of patches to be installed.",
      "default": ""
    },
    "RebootOption": {
      "type": "String",
      "description": "(Optional) Whether the node is rebooted after patches are installed.",
      "default": "RebootIfNeeded",
      "allowedValues": ["RebootIfNeeded", "NoReboot"]
    },
    "BaselineOverride": {
      "type": "String",
      "description": "(Optional) The patch baseline to use in place of the node's own, as a JSON document.",
      "default": ""
    }
  },
  "mainSteps": [
    {
      "name": "PatchBaseline",
      "action": "aws:runPatchBaseline",
      "inputs": {
        "Operation": "{{ Operation }}",
        "SnapshotId": "{{ SnapshotId }}",
        "InstallOverrideList": "{{ InstallOverrideList }}",
        "RebootOption": "{{ RebootOption }}",
        "BaselineOverride": "{{ BaselineOverride }}"
      }
    }
  ]
}"#;

/// The tag keys whose value names the patch group a node belongs to: "the tag key must be
/// `Patch Group` or `PatchGroup`" (AWS Systems Manager User Guide, "Add nodes to a patch
/// group"), in that order — a node carrying both is in the group the first names.
pub const PATCH_GROUP_TAG_KEYS: &[&str] = &["Patch Group", "PatchGroup"];

/// The operating system a Linux node is patched against when this endpoint cannot read
/// which distribution its image is of.
///
/// A managed node reports no `PlatformName` here (nothing reads inside its guest), so the
/// distribution is read off the name of the Amazon EC2 image the node was launched from
/// ([`DISTRIBUTION_IMAGE_NAMES`]); this is the emulator's CHOSEN fallback for a node whose
/// image names none — the current Amazon Linux release, which is what the images this
/// emulator publishes are builds of.
pub const FALLBACK_LINUX_OPERATING_SYSTEM: &str = "AMAZON_LINUX_2023";

/// How an image name names the distribution it is a build of: the prefix of the image NAME
/// Amazon publishes a distribution's images under, and the modelled `OperatingSystem` value
/// (`crate::ops::types::OPERATING_SYSTEM_VALUES`) Patch Manager knows that distribution by.
///
/// The prefixes are the ones the published image names of each distribution start with
/// (`al2023-ami-…`, `amzn2-ami-…`, `ubuntu/images/…`, `RHEL-9…`), matched
/// case-insensitively and longest first, so `amzn2` is not read as `amzn`.
pub const DISTRIBUTION_IMAGE_NAMES: &[(&str, &str)] = &[
    ("al2023", "AMAZON_LINUX_2023"),
    ("al2022", "AMAZON_LINUX_2022"),
    ("amzn2", "AMAZON_LINUX_2"),
    ("amzn", "AMAZON_LINUX"),
    ("ubuntu", "UBUNTU"),
    ("debian", "DEBIAN"),
    ("rhel", "REDHAT_ENTERPRISE_LINUX"),
    ("centos", "CENTOS"),
    ("rocky", "ROCKY_LINUX"),
    ("almalinux", "ALMA_LINUX"),
    ("suse", "SUSE"),
    ("sles", "SUSE"),
    ("oracle", "ORACLE_LINUX"),
];

/// The `OperatingSystem` of the one platform type that names a distribution of its own.
const WINDOWS_OPERATING_SYSTEM: &str = "WINDOWS";

/// The `OperatingSystem` of a node whose platform type is Apple's.
const MACOS_OPERATING_SYSTEM: &str = "MACOS";

/// What one send asked a node's agent to do: the patch-baseline step of the document it
/// resolved, with that step's inputs taken from the parameters the send supplied and the
/// defaults the document declares.
#[derive(Debug, Clone)]
pub struct PatchBaselineRun {
    /// The step's own `name`, which is what `GetCommandInvocation`'s `PluginName` names.
    pub step_name: String,
    /// The patching operation to perform: [`OPERATION_SCAN`] or [`OPERATION_INSTALL`].
    pub operation: String,
    /// The reboot option the run was asked for ([`REBOOT_OPTION_REBOOT_IF_NEEDED`],
    /// [`REBOOT_OPTION_NO_REBOOT`]), which is what the report records.
    pub reboot_option: String,
    /// "The ID of the patch baseline snapshot used during the patching operation when this
    /// compliance data was collected" (`InstancePatchState$SnapshotId`), for a send that
    /// named one.
    pub snapshot_id: Option<String>,
    /// "An https URL or an Amazon S3 path-style URL to a list of patches to be installed"
    /// (`InstancePatchState$InstallOverrideList`), for a send that named one.
    pub install_override_list: Option<String>,
}

/// The patch-baseline run the document `content` asks for on a node of `platform_type`, or
/// `None` when that content declares no such step for it — which is every other document,
/// including every one a caller created.
///
/// `supplied` are the parameter values the send carried and `declared` the document's own
/// parameters, whose `DefaultValue` fills in for one the send left out, exactly as a
/// command document's script resolves its inputs ([`super::guest_execution::shell_step`]).
pub fn requested(
    content: &str,
    platform_type: &str,
    supplied: &BTreeMap<String, Vec<String>>,
    declared: &[DocumentParameter],
) -> Option<PatchBaselineRun> {
    let document = super::load_single_document(content).ok()?;
    let map = document.as_hash()?;
    let steps = super::mapping_get(map, "mainSteps")?.as_vec()?;
    let step = steps.iter().filter_map(::yaml_rust2::Yaml::as_hash).find(|step| {
        super::mapping_get(step, "action")
            .and_then(super::scalar_string)
            .as_deref()
            == Some(ACTION_RUN_PATCH_BASELINE)
            && super::document_platforms::step_runs_on(step, platform_type)
    })?;
    let value_of = |name: &str| -> Option<String> {
        supplied
            .get(name)
            .and_then(|values| values.first())
            .cloned()
            .or_else(|| {
                declared
                    .iter()
                    .find(|parameter| parameter.name.as_deref() == Some(name))
                    .and_then(|parameter| parameter.default_value.clone())
            })
            .filter(|value| !value.is_empty())
    };
    // "Scan assesses the patch compliance state" is what a run reports when the operation
    // names neither of the two: the document's `allowedValues` are the only two it admits,
    // and a scan is the operation that changes nothing on the node.
    let operation = match value_of(PARAMETER_OPERATION).as_deref() {
        Some(named) if named.eq_ignore_ascii_case(OPERATION_INSTALL) => OPERATION_INSTALL,
        _ => OPERATION_SCAN,
    };
    let reboot_option = match value_of(PARAMETER_REBOOT_OPTION).as_deref() {
        Some(named) if named.eq_ignore_ascii_case(REBOOT_OPTION_NO_REBOOT) => {
            REBOOT_OPTION_NO_REBOOT
        }
        _ => REBOOT_OPTION_REBOOT_IF_NEEDED,
    };
    Some(PatchBaselineRun {
        step_name: super::mapping_get(step, "name")
            .and_then(super::scalar_string)
            .unwrap_or_default(),
        operation: operation.to_string(),
        reboot_option: reboot_option.to_string(),
        snapshot_id: value_of(PARAMETER_SNAPSHOT_ID),
        install_override_list: value_of(PARAMETER_INSTALL_OVERRIDE_LIST),
    })
}

/// The `OperatingSystem` Patch Manager patches the node `instance_id` against.
///
/// A node reports its `PlatformType` and nothing finer, so a Windows node is patched
/// against the Windows baseline and a macOS node against the macOS one; for a Linux node
/// the distribution is read off the NAME of the Amazon EC2 image it was launched from,
/// which is where that fact lives ([`DISTRIBUTION_IMAGE_NAMES`]), and a node whose image
/// this emulator no longer holds — or one that is no instance at all — is patched against
/// [`FALLBACK_LINUX_OPERATING_SYSTEM`].
pub fn node_operating_system<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
    platform_type: Option<&str>,
) -> &'static str {
    match platform_type {
        Some(platform) if platform == PLATFORM_TYPE_WINDOWS => return WINDOWS_OPERATING_SYSTEM,
        Some(platform) if platform != PLATFORM_TYPE_LINUX => return MACOS_OPERATING_SYSTEM,
        _ => {}
    }
    distribution_of_image(engine, instance_id).unwrap_or(FALLBACK_LINUX_OPERATING_SYSTEM)
}

/// The distribution the image of the instance `instance_id` is a build of, or `None` when
/// this engine holds no such instance, no such image, or an image whose name names no
/// distribution this mapping knows.
fn distribution_of_image<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
) -> Option<&'static str> {
    let ec2 = ec2::service_state(engine)?;
    let store = ec2.lock();
    let image_id = store.instances.get(instance_id)?.image_id.clone()?;
    let name = store.images.get(&image_id)?.name.clone()?;
    let name = name.to_ascii_lowercase();
    DISTRIBUTION_IMAGE_NAMES
        .iter()
        .find(|(prefix, _)| name.starts_with(prefix))
        .map(|(_, operating_system)| *operating_system)
}

/// Perform `run` on the node `instance_id` of `region`, which is patched against
/// `operating_system`'s baseline ([`node_operating_system`]), at `now` — reporting the patch
/// compliance it found, or `false` when `region` holds no such node or no baseline is in
/// force for that operating system, which is a run that reached nothing.
///
/// Takes no engine: everything it reads and writes is THIS service's own store, so it runs
/// with that store's guard held and nothing else's — which is why the operating system is
/// resolved before the guard is taken rather than here.
///
/// A run REPLACES whatever the previous patching operation reported for that node ("one
/// patching operation on a node supersedes what the previous one reported for it"), and the
/// patches the node already holds are the ones that previous report says are installed, so
/// an `Install` run is what makes a later run report a patch installed.
pub fn perform(
    store: &mut ComAmazonawsSsmAmazonssmState,
    region: &str,
    instance_id: &str,
    operating_system: &str,
    run: &PatchBaselineRun,
    now: SystemTime,
) -> bool {
    let Some(node) = store.managed_node(region, instance_id) else {
        return false;
    };
    let patch_group = PATCH_GROUP_TAG_KEYS
        .iter()
        .find_map(|key| node.tags.get(*key))
        .cloned()
        .unwrap_or_default();
    // The baseline in force for the node: the one its patch group is registered with, and
    // the default baseline of its operating system when its group has none — the one reading
    // of "the patch baseline that should be used for the specified patch group"
    // ([`ComAmazonawsSsmAmazonssmState::patch_baseline_in_force`]), so what a patch-group
    // read reports and what this run is performed against cannot disagree.
    let Some(baseline) = store.patch_baseline_in_force(region, &patch_group, operating_system)
    else {
        return false;
    };
    // What the node already holds, from the report the previous run left: the state each
    // installed patch was left in and the instant it was installed at, both of which a
    // later report carries on — the report IS what this endpoint knows of the node.
    let installed: BTreeMap<String, (String, SystemTime)> = store
        .instance_patch_state(region, instance_id)
        .map(|previous| {
            previous
                .patches
                .iter()
                .filter(|row| row.is_installed())
                .map(|row| (row.patch_id.clone(), (row.state.clone(), row.installed_time)))
                .collect()
        })
        .unwrap_or_default();
    // The baseline's own approval configuration is what decides each patch's state. A
    // baseline Systems Manager predefined carries the configuration published with it
    // ([`super::patch_baselines::predefined_baseline_record`]), so a node is patched
    // against a real set of approvals before the account has created a baseline of its own.
    let predefined = super::patch_baselines::predefined_baseline_with_id(
        region,
        &baseline.baseline_id,
    )
    .map(|baseline| super::patch_baselines::predefined_baseline_record(region, baseline));
    let effective = store
        .patch_baseline(region, &baseline.baseline_id)
        .or(predefined.as_ref())
        .map(|record| super::patch_catalog::effective_patches(record, now))
        .unwrap_or_default();
    let installing = run.operation == OPERATION_INSTALL;
    let pending_reboot = run.reboot_option == REBOOT_OPTION_NO_REBOOT;
    let rows: Vec<PatchComplianceRow> = effective
        .into_iter()
        .map(|record| {
            let held = installed.get(&record.patch.id).cloned();
            let approved = matches!(
                record.deployment_status,
                DEPLOYMENT_STATUS_APPROVED | DEPLOYMENT_STATUS_EXPLICIT_APPROVED
            );
            let rejected = record.deployment_status == DEPLOYMENT_STATUS_EXPLICIT_REJECTED;
            let (state, installed_time) = match (held, approved, rejected) {
                // Installed and still approved: the patch the baseline wants is on the
                // node, as of the instant it was installed — and still waiting for its
                // reboot where the run that installed it was not allowed to reboot the
                // node, since nothing here reboots one.
                (Some((held, at)), true, _) => (
                    if held == STATE_INSTALLED_PENDING_REBOOT {
                        STATE_INSTALLED_PENDING_REBOOT
                    } else {
                        STATE_INSTALLED
                    },
                    at,
                ),
                // Installed and since rejected: "patches with a status of
                // INSTALLED_REJECTED were typically installed before they were added to a
                // RejectedPatches list" (`DescribePatchGroupState`).
                (Some((_, at)), _, true) => (STATE_INSTALLED_REJECTED, at),
                // Installed and no longer selected by any rule: a patch on the node that
                // the baseline does not define.
                (Some((_, at)), _, _) => (super::instance_patches::STATE_INSTALLED_OTHER, at),
                // Approved and absent: an `Install` run installs it now, and a `Scan` run
                // reports it missing. A node the run does not reboot holds a patch that
                // is installed and waiting for its reboot.
                (None, true, _) if installing && pending_reboot => {
                    (STATE_INSTALLED_PENDING_REBOOT, now)
                }
                (None, true, _) if installing => (STATE_INSTALLED, now),
                (None, true, _) => (STATE_MISSING, now),
                // Absent and not approved — due to be approved by a rule, or explicitly
                // rejected: the baseline defines the patch and it is not one to install on
                // this node.
                (None, _, _) => (STATE_NOT_APPLICABLE, now),
            };
            PatchComplianceRow::reported(
                &record.patch,
                operating_system,
                state,
                // "The date/time the patch was installed on the managed node. Not all
                // operating systems provide this level of information"
                // (`PatchComplianceData$InstalledTime`) — a patch that is not installed has
                // no such instant, which this endpoint reports as the epoch rather than as
                // the instant of the run that did not install it.
                if state == STATE_MISSING || state == STATE_NOT_APPLICABLE {
                    SystemTime::UNIX_EPOCH
                } else {
                    installed_time
                },
                record.compliance_level.clone(),
            )
        })
        .collect();

    let mut report = InstancePatchStateRecord::from_rows(
        instance_id,
        &patch_group,
        &baseline.baseline_id,
        &run.operation,
        now,
        now,
        rows,
    );
    report.snapshot_id = run.snapshot_id.clone();
    report.install_override_list = run.install_override_list.clone();
    report.reboot_option = Some(run.reboot_option.clone());
    // "The time of the last attempt to patch the managed node with `NoReboot` specified as
    // the reboot option" (`InstancePatchState$LastNoRebootInstallOperationTime`): this run,
    // when that is what it was, and otherwise whatever the previous report recorded.
    report.last_no_reboot_install_operation_time = if installing && pending_reboot {
        Some(now)
    } else {
        store
            .instance_patch_state(region, instance_id)
            .and_then(|previous| previous.last_no_reboot_install_operation_time)
    };
    store.insert_instance_patch_state(region, report)
}
