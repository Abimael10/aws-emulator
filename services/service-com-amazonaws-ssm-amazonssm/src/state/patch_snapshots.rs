// SPDX-License-Identifier: CC0-1.0

//! The PATCH SNAPSHOT one managed node deploys a patch baseline from: what a snapshot holds,
//! where this emulator serves it, and what a node's `Product` is.
//!
//! # What a snapshot is
//!
//! `GetDeployablePatchSnapshotForInstance` is the call Patch Manager's own document makes
//! from INSIDE a node ("This API is primarily used by the AWS-RunPatchBaseline Systems
//! Manager document"): the node hands up the snapshot id its run was given
//! ([`super::patch_runs::PARAMETER_SNAPSHOT_ID`]) and is answered the snapshot of the patch
//! baseline in force for it, with a URL to download that snapshot from.
//!
//! A snapshot is FROZEN at the instant it is first taken: the same `(node, snapshot id)` pair
//! is answered the same product and the same content however often it is asked, which is what
//! makes it a snapshot rather than a second evaluation of the baseline. So the document is
//! rendered once, when the snapshot is minted, and held ([`PatchSnapshotRecord`]) — one
//! maintenance-window task hands one snapshot id to many nodes, so a snapshot belongs to the
//! node it was taken for as much as to the id it was asked for, and the store is keyed by
//! both.
//!
//! # What it holds, and where each part comes from
//!
//! * the baseline in force for the node — the one its patch group is registered with, and the
//!   default of its operating system when its group has none
//!   ([`super::ComAmazonawsSsmAmazonssmState::patch_baseline_in_force`]) — or the baseline the
//!   request OVERRODE it with ([`baseline_from_override`]), so the one reading of "the patch
//!   baseline the managed node uses" serves a patching run and a snapshot alike;
//! * the patches that baseline makes effective at the instant the snapshot was taken
//!   ([`super::patch_catalog::effective_patches`]), which is what the node installs from;
//! * the node's `Product` ([`node_product`]), the specific release of its operating system.
//!
//! # Where it is served
//!
//! AWS answers "a pre-signed Amazon Simple Storage Service (Amazon S3) URL"
//! (`GetDeployablePatchSnapshotForInstanceResult$SnapshotDownloadUrl`) of the bucket Patch
//! Manager keeps snapshots in, which SSM Agent is documented to need read access to
//! (`arn:aws:s3:::patch-baseline-snapshot-<region>/*`, AWS Systems Manager User Guide, "About
//! minimum S3 Bucket permissions for SSM Agent"). This emulator serves no AWS bucket, so the
//! URL it hands out is signed for its OWN endpoint at the path that bucket's object is
//! addressed by ([`snapshot_object_path`]) — a URL the node can actually fetch — and the front
//! door claims and serves that path from the record below (`crate::service`), so the address
//! announced and the address served cannot disagree.
//!
//! WHAT MAKES THAT OBJECT REACHABLE is the key component this endpoint MINTS for each
//! snapshot ([`PatchSnapshotRecord::object_key`]) and hands to the one caller it answered,
//! inside the signed URL. A real presigned URL is reachable only with the signature it was
//! handed out with; nothing else about the object's address is secret there either — the
//! bucket is published, and the node id and the snapshot id are the caller's own. So the
//! minted key is what stands in for that signature here: a request that was never handed the
//! URL addresses no object, however well it guesses the Region, the node and the snapshot id,
//! and the download is answered from the RECORD that key resolves rather than from the path's
//! own values.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use super::patch_baselines::{
    PatchBaselineRecord, DEFAULT_APPROVED_PATCHES_COMPLIANCE_LEVEL,
    DEFAULT_REJECTED_PATCHES_ACTION,
};
use super::patch_properties::{PATCH_SET_OS, PUBLISHED_PATCH_METADATA};
use super::ComAmazonawsSsmAmazonssmState;
use crate::ops::types::BaselineOverride;

