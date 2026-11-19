// SPDX-License-Identifier: CC0-1.0

//! AWS Storage Gateway's published default-quota catalog.

use super::*;

/// AWS Storage Gateway's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `storagegateway` service code
/// (what `aws service-quotas list-aws-default-service-quotas --service-code
/// storagegateway` reports, which is the "AWS Storage Gateway quotas" pages of its user
/// guide), and the `QuotaCode`s that catalog is keyed on.
///
/// Storage Gateway publishes its quotas PER GATEWAY TYPE — the volumes a volume gateway
/// serves, the shares a file gateway serves and the virtual tapes a tape gateway serves —
/// which is why most rows are named for the plane of one gateway rather than for the
/// account. A row that measures a capacity carries the unit AWS publishes it in rather
/// than being a bare count, except where the service names the unit IN the quota's own
/// name and publishes `Unit` `None` for it.
///
/// The recorded listing states the quota published under `L-311F8856` (the largest virtual
/// tape, 15, with the unit in the name), under `L-6D83F84B` (the cached volume gateway's
/// upload-buffer minimum, 150), under `L-87640A7E` (the tape gateway's upload-buffer
/// minimum, 150), under `L-F3EDDF8C` (the tape gateway's cache minimum, 150), under
/// `L-F5915598` (the file gateway's cache MAXIMUM, 64, with the unit in the name), under
/// `L-4951D254` (the total size of the tapes of one virtual tape library, 1, with the unit
/// in the name) and under `L-C86341FE` (the longest path, 1024 Bytes), beside the sentence
/// the service words each of those with. The stored-volume size, the virtual-tape count,
/// the cache-disk count, the tag count, the snapshot-schedule volume count, the cached
/// volume storage of a gateway and the automatic tape creation policy count this catalog
/// used to key on those seven codes are NOT published here, and neither is the code it used
/// to key the virtual-tape size on (`L-6F75AC83`): no source available here states AWS's
/// code for the first group or AWS's quota for the last one, so they are left to the
/// default-quota-set gap rather than served under another quota's code. A row whose
/// published `Description` no source available here states carries none, rather than a
/// sentence this emulator words itself.
pub(in crate::state) fn storagegateway_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Gateways per Region", 100.0)
            .code("L-14F83003")
            .describe(
                "The maximum number of gateways in this account in the current Region.",
            ),
        quota("Cached volumes per gateway", 32.0)
            .code("L-2232E8E3")
            .fixed()
            .describe("The maximum number of cached volumes per volume gateway."),
        quota("Stored volumes per gateway", 32.0)
            .code("L-2E88EE16")
            .fixed()
            .describe("The maximum number of stored volumes per volume gateway."),
        quota("Maximum size of a cached volume", 32.0)
            .code("L-2EC26EAB")
            .unit("TiB")
            .fixed()
            .describe("The maximum size, in TiB, of one cached volume."),
        quota("Max size of a virtual tape in TiB", 15.0)
            .code("L-311F8856")
            .fixed()
            .describe("Maximum size of a virtual tape"),
        // The tapes of ONE virtual tape library, whose unit the service names IN the
        // quota's own name and publishes `Unit` `None` for.
        quota("Total size of tapes in a virtual tape library in PiB", 1.0)
            .code("L-4951D254")
            .fixed()
            .describe("Total size of all tapes in a virtual tape library (VTL)"),
        quota("Total stored volume storage per gateway", 512.0)
            .code("L-5308FBCA")
            .unit("TiB")
            .fixed()
            .describe(
                "The maximum total size, in TiB, of the stored volumes of one volume \
                 gateway.",
            ),
        quota("File shares per file gateway", 50.0)
            .code("L-58DCC961")
            .fixed()
            .describe("The maximum number of file shares per file gateway."),
        quota("Maximum size of a file", 5.0)
            .code("L-59D49F15")
            .unit("TiB")
            .fixed()
            .describe("The maximum size, in TiB, of one file written through a file share."),
        quota("Cached volume gateway Upload Buffer Minimum in GiB", 150.0)
            .code("L-6D83F84B")
            .fixed()
            .describe("Minimum upload buffer size for Cached Volume Gateway"),
        quota("Minimum size of a virtual tape", 100.0)
            .code("L-761F1680")
            .unit("Gigabytes")
            .fixed()
            .describe("The minimum size, in GB, of one virtual tape."),
        quota("Total virtual tape storage per gateway", 1024.0)
            .code("L-7B01AFFB")
            .unit("TiB")
            .fixed()
            .describe(
                "The maximum total size, in TiB, of the virtual tapes of one tape gateway.",
            ),
        quota("Virtual tapes in the tape archive", 10_000.0)
            .code("L-803470D8")
            .describe("The maximum number of virtual tapes in the tape archive."),
        quota("Snapshot schedules per volume", 1.0)
            .code("L-81A6E497")
            .fixed()
            .describe("The maximum number of snapshot schedules per volume."),
        quota("Tape gateway Upload Buffer Minimum in GiB", 150.0)
            .code("L-87640A7E")
            .fixed()
            .describe("Minimum upload buffer size for Tape Gateway"),
        quota("Upload buffer disks per gateway", 20.0)
            .code("L-96D17DE9")
            .fixed()
            .describe(
                "The maximum number of local disks used as upload buffer by one gateway.",
            ),
        quota("Cache storage per gateway", 64.0)
            .code("L-98F44425")
            .unit("TiB")
            .fixed()
            .describe("The maximum cache storage, in TiB, of one gateway."),
        quota("Upload buffer storage per gateway", 2.0)
            .code("L-99E991AF")
            .unit("TiB")
            .fixed()
            .describe("The maximum upload buffer storage, in TiB, of one gateway."),
        quota("Custom tape pools per account", 100.0)
            .code("L-BD85E1A6")
            .describe(
                "The maximum number of custom tape pools in this account in the current \
                 Region.",
            ),
        // The longest path a gateway serves, which the service publishes in Bytes.
        quota("Path length", 1024.0)
            .code("L-C86341FE")
            .unit("Bytes")
            .fixed()
            .describe("Maximum path length"),
        quota("Bandwidth rate limit schedule intervals per gateway", 20.0)
            .code("L-CEFD39A0")
            .fixed()
            .describe(
                "The maximum number of intervals in the bandwidth rate limit schedule of \
                 one gateway.",
            ),
        quota("Clients per NFS file share", 100.0)
            .code("L-E0AEEBAC")
            .fixed()
            .describe("The maximum number of clients allowed by one NFS file share."),
        quota("Tape gateway Cache Minimum in GiB", 150.0)
            .code("L-F3EDDF8C")
            .fixed()
            .describe("Minimum cache size for Tape Gateway"),
        // The cache of ONE file gateway, whose unit the service names IN the quota's own
        // name and publishes `Unit` `None` for.
        quota("File gateway Cache Maximum in TiB", 64.0)
            .code("L-F5915598")
            .fixed()
            .describe("Maximum cache size for File Gateway"),
        quota("Audit logs per file share", 1.0)
            .code("L-FF1BE522")
            .fixed()
            .describe("The maximum number of audit log destinations per file share."),
    ]
}
