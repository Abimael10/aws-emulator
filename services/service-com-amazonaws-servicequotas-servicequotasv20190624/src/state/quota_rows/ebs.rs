// SPDX-License-Identifier: CC0-1.0

//! Amazon Elastic Block Store's published default-quota catalog.

use super::*;

/// The `AWS/Usage` display name Amazon EBS publishes its API call counts under, which
/// is the `Service` dimension of every metered EBS quota.
const EBS_USAGE_SERVICE: &str = "EBS";

/// One of the EBS direct-API request planes: so many requests of one operation PER SECOND
/// against one scope — a single snapshot, or the account.
///
/// SOURCE: the recorded default-quota listing of `ebs`, whose block-write and
/// block-listing rows are published as `<Api> requests per <scope>` with the sentence
/// `The maximum number of <Api> requests allowed per <scope>.`, over a one-second window
/// and with no usage metric, and are NOT adjustable. One constructor fills the class so two
/// of its rows cannot drift apart.
fn direct_api_rate(code: &str, api: &str, scope: &str, per_second: f64) -> QuotaRow {
    quota(format!("{api} requests per {scope}"), per_second)
        .code(code)
        .fixed()
        .period(1, "SECOND")
        .describe(&format!(
            "The maximum number of {api} requests allowed per {scope}."
        ))
}

/// The sentence Amazon EBS publishes for one of its AGGREGATED planes: how much of a
/// resource all volumes of one type in the Region may hold together.
///
/// SOURCE: the recorded default-quota listing of `ebs`, whose gp2 storage-modification row
/// reads `The maximum aggregated amount of storage, in TiB, that can be requested in volume
/// modifications across General Purpose SSD (gp2) volumes in this Region.` The class is that
/// one sentence around what the plane aggregates — with the unit it is counted in where the
/// plane states one — whether the plane is the storage itself or the modifications requested
/// of it, and the volume type it is counted over, so it is built once here.
fn aggregated_description(aggregated: &str, requested_for: &str, volume_type: &str) -> String {
    format!(
        "The maximum aggregated amount of {aggregated} that can be requested \
         {requested_for}across {volume_type} volumes in this Region."
    )
}