/// The first labels of the bucket Patch Manager keeps a Region's patch snapshots in: the
/// bucket is `patch-baseline-snapshot-<region>` (AWS Systems Manager User Guide, "About
/// minimum S3 Bucket permissions for SSM Agent"), so the Region completes the name.
pub const SNAPSHOT_BUCKET_PREFIX: &str = "patch-baseline-snapshot-";

/// How long a snapshot download URL this emulator signs stays valid. CHOSEN — AWS documents
/// no validity for it — and long enough that a node which was handed one has time to fetch
/// it.
pub const SNAPSHOT_DOWNLOAD_URL_VALIDITY: Duration = Duration::from_secs(30 * 60);

/// How many snapshots this emulator holds, across every Region and node, before another one
/// is refused: the store is in memory for as long as the process runs, and one node asks for
/// a snapshot per patching run, so the ceiling is the emulator's OWN guard against a store
/// that grows without bound rather than a limit AWS publishes.
pub const MAX_PATCH_SNAPSHOTS: usize = 2_000;

/// What a request past [`MAX_PATCH_SNAPSHOTS`] is told — named as the emulator's own limit,
/// because it is.
#[must_use]
pub fn snapshot_store_full_message() -> String {
    format!(
        "This emulator records at most {MAX_PATCH_SNAPSHOTS} patch snapshots at a time; delete \
         nodes or restart the emulator to take another."
    )
}

/// The bucket a snapshot of `region` is addressed in.
#[must_use]
pub fn snapshot_bucket(region: &str) -> String {
    format!("{SNAPSHOT_BUCKET_PREFIX}{region}")
}

/// The path one snapshot's object is addressed at: the Region's bucket, then the account, the
/// node, the snapshot id and the key component this endpoint minted for that snapshot
/// ([`mint_object_key`]), as a path-style S3 URL lays a bucket and a key out.
///
/// ONE place states the form, so the URL this service hands out and the request it claims back
/// cannot disagree about it (see [`parse_snapshot_target`]).
///
/// Nothing here is escaped because nothing here can need it: the Region is
/// [`super::region_is_well_formed`], the node id and the snapshot id carry only what their
/// modelled patterns admit (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$` and the 36-character UUID
/// form), and the minted key is hexadecimal and hyphens — every one of them a path segment as
/// it stands.
#[must_use]
pub fn snapshot_object_path(
    region: &str,
    instance_id: &str,
    snapshot_id: &str,
    object_key: &str,
) -> String {
    format!(
        "/{bucket}/{account}/{instance_id}/{snapshot_id}/{object_key}",
        bucket = snapshot_bucket(region),
        account = ::engine::emulator_account_id(),
    )
}

/// A fresh key component for one snapshot's object: the crate's one identifier draw
/// ([`super::new_uuid`]), so it is unguessable and nothing of the request is in it.
///
/// Drawn once, when the snapshot is taken, and PERSISTED with it, so every URL handed out for
/// that snapshot addresses the one object and a request holding none of them addresses nothing
/// (see the module docs).
#[must_use]
pub fn mint_object_key() -> String {
    super::new_uuid()
}

/// Whether `path` addresses an object of a snapshot bucket at all.
///
/// The PATH is what makes a request the snapshot plane's: a URL this service announced is
/// addressed to its own endpoint, which every modelled operation is addressed to as well, so
/// the published bucket the path opens with is what tells the two apart. The KEY is not part
/// of that reading, so a key naming no snapshot this store holds is answered by the plane as
/// the object it does not hold rather than by the router as a request naming no operation.
#[must_use]
pub fn addresses_snapshot_store(path: &str) -> bool {
    path.strip_prefix('/')
        .and_then(|rest| rest.split('/').next())
        .and_then(|bucket| bucket.strip_prefix(SNAPSHOT_BUCKET_PREFIX))
        .is_some_and(super::region_is_well_formed)
}

