// SPDX-License-Identifier: CC0-1.0

//! The HADOOP a cluster's nodes run: the version its release installs, and the configuration
//! that Hadoop resolves on those nodes.
//!
//! # The rule this module implements
//!
//! "The Amazon EMR release … determines the version of the open-source application packages
//! installed on the cluster" (`RunJobFlow$ReleaseLabel` doc), and Hadoop is the framework an
//! Amazon EMR cluster runs: so the Hadoop a cluster's nodes have installed, and the version
//! `hadoop version` prints on them, come off the ONE release catalog this service answers
//! `Cluster$Applications` out of ([`release_application_version`]) rather than out of anything
//! a request carried. A cluster whose release this catalog does not know installs no Hadoop
//! this emulator can account for, and its nodes then provide no Hadoop command at all.
//!
//! Hadoop reads its settings out of its own configuration files, which Amazon EMR writes onto
//! the nodes: "you can override the default configurations for applications by supplying a
//! configuration object" (`RunJobFlow$Configurations` doc, and the Release Guide's "Configuring
//! applications" page). So a property is resolved here the way it is resolved on a node: the
//! value the cluster's own configuration tree sets wins ([`configuration_property`]), and
//! otherwise it is the DEFAULT Amazon EMR writes for a cluster of this shape
//! ([`EMR_HADOOP_DEFAULTS`]) — and a property neither sets is one the configuration does not
//! carry, which is what `hdfs getconf` reports as missing.

use super::*;

/// The application whose binaries this module answers for, as the catalog spells it.
pub const HADOOP_APPLICATION_NAME: &str = "Hadoop";

/// The filesystem implementation `s3://` and `s3n://` paths resolve to on an Amazon EMR
/// cluster: EMRFS, "an implementation of HDFS that all Amazon EMR clusters use for reading and
/// writing regular files from Amazon EMR directly to Amazon S3" (Amazon EMR Management Guide),
/// whose class is the value Amazon EMR writes for `fs.s3.impl`.
pub const EMR_FILE_SYSTEM_CLASS: &str = "com.amazon.ws.emr.hadoop.fs.EmrFileSystem";

/// The filesystem implementation an `s3a://` path resolves to: Hadoop's own S3A connector,
/// which Amazon EMR leaves as Hadoop ships it.
pub const S3A_FILE_SYSTEM_CLASS: &str = "org.apache.hadoop.fs.s3a.S3AFileSystem";

/// `fs.defaultFS`: the filesystem a path with no scheme names, which on an Amazon EMR cluster
/// is the cluster's own HDFS on its primary node.
pub const DEFAULT_FILE_SYSTEM_PROPERTY: &str = "fs.defaultFS";

/// `dfs.replication`: "the number of copies of each block" HDFS keeps (Amazon EMR Release
/// Guide, "HDFS configuration").
pub const REPLICATION_PROPERTY: &str = "dfs.replication";

/// `dfs.blocksize`: the block size HDFS writes a file in.
pub const BLOCK_SIZE_PROPERTY: &str = "dfs.blocksize";

/// The port the HDFS NameNode of a cluster's primary node serves on, which is what
/// `fs.defaultFS` addresses it at (Amazon EMR Management Guide, "View web interfaces hosted on
/// Amazon EMR clusters").
const HDFS_NAME_NODE_PORT: u16 = 8020;

/// The HDFS block size an Amazon EMR cluster writes under: 128 MiB, which is Hadoop's own
/// default for `dfs.blocksize`.
const HDFS_BLOCK_SIZE_BYTES: u64 = 134_217_728;

/// The REPLICATION Amazon EMR writes for a cluster by the number of nodes it has: "1 for
/// clusters with less than four nodes, 2 for clusters with less than ten nodes, 3 for all other
/// clusters" (Amazon EMR Release Guide, "HDFS configuration" — the default of
/// [`REPLICATION_PROPERTY`]). CATALOG DATA: the published bands, as `(nodes below, copies)`
/// with the last entry the band every larger cluster falls in.
const REPLICATION_BANDS: [(usize, u32); 2] = [(4, 1), (10, 2)];

/// The copies every cluster at or above the last band keeps.
const REPLICATION_ABOVE_THE_BANDS: u32 = 3;