/// Amazon Elastic Block Store's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `ebs` service code (the "Amazon
/// EBS quotas" page of the Amazon EBS User Guide) for the storage, IOPS and snapshot
/// planes it publishes, and the recorded default-quota listing of that code for which
/// `QuotaCode` names which quota and for the per-API request rates it publishes beside
/// them. EBS publishes a storage plane PER VOLUME TYPE — a volume type is what the
/// capacity is bought under — so a storage row is named for the type, and names the TiB
/// the capacity is counted in IN THAT NAME rather than in a `Unit` of its own, which is
/// what the modification plane of a type does as well.
/// A request-rate row carries the one-second window and the `AWS/Usage` `CallCount` of
/// the operation it throttles ([`request_rate`]); the EBS direct APIs are published as
/// their own class ([`direct_api_rate`]), over the same window but without a usage metric
/// and against the scope the rate is counted on.
///
/// Adjustability is published PER CODE and is not a property of the class: the recorded
/// listing reports this service's rate planes as NOT adjustable except the ones whose codes
/// it reports the other way round, so each row carries the adjustability the listing reports
/// for its own code rather than the one its constructor defaults to.
/// A quota of this service the data available here names no code for is NOT published
/// under a neighbouring code or a stand-in — a code identifies ONE quota (see
/// [`super::descriptive_quota_code`]) — so the snapshot plane of the Region and the
/// per-API rates whose codes are not among the recorded ones are left to the
/// default-quota-set gap. The converse holds as well, and for the same reason: the recorded
/// listing reports `L-C125AE42` and `L-DB2FBAA1` with members no quota this catalog can
/// name carries (a one-second window, and the opposite adjustability), so those two codes
/// name quotas this catalog cannot cite and are left to the gap rather than carrying the
/// attachment plane of a Multi-Attach enabled volume and the fast-snapshot-restore plane of
/// the Region, which are left to it with them.
pub(in crate::state) fn ebs_quotas() -> Vec<QuotaRow> {
    let rate =
        |code: &str, api: &str, per_second: f64| request_rate(code, EBS_USAGE_SERVICE, api, per_second);
    // A storage plane of one volume type: the capacity, in TiB, of all volumes of that
    // type in the Region. The TiB is named in the quota's own name, so the row carries no
    // `Unit` of its own.
    let storage = |code: &str, volume_type: &str, tebibytes: f64| {
        quota(format!("Storage for {volume_type} volumes, in TiB"), tebibytes)
            .code(code)
            .describe(&aggregated_description("storage, in TiB,", "", volume_type))
    };
    // The storage all volumes of one type may be MODIFIED to in the Region, which the
    // service publishes beside the storage plane itself. The TiB it is counted in is named
    // in the quota's own name, so the row carries no `Unit` of its own either.
    let storage_modifications = |code: &str, volume_type: &str, tebibytes: f64| {
        quota(
            format!("Storage modifications for {volume_type} volumes, in TiB"),
            tebibytes,
        )
        .code(code)
        .describe(&aggregated_description(
            "storage, in TiB,",
            "in volume modifications ",
            volume_type,
        ))
    };
    // An IOPS plane of one volume type: the provisioned IOPS of all volumes of that
    // type in the Region.
    let iops = |code: &str, volume_type: &str, operations_per_second: f64| {
        quota(
            format!("IOPS for {volume_type} volumes"),
            operations_per_second,
        )
        .code(code)
        .describe(&aggregated_description("IOPS", "", volume_type))
    };
    vec![
        // The one volume-attachment rate AWS publishes as not adjustable.
        rate("L-028ACFB9", "AttachVolume", 100.0).fixed(),
        rate("L-07399329", "CompleteSnapshot", 100.0),
        rate("L-09BD8365", "CopySnapshot", 10.0),
        // The two EBS direct-API request planes the service publishes as FIXED: the
        // block-write rate of one snapshot and the block-listing rate of the account.
        // The block-listing plane keeps the value the recorded listing reports for its
        // code, which is the one this catalog already served there.
        direct_api_rate("L-1774F84A", "PutSnapshotBlock", "snapshot", 1000.0),
        direct_api_rate("L-18E976AB", "ListSnapshotBlocks", "account", 50.0),
        rate("L-1D4D9345", "CreateVolume", 100.0).fixed(),
        storage("L-17AF77E8", "Throughput Optimized HDD (st1)", 50.0),
        rate("L-309BACF6", "DeleteSnapshot", 100.0),
        rate("L-35B31D98", "DeleteVolume", 100.0),
        // The volume-copy plane of the account: how many copies may run AT ONCE, which
        // is a concurrency and not one of the per-API rates beside it. The service words
        // this one as a maximum of its own rather than in the counted-plane voice.
        quota("Max concurrent copy volume operations per account", 5.0)
            .code("L-39BD5252")
            .describe(
                "Maximum number of concurrent copy volume operations allowed per account in \
                 this region.",
            ),
        rate("L-3A0E616D", "DescribeSnapshots", 100.0),
        rate("L-59C8FC87", "DescribeVolumeStatus", 100.0),
        rate("L-5F80CA91", "DescribeVolumes", 100.0),
        rate("L-631ECBD3", "DescribeVolumesModifications", 100.0),
        rate("L-651D1834", "DetachVolume", 100.0),
        rate("L-750405C3", "DisableFastSnapshotRestores", 10.0).fixed(),
        storage("L-7A658B76", "General Purpose SSD (gp3)", 50.0),
        rate("L-82ACEF56", "EnableFastSnapshotRestores", 10.0),
        // The EBS direct APIs: the block-level read and write rates of a snapshot,
        // which AWS publishes far above its control-plane rates.
        rate("L-835364B2", "GetSnapshotBlock", 1000.0).fixed(),
        iops("L-8656991D", "Provisioned IOPS SSD (io2)", 100_000.0).fixed(),
        // The storage-modification plane of the Throughput Optimized HDD type, which the
        // recorded listing publishes under this code; the `ListChangedBlocks` rate this
        // catalog had served here names no other code that listing states, so it is left to
        // the default-quota-set gap.
        storage_modifications("L-87C9DEA6", "Throughput Optimized HDD (st1)", 500.0),
        storage("L-8D977E7E", "Provisioned IOPS SSD (io2)", 20.0),
        rate("L-915A3DBB", "ListSnapshotBlocks", 100.0).fixed(),
        rate("L-94D7AB7D", "LockSnapshot", 10.0).fixed(),
        rate("L-98A0B26D", "ModifySnapshotAttribute", 100.0),
        storage("L-9A0E0F82", "Magnetic (standard)", 20.0),
        storage("L-9CF3C2EB", "Cold HDD (sc1)", 50.0),
        rate("L-9F6E7C4E", "ModifySnapshotTier", 10.0).fixed(),
        rate("L-A37D9CF3", "ModifyVolume", 100.0),
        rate("L-AFAE1BE8", "PutSnapshotBlock", 1000.0),
        rate("L-AFEBDF7A", "ResetSnapshotAttribute", 100.0).fixed(),
        iops("L-B3A130E6", "Provisioned IOPS SSD (io1)", 300_000.0),
        rate("L-B9F7C487", "RestoreSnapshotTier", 10.0),
        quota("In-progress snapshot archives per account", 5.0)
            .code("L-D0291BE3")
            .fixed()
            .describe(
                "The maximum number of snapshot archives that can be in progress at once in \
                 this account in the current Region.",
            ),
        storage("L-D18FCD1D", "General Purpose SSD (gp2)", 50.0),
        rate("L-D8F37C68", "StartSnapshot", 10.0).fixed(),
        quota("Concurrent snapshot copies per destination Region", 20.0)
            .code("L-DB70D580")
            .fixed()
            .describe(
                "The maximum number of snapshot copies that can be in progress at once to a \
                 single destination Region.",
            ),
        rate("L-E137849C", "UnlockSnapshot", 10.0),
        quota("Archived snapshots per volume", 25.0)
            .code("L-E20676C1")
            .describe(
                "The maximum number of snapshots of one volume that can be in the archive \
                 storage tier.",
            ),
        // The storage a volume type's volumes may be MODIFIED to, published beside the
        // storage plane of the same type.
        storage_modifications("L-F06E64A8", "General Purpose SSD (gp2)", 500.0),
        storage("L-FD252861", "Provisioned IOPS SSD (io1)", 50.0),
    ]
}