/// The snapshot one request to the snapshot plane addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTarget {
    /// The Region whose bucket the object is in, read off the bucket's own name.
    pub region: String,
    /// The node the snapshot was taken for.
    pub instance_id: String,
    /// The snapshot's own id.
    pub snapshot_id: String,
    /// The minted key component the request carried, which the held snapshot's own
    /// ([`PatchSnapshotRecord::object_key`]) has to match for the object to be served at all.
    pub object_key: String,
}

/// The snapshot `path` addresses, or `None` when it addresses none — read back from the same
/// parts [`snapshot_object_path`] builds the announced URL out of.
///
/// An object under ANOTHER account's prefix names no snapshot of this emulator: the account is
/// part of the key the URL published, so a path that carries a different one was never handed
/// out by it.
#[must_use]
pub fn parse_snapshot_target(path: &str) -> Option<SnapshotTarget> {
    let mut segments = path.strip_prefix('/')?.split('/');
    let region = segments.next()?.strip_prefix(SNAPSHOT_BUCKET_PREFIX)?;
    if !super::region_is_well_formed(region) {
        return None;
    }
    let account = segments.next()?;
    if account != ::engine::emulator_account_id() {
        return None;
    }
    let instance_id = segments.next()?;
    let snapshot_id = segments.next()?;
    let object_key = segments.next()?;
    if segments.next().is_some()
        || instance_id.is_empty()
        || snapshot_id.is_empty()
        || object_key.is_empty()
    {
        return None;
    }
    Some(SnapshotTarget {
        region: region.to_string(),
        instance_id: instance_id.to_string(),
        snapshot_id: snapshot_id.to_string(),
        object_key: object_key.to_string(),
    })
}

/// One patch snapshot, as the store holds it.
///
/// The document is rendered when the snapshot is MINTED and held as it was rendered, because a
/// snapshot is frozen at that instant (see the module docs); every other member is what the
/// read that minted it reported, so a second read of the same snapshot reports the same thing.
#[derive(Debug, Clone)]
pub struct PatchSnapshotRecord {
    /// "The user-defined snapshot ID" (`GetDeployablePatchSnapshotForInstanceResult$SnapshotId`).
    pub snapshot_id: String,
    /// The node the snapshot was taken for.
    pub instance_id: String,
    /// The key component this endpoint minted for the snapshot's object
    /// ([`mint_object_key`]): what makes that object reachable, and what a download has to
    /// carry (see the module docs).
    pub object_key: String,
    /// "The specific operating system … on the managed node for the specified patch snapshot"
    /// (`$Product`), as [`node_product`] resolved it.
    pub product: String,
    /// The baseline the snapshot is of — its own id, or the id of the baseline the request
    /// overrode it with.
    pub baseline_id: String,
    /// When the snapshot was taken.
    pub created_at: SystemTime,
    /// The snapshot itself: the bytes the download URL serves ([`snapshot_document`]).
    pub document: String,
}

impl ComAmazonawsSsmAmazonssmState {
    /// The snapshot `region` holds for `instance_id` under `snapshot_id`, or `None` when no
    /// read has taken one yet.
    pub fn patch_snapshot(
        &self,
        region: &str,
        instance_id: &str,
        snapshot_id: &str,
    ) -> Option<&PatchSnapshotRecord> {
        self.patch_snapshots.get(&(
            region.to_string(),
            instance_id.to_string(),
            snapshot_id.to_string(),
        ))
    }

    /// The snapshot one request to the download plane addresses: the held snapshot of that
    /// node whose minted key component is the one the request carried, and `None` for every
    /// other request.
    ///
    /// The KEY decides it, not the path: a request that carries a key this store minted for
    /// another snapshot — or one it minted for nothing at all — addresses no object, so the
    /// bytes a download serves are only ever the ones the caller that was handed this URL was
    /// answered (see the module docs).
    pub fn addressed_patch_snapshot(
        &self,
        target: &SnapshotTarget,
    ) -> Option<&PatchSnapshotRecord> {
        self.patch_snapshot(&target.region, &target.instance_id, &target.snapshot_id)
            .filter(|snapshot| snapshot.object_key == target.object_key)
    }