/// The configuration Hadoop resolves on a cluster's nodes.
#[derive(Debug, Clone)]
pub struct HadoopConfiguration {
    /// The version of Hadoop the cluster's release installs, and `None` for a cluster whose
    /// release installs none this emulator knows of.
    version: Option<String>,
    /// The cluster's own configuration tree, which is what overrides a default.
    configurations: Vec<Configuration>,
    /// The values Amazon EMR writes for a cluster of this shape.
    defaults: BTreeMap<String, String>,
}

/// The Hadoop configuration the nodes of `cluster` resolve.
#[must_use]
pub fn cluster_hadoop_configuration(cluster: &ClusterRecord) -> HadoopConfiguration {
    let replication = default_replication(cluster.node_count());
    let defaults = BTreeMap::from([
        (
            DEFAULT_FILE_SYSTEM_PROPERTY.to_string(),
            format!(
                "{HDFS_SCHEME}{}:{HDFS_NAME_NODE_PORT}",
                cluster.master_dns_name
            ),
        ),
        ("fs.s3.impl".to_string(), EMR_FILE_SYSTEM_CLASS.to_string()),
        ("fs.s3n.impl".to_string(), EMR_FILE_SYSTEM_CLASS.to_string()),
        ("fs.s3a.impl".to_string(), S3A_FILE_SYSTEM_CLASS.to_string()),
        (REPLICATION_PROPERTY.to_string(), replication.to_string()),
        (
            BLOCK_SIZE_PROPERTY.to_string(),
            HDFS_BLOCK_SIZE_BYTES.to_string(),
        ),
    ]);
    HadoopConfiguration {
        version: cluster
            .release_label
            .as_deref()
            .and_then(|label| release_application_version(label, HADOOP_APPLICATION_NAME))
            .map(str::to_string),
        configurations: cluster.configurations.clone(),
        defaults,
    }
}

impl HadoopConfiguration {
    /// The version of Hadoop installed on the cluster's nodes, and `None` when the cluster
    /// installs none this emulator can account for.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The value `key` carries on the nodes: the one the cluster's own configuration sets, the
    /// default Amazon EMR writes for it, or `None` for a property the configuration does not
    /// carry at all.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<String> {
        if let Some(configured) = configuration_property(&self.configurations, key) {
            return Some(configured.clone());
        }
        self.defaults.get(key).cloned()
    }

    /// How many copies of each block HDFS keeps on this cluster, as the resolved
    /// [`REPLICATION_PROPERTY`] states it — what a listing and a `-stat` of an HDFS file
    /// report.
    #[must_use]
    pub fn replication(&self) -> u32 {
        self.value(REPLICATION_PROPERTY)
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(1)
    }

    /// The block size HDFS writes a file of this cluster in, as the resolved
    /// [`BLOCK_SIZE_PROPERTY`] states it.
    #[must_use]
    pub fn block_size(&self) -> u64 {
        self.value(BLOCK_SIZE_PROPERTY)
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(HDFS_BLOCK_SIZE_BYTES)
    }
}

/// The copies of each block a cluster of `nodes` nodes keeps, as the published bands state it.
#[must_use]
pub fn default_replication(nodes: usize) -> u32 {
    REPLICATION_BANDS
        .into_iter()
        .find_map(|(below, copies)| (nodes < below).then_some(copies))
        .unwrap_or(REPLICATION_ABOVE_THE_BANDS)
}

#[cfg(test)]
mod tests {
    use super::{default_replication, REPLICATION_ABOVE_THE_BANDS};

    /// RULE, at each published bound: the copies of each block an Amazon EMR cluster's HDFS
    /// keeps follow from the number of nodes it has — "1 for clusters with less than four
    /// nodes, 2 for clusters with less than ten nodes, 3 for all other clusters".
    ///
    /// The rows are each band's own sizes and the first size past it.
    #[test]
    fn the_published_bands_decide_the_replication() {
        // (case, nodes, copies)
        let rows: [(&str, usize, u32); 7] = [
            ("one node", 1, 1),
            ("the last size of the first band", 3, 1),
            ("the first size of the second band", 4, 2),
            ("the last size of the second band", 9, 2),
            ("the first size above the bands", 10, REPLICATION_ABOVE_THE_BANDS),
            ("well above the bands", 64, REPLICATION_ABOVE_THE_BANDS),
            ("no node at all", 0, 1),
        ];
        for (case, nodes, copies) in rows {
            let answered = default_replication(nodes);
            assert_eq!(
                answered, copies,
                "case {case:?} ({nodes} nodes): expected {copies} copies, got {answered}"
            );
        }
    }
}