    /// Hold `snapshot` as the snapshot of its node in `region`, or answer `false` when this
    /// store already holds [`MAX_PATCH_SNAPSHOTS`] of them.
    ///
    /// A snapshot already held is left exactly as it was taken — a snapshot is frozen — so
    /// re-taking one neither rewrites it nor counts against the ceiling again.
    pub fn insert_patch_snapshot(&mut self, region: &str, snapshot: PatchSnapshotRecord) -> bool {
        let key = (
            region.to_string(),
            snapshot.instance_id.clone(),
            snapshot.snapshot_id.clone(),
        );
        if self.patch_snapshots.contains_key(&key) {
            return true;
        }
        if self.patch_snapshots.len() >= MAX_PATCH_SNAPSHOTS {
            return false;
        }
        self.patch_snapshots.insert(key, snapshot);
        true
    }
}

/// The baseline a request's `BaselineOverride` stands for, as the same record a baseline the
/// account created is held as — so which patches it makes effective is worked out by the one
/// reader of a baseline ([`super::patch_catalog::effective_patches`]) rather than a second way.
///
/// Nothing of it is stored: an override is a baseline the CALL carried rather than one the
/// account holds, so it exists for the snapshot that was taken with it and is identified by
/// that snapshot's own id. Its instants are the instant the snapshot was taken, which is when
/// the request stated it.
#[must_use]
pub fn baseline_from_override(
    region: &str,
    snapshot_id: &str,
    operating_system: &str,
    overridden: &BaselineOverride,
    at: SystemTime,
) -> PatchBaselineRecord {
    PatchBaselineRecord {
        baseline_id: override_baseline_id(snapshot_id),
        region: region.to_string(),
        name: override_baseline_id(snapshot_id),
        operating_system: operating_system.to_string(),
        global_filters: overridden.global_filters.clone(),
        approval_rules: overridden.approval_rules.clone(),
        approved_patches: overridden.approved_patches.clone().unwrap_or_default(),
        approved_patches_compliance_level: overridden
            .approved_patches_compliance_level
            .clone()
            .unwrap_or_else(|| DEFAULT_APPROVED_PATCHES_COMPLIANCE_LEVEL.to_string()),
        approved_patches_enable_non_security: overridden.approved_patches_enable_non_security,
        rejected_patches: overridden.rejected_patches.clone().unwrap_or_default(),
        rejected_patches_action: overridden
            .rejected_patches_action
            .clone()
            .unwrap_or_else(|| DEFAULT_REJECTED_PATCHES_ACTION.to_string()),
        description: None,
        sources: overridden.sources.clone().unwrap_or_default(),
        available_security_updates_compliance_status: overridden
            .available_security_updates_compliance_status
            .clone(),
        client_token: None,
        tags: BTreeMap::new(),
        created_date: at,
        modified_date: at,
    }
}

/// How a baseline a request overrode its node's with is NAMED in the snapshot it was taken
/// into: the snapshot's own id, because that override belongs to that one snapshot and to no
/// baseline the account holds.
#[must_use]
pub fn override_baseline_id(snapshot_id: &str) -> String {
    format!("override-{snapshot_id}")
}

/// The `Product` of the node `instance_id`: "the specific operating system (for example
/// Windows Server 2012 or Amazon Linux 2015.09) on the managed node"
/// (`GetDeployablePatchSnapshotForInstanceResult$Product`).
///
/// A product is one of the ones Patch Manager PUBLISHES patches for under that node's
/// operating system ([`PUBLISHED_PATCH_METADATA`], the operating-system patch set), so a
/// snapshot names a product the patches in it actually apply to. WHICH of them is read off the
/// release the node is of, from the two places that fact can live: the operating system's own
/// name and version as the node reports them (`InstanceProperty$PlatformName`,
/// `$PlatformVersion`), and the NAME of the Amazon EC2 image the node was launched from — the
/// same fact Patch Manager reads a Linux node's distribution off
/// ([`super::patch_runs::node_operating_system`]). The product named there is the one reported,
/// longest name first so `AmazonLinux2.0` is not read as `AmazonLinux2`; a node whose release
/// neither names is reported as the NEWEST product published for its operating system, which is
/// the last of them, because the catalogue publishes a product per release in ascending order.
pub fn node_product<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
    operating_system: &str,
    platform_name: Option<&str>,
    platform_version: Option<&str>,
) -> String {
    let published = published_products(operating_system);
    let reported = normalized(&format!(
        "{} {} {}",
        platform_name.unwrap_or_default(),
        platform_version.unwrap_or_default(),
        super::patch_runs::image_name(engine, instance_id).unwrap_or_default(),
    ));
    let named = published
        .iter()
        .filter(|product| reported.contains(&normalized(product)))
        .max_by_key(|product| product.len());
    match named.or_else(|| published.last()) {
        Some(product) => (*product).to_string(),
        // An operating system the catalogue publishes no product for at all: the operating
        // system itself is the most specific thing known about the node's platform then.
        None => operating_system.to_string(),
    }
}

/// The products Patch Manager publishes OPERATING-SYSTEM patches for under `operating_system`,
/// in the catalogue's own ascending order. The application patch sets are left out: a node's
/// product is a release of its operating system, not a Microsoft application installed on it.
fn published_products(operating_system: &str) -> Vec<&'static str> {
    PUBLISHED_PATCH_METADATA
        .iter()
        .filter(|group| {
            group.operating_system == operating_system && group.patch_set == PATCH_SET_OS
        })
        .flat_map(|group| group.products.iter().copied())
        .collect()
}

/// `text` with everything but its letters and digits dropped, lowercased: the one form in
/// which a published product name (`WindowsServer2019`) and the places a release is named
/// (`Windows_Server-2019-English-Full-Base`, `Amazon Linux` + `2`) can be compared at all.
fn normalized(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// The snapshot document itself: the node it was taken for, the baseline it is of, and the
/// patches that baseline makes effective at `at` — what a node downloading the snapshot
/// installs from.
///
/// Every value is read off the baseline the store holds and the catalogue Patch Manager
/// publishes, so the document a download serves is the one the snapshot was taken of.
#[must_use]
pub fn snapshot_document(
    region: &str,
    instance_id: &str,
    snapshot_id: &str,
    product: &str,
    baseline: &PatchBaselineRecord,
    at: SystemTime,
) -> String {
    let patches: Vec<::serde_json::Value> = super::patch_catalog::effective_patches(baseline, at)
        .iter()
        .map(|effective| {
            ::serde_json::json!({
                "id": effective.patch.id,
                "title": effective.patch.title,
                "product": effective.patch.product,
                "classification": effective.patch.classification,
                "severity": effective.patch.severity,
                "releaseDate": instant(effective.patch.release_date),
                "deploymentStatus": effective.deployment_status,
                "complianceLevel": effective.compliance_level,
            })
        })
        .collect();
    ::serde_json::json!({
        "snapshotId": snapshot_id,
        "instanceId": instance_id,
        "region": region,
        "accountId": ::engine::emulator_account_id(),
        "product": product,
        "createdTime": instant(at),
        "patchBaseline": {
            "baselineId": baseline.baseline_id,
            "name": baseline.name,
            "operatingSystem": baseline.operating_system,
            "approvedPatches": baseline.approved_patches,
            "approvedPatchesComplianceLevel": baseline.approved_patches_compliance_level,
            "approvedPatchesEnableNonSecurity": baseline.approved_patches_enable_non_security,
            "rejectedPatches": baseline.rejected_patches,
            "rejectedPatchesAction": baseline.rejected_patches_action,
        },
        "patches": patches,
    })
    .to_string()
}

/// One instant of the document, rendered the way this service renders a timestamp a body
/// carries as text (`::aws_smithy_types::date_time::Format::DateTime`).
fn instant(at: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}
