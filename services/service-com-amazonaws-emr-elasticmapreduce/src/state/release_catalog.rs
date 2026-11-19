// SPDX-License-Identifier: CC0-1.0

//! The Amazon EMR RELEASE CATALOG: which releases this emulator offers, what each of them
//! bundles, and the Amazon Linux releases they are built on.
//!
//! One concern of this service's state, split out of [`super`]: `DescribeReleaseLabel` and
//! `ListReleaseLabels` publish this catalog, a cluster's `ReleaseLabel` is resolved in it,
//! and `Cluster$Applications` report the versions the cluster's release ships
//! ([`super::installed_applications`]) — so the one table lives here and every reader reaches
//! it through `crate::state`.

use super::*;

/// One Amazon EMR release line (`emr-<major>.<minor>`) and the patch releases published
/// on it. Each `(line, patch)` pair is one label of the `emr-<major>.<minor>.<patch>`
/// series; the labels of another form are published beside it ([`PRE_RELEASE_LABELS`],
/// [`SPARK_RELEASE_LINES`]).
#[derive(Debug, Clone, Copy)]
pub struct ReleaseLine {
    pub major: u32,
    pub minor: u32,
    pub patches: &'static [u32],
}

/// Every release line this emulator offers, oldest first.
///
/// The catalog publishes EVERY release of every line Amazon EMR still lists, at the patch
/// level it publishes them — the series a client reads out of `ListReleaseLabels` to choose
/// a `ReleaseLabel` from, so a line missing here is a release no caller can discover and no
/// launch can name. Source for the lines and their patch releases is the Amazon EMR Release
/// Guide's release-version table, family by family; the labels Amazon EMR no longer lists
/// (the 3.x series, and the 4.0/4.1 and 4.7 lines) are not published and so are not here.
pub const RELEASE_LINES: &[ReleaseLine] = &[
    // The 4.x series: the oldest labels Amazon EMR still publishes.
    ReleaseLine { major: 4, minor: 2, patches: &[0] },
    ReleaseLine { major: 4, minor: 3, patches: &[0] },
    ReleaseLine { major: 4, minor: 4, patches: &[0] },
    ReleaseLine { major: 4, minor: 5, patches: &[0] },
    ReleaseLine { major: 4, minor: 6, patches: &[0] },
    // The 4.8 and 4.9 lines are published from patch 2 and patch 1 on: the patches below
    // those are not among the labels Amazon EMR lists for the series.
    ReleaseLine { major: 4, minor: 8, patches: &[2, 3, 4, 5] },
    ReleaseLine { major: 4, minor: 9, patches: &[1, 2, 3, 4, 5, 6] },
    // The 5.x series, from its first release to its last.
    // The 5.0 line is published as its first release and its third patch; the patches
    // between are not among the labels Amazon EMR lists for the series.
    ReleaseLine { major: 5, minor: 0, patches: &[0, 3] },
    ReleaseLine { major: 5, minor: 1, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 2, patches: &[0, 1, 2, 3] },
    ReleaseLine { major: 5, minor: 3, patches: &[0, 1, 2] },
    ReleaseLine { major: 5, minor: 4, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 5, patches: &[0, 1, 2, 3, 4] },
    ReleaseLine { major: 5, minor: 6, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 7, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 8, patches: &[0, 1, 2, 3] },
    ReleaseLine { major: 5, minor: 9, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 10, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 11, patches: &[0, 1, 2, 3, 4] },
    ReleaseLine { major: 5, minor: 12, patches: &[0, 1, 2, 3] },
    ReleaseLine { major: 5, minor: 13, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 14, patches: &[0, 1, 2] },
    ReleaseLine { major: 5, minor: 15, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 16, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 17, patches: &[0, 1, 2] },
    ReleaseLine { major: 5, minor: 18, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 19, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 20, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 21, patches: &[0, 1, 2] },
    ReleaseLine { major: 5, minor: 22, patches: &[0] },
    ReleaseLine { major: 5, minor: 23, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 24, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 25, patches: &[0] },
    ReleaseLine { major: 5, minor: 26, patches: &[0] },
    ReleaseLine { major: 5, minor: 27, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 28, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 29, patches: &[0] },
    ReleaseLine { major: 5, minor: 30, patches: &[0, 1, 2] },
    ReleaseLine { major: 5, minor: 31, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 32, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 33, patches: &[0, 1] },
    ReleaseLine { major: 5, minor: 34, patches: &[0] },
    ReleaseLine { major: 5, minor: 35, patches: &[0] },
    ReleaseLine { major: 5, minor: 36, patches: &[0, 1, 2] },
    ReleaseLine { major: 6, minor: 0, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 1, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 2, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 3, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 4, patches: &[0] },
    ReleaseLine { major: 6, minor: 5, patches: &[0] },
    ReleaseLine { major: 6, minor: 6, patches: &[0] },
    ReleaseLine { major: 6, minor: 7, patches: &[0] },
    ReleaseLine { major: 6, minor: 8, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 9, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 10, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 11, patches: &[0, 1] },
    ReleaseLine { major: 6, minor: 12, patches: &[0] },
    ReleaseLine { major: 6, minor: 13, patches: &[0] },
    ReleaseLine { major: 6, minor: 14, patches: &[0] },
    ReleaseLine { major: 6, minor: 15, patches: &[0] },
    ReleaseLine { major: 7, minor: 0, patches: &[0] },
    ReleaseLine { major: 7, minor: 1, patches: &[0] },
    ReleaseLine { major: 7, minor: 2, patches: &[0] },
    ReleaseLine { major: 7, minor: 3, patches: &[0] },
    ReleaseLine { major: 7, minor: 4, patches: &[0] },
    ReleaseLine { major: 7, minor: 5, patches: &[0] },
    // The lines published since: Amazon EMR offers every release of the series, so a
    // launch naming one of them resolves rather than being refused. Source for the labels
    // is the Release Guide's release-version table, as for every line above; each bundles
    // the application versions the newest row at or before its line carries
    // ([`CatalogApplication::version_at`]), which is the rule that already gives a patch
    // release its versions.
    ReleaseLine { major: 7, minor: 6, patches: &[0] },
    ReleaseLine { major: 7, minor: 7, patches: &[0] },
    ReleaseLine { major: 7, minor: 8, patches: &[0] },
    ReleaseLine { major: 7, minor: 9, patches: &[0] },
    ReleaseLine { major: 7, minor: 10, patches: &[0] },
    ReleaseLine { major: 7, minor: 11, patches: &[0] },
    ReleaseLine { major: 7, minor: 12, patches: &[0] },
    ReleaseLine { major: 7, minor: 13, patches: &[0] },
    ReleaseLine { major: 7, minor: 14, patches: &[0] },
];

/// One PRE-GA release label the catalog publishes: a release Amazon EMR published before
/// the general-availability release of the same version, whose label carries a qualifier
/// after the version (`emr-6.0.0-beta`).
///
/// Such a label is NOT of the `emr-<major>.<minor>.<patch>` form, so it cannot be stated
/// as a patch of a [`ReleaseLine`]; it is published beside its line and resolves to it, so
/// a pre-GA release reports what its line bundles.
#[derive(Debug, Clone, Copy)]
pub struct PreReleaseLabel {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// What follows the version in the label, after a hyphen: `beta`, `beta2`.
    pub qualifier: &'static str,
}

impl PreReleaseLabel {
    /// The label of this pre-GA release, as Amazon EMR spells it.
    pub fn label(&self) -> String {
        pre_release_label(self.major, self.minor, self.patch, self.qualifier)
    }

    /// The release line this pre-GA release belongs to.
    pub fn release(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }
}

/// Every pre-GA release label the catalog publishes, oldest qualifier first.
///
/// Source is the Release Guide's release-version table, the same table [`RELEASE_LINES`]
/// comes from: the 6.0 line was published as two beta releases before its GA release, and
/// both labels are still listed.
pub const PRE_RELEASE_LABELS: &[PreReleaseLabel] = &[
    PreReleaseLabel { major: 6, minor: 0, patch: 0, qualifier: "beta" },
    PreReleaseLabel { major: 6, minor: 0, patch: 0, qualifier: "beta2" },
];

/// The label of the pre-GA release `major.minor.patch-qualifier`, as Amazon EMR spells it.
pub fn pre_release_label(major: u32, minor: u32, patch: u32, qualifier: &str) -> String {
    format!("{}-{qualifier}", release_label(major, minor, patch))
}

/// The release a pre-GA label names, when the catalog publishes it.
///
/// Spelled exactly as [`PreReleaseLabel::label`] renders it, so a differently cased or
/// padded variant names no release.
pub fn pre_release(label: &str) -> Option<(u32, u32, u32)> {
    PRE_RELEASE_LABELS
        .iter()
        .find(|pre_release| pre_release.label() == label)
        .map(PreReleaseLabel::release)
}

/// One application in the catalog.
#[derive(Debug, Clone, Copy)]
pub struct CatalogApplication {
    /// `SimplifiedApplication$Name`: the one identifier Amazon EMR publishes for this
    /// application, reported verbatim and never case-folded.
    pub name: &'static str,
    /// The version this application carries from a release line onward: a row applies
    /// to every release at or after its line, up to the next row's line. Rows are in
    /// ascending line order, and a release BEFORE the first row predates the
    /// application, which such a release does not bundle at all.
    pub versions: &'static [((u32, u32), &'static str)],
    /// The line from which Amazon EMR no longer bundles this application, when it
    /// dropped it.
    pub removed_from: Option<(u32, u32)>,
}

impl CatalogApplication {
    /// The version this application carries on the release line `(major, minor)`, or
    /// `None` when that release does not bundle it.
    pub fn version_at(&self, major: u32, minor: u32) -> Option<&'static str> {
        if self.removed_from.is_some_and(|from| (major, minor) >= from) {
            return None;
        }
        self.versions
            .iter()
            .rfind(|(line, _)| *line <= (major, minor))
            .map(|&(_, version)| version)
    }
}

/// The applications Amazon EMR bundles, with the version each release line carries.
///
/// The rows reach back to the oldest line the catalog publishes ([`RELEASE_LINES`]), so
/// every release it offers reports what it bundles; the source is the Release Guide's
/// "Application versions" tables, release series by release series.
pub const APPLICATION_CATALOG: &[CatalogApplication] = &[
    CatalogApplication {
        name: "Flink",
        versions: &[
            // Amazon EMR offers Flink from 5.1.0 onward.
            ((5, 1), "1.1.3"),
            ((5, 8), "1.3.1"),
            ((5, 20), "1.6.2"),
            ((5, 25), "1.8.1"),
            ((5, 27), "1.9.0"),
            ((5, 29), "1.9.1"),
            ((5, 30), "1.10.0"),
            ((5, 33), "1.12.1"),
            ((5, 36), "1.14.2"),
            ((6, 0), "1.9.1"),
            ((6, 1), "1.11.2"),
            ((6, 6), "1.14.2"),
            ((6, 9), "1.15.2"),
            ((6, 12), "1.17.0"),
            ((6, 15), "1.17.1"),
            ((7, 0), "1.18.0"),
            ((7, 2), "1.19.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Ganglia",
        versions: &[((4, 2), "3.7.2"), ((5, 30), "3.7.2"), ((6, 0), "3.7.2")],
        // Amazon EMR 7.0.0 no longer bundles Ganglia.
        removed_from: Some((7, 0)),
    },
    CatalogApplication {
        name: "Hadoop",
        versions: &[
            ((4, 2), "2.6.0"),
            ((4, 3), "2.7.1"),
            ((4, 8), "2.7.2"),
            ((4, 9), "2.7.3"),
            ((5, 0), "2.7.2"),
            ((5, 2), "2.7.3"),
            ((5, 12), "2.8.3"),
            ((5, 16), "2.8.4"),
            ((5, 20), "2.8.5"),
            ((5, 30), "2.8.5"),
            ((5, 31), "2.10.0"),
            ((5, 33), "2.10.1"),
            ((6, 0), "3.2.1"),
            ((6, 9), "3.3.3"),
            ((6, 15), "3.3.6"),
            ((7, 5), "3.4.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "HBase",
        versions: &[
            // Amazon EMR offers HBase from 4.6.0 onward.
            ((4, 6), "1.2.0"),
            ((5, 0), "1.2.1"),
            ((5, 7), "1.3.1"),
            ((5, 17), "1.4.4"),
            ((5, 23), "1.4.9"),
            ((5, 28), "1.4.10"),
            ((5, 30), "1.4.13"),
            ((6, 0), "2.2.3"),
            ((6, 6), "2.4.4"),
            ((6, 9), "2.4.13"),
            ((6, 15), "2.4.17"),
            ((7, 2), "2.5.8"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "HCatalog",
        // HCatalog is Hive's table and storage-management layer and ships WITH Hive, so
        // the version a release carries is its Hive version (Release Guide "Application
        // versions" tables, the source of every row here).
        versions: &[
            ((4, 2), "1.0.0"),
            ((5, 0), "2.1.0"),
            ((5, 3), "2.1.1"),
            ((5, 8), "2.3.0"),
            ((5, 18), "2.3.3"),
            ((5, 20), "2.3.4"),
            ((5, 25), "2.3.5"),
            ((5, 28), "2.3.6"),
            ((5, 30), "2.3.6"),
            ((5, 33), "2.3.7"),
            ((5, 36), "2.3.9"),
            ((6, 0), "3.1.2"),
            ((6, 9), "3.1.3"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Hive",
        versions: &[
            ((4, 2), "1.0.0"),
            ((5, 0), "2.1.0"),
            ((5, 3), "2.1.1"),
            ((5, 8), "2.3.0"),
            ((5, 18), "2.3.3"),
            ((5, 20), "2.3.4"),
            ((5, 25), "2.3.5"),
            ((5, 28), "2.3.6"),
            ((5, 30), "2.3.6"),
            ((5, 33), "2.3.7"),
            ((5, 36), "2.3.9"),
            ((6, 0), "3.1.2"),
            ((6, 9), "3.1.3"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Hudi",
        versions: &[
            // Amazon EMR offers Hudi from 5.28.0 onward.
            ((5, 28), "0.5.0-incubating"),
            ((5, 30), "0.5.2"),
            ((5, 36), "0.10.0"),
            ((6, 0), "0.5.0"),
            ((6, 6), "0.10.1"),
            ((6, 9), "0.12.1"),
            ((6, 12), "0.13.1"),
            ((6, 15), "0.14.0"),
            ((7, 2), "0.15.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Hue",
        versions: &[
            ((4, 2), "3.7.1"),
            ((5, 0), "3.10.0"),
            ((5, 8), "4.0.1"),
            ((5, 15), "4.2.0"),
            ((5, 21), "4.4.0"),
            ((5, 30), "4.6.0"),
            ((5, 36), "4.10.0"),
            ((6, 0), "4.4.0"),
            ((6, 6), "4.10.0"),
            ((6, 9), "4.11.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Iceberg",
        // Amazon EMR offers Iceberg from 6.5.0 onward.
        versions: &[
            ((6, 5), "0.11.0"),
            ((6, 9), "0.14.1"),
            ((6, 12), "1.2.1"),
            ((6, 15), "1.4.0"),
            ((7, 0), "1.4.2"),
            ((7, 2), "1.5.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "JupyterEnterpriseGateway",
        versions: &[((6, 0), "2.1.0"), ((6, 9), "2.6.0")],
        removed_from: None,
    },
    CatalogApplication {
        name: "JupyterHub",
        versions: &[
            // Amazon EMR offers JupyterHub from 5.14.0 onward.
            ((5, 14), "0.8.1"),
            ((5, 24), "0.9.6"),
            ((5, 30), "1.1.0"),
            ((5, 36), "1.4.1"),
            ((6, 0), "1.1.0"),
            ((6, 6), "1.4.1"),
            ((6, 15), "1.5.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Livy",
        versions: &[
            // Amazon EMR offers Livy from 5.9.0 onward.
            ((5, 9), "0.4.0"),
            ((5, 20), "0.5.0"),
            ((5, 25), "0.6.0"),
            ((5, 30), "0.7.0"),
            ((5, 36), "0.7.1"),
            ((6, 0), "0.7.0"),
            ((6, 6), "0.7.1"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "MXNet",
        versions: &[
            // Amazon EMR offers MXNet from 5.10.0 onward.
            ((5, 10), "0.12.0"),
            ((5, 19), "1.3.1"),
            ((5, 24), "1.4.0"),
            ((5, 30), "1.6.0"),
            ((5, 36), "1.8.0"),
            ((6, 0), "1.5.1"),
            ((6, 1), "1.6.0"),
            ((6, 4), "1.8.0"),
            ((6, 9), "1.9.1"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Oozie",
        versions: &[
            ((4, 2), "4.2.0"),
            ((5, 0), "4.2.0"),
            ((5, 26), "5.1.0"),
            ((5, 30), "5.2.0"),
            ((5, 36), "5.2.1"),
            ((6, 0), "5.1.0"),
            ((6, 1), "5.2.0"),
            ((6, 6), "5.2.1"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Phoenix",
        versions: &[
            // Amazon EMR offers Phoenix from 4.7.0 onward, so the oldest 4.x line that
            // bundles it among the ones the catalog publishes is 4.8.
            ((4, 8), "4.7.0"),
            ((5, 0), "4.7.0"),
            ((5, 8), "4.11.0"),
            ((5, 17), "4.14.0"),
            ((5, 30), "4.14.3"),
            ((6, 0), "5.0.0"),
            ((6, 2), "5.1.2"),
            ((6, 15), "5.1.3"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Pig",
        versions: &[
            ((4, 2), "0.14.0"),
            ((5, 0), "0.16.0"),
            ((5, 9), "0.17.0"),
            ((5, 30), "0.17.0"),
            ((6, 0), "0.17.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Presto",
        versions: &[
            ((4, 2), "0.125"),
            ((4, 6), "0.143"),
            ((4, 9), "0.157"),
            ((5, 0), "0.150"),
            ((5, 8), "0.184"),
            ((5, 20), "0.214"),
            ((5, 25), "0.220"),
            ((5, 28), "0.227"),
            ((5, 30), "0.232"),
            ((5, 36), "0.267"),
            ((6, 0), "0.230"),
            ((6, 6), "0.267"),
            ((6, 9), "0.276"),
            ((6, 15), "0.283"),
            ((7, 0), "0.284"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Spark",
        versions: &[
            ((4, 2), "1.5.2"),
            ((4, 3), "1.6.0"),
            ((4, 4), "1.6.1"),
            ((4, 8), "1.6.2"),
            ((4, 9), "1.6.3"),
            ((5, 0), "2.0.0"),
            ((5, 1), "2.0.1"),
            ((5, 2), "2.0.2"),
            ((5, 3), "2.1.0"),
            ((5, 5), "2.1.1"),
            ((5, 8), "2.2.0"),
            ((5, 11), "2.2.1"),
            ((5, 13), "2.3.0"),
            ((5, 16), "2.3.1"),
            ((5, 18), "2.3.2"),
            ((5, 20), "2.4.0"),
            ((5, 24), "2.4.2"),
            ((5, 25), "2.4.3"),
            ((5, 27), "2.4.4"),
            ((5, 30), "2.4.5"),
            ((5, 33), "2.4.7"),
            ((5, 36), "2.4.8"),
            ((6, 0), "2.4.4"),
            ((6, 1), "3.0.0"),
            ((6, 3), "3.1.1"),
            ((6, 6), "3.2.0"),
            ((6, 8), "3.3.0"),
            ((6, 12), "3.4.0"),
            ((6, 15), "3.4.1"),
            ((7, 0), "3.5.0"),
            ((7, 2), "3.5.1"),
            ((7, 5), "3.5.2"),
            // The versions the Release Guide's table carries for the lines published
            // since, as for every row above.
            ((7, 6), "3.5.4"),
            ((7, 9), "3.5.5"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Sqoop",
        // One version across the lines that bundle it at all.
        versions: &[((4, 2), "1.4.6"), ((5, 0), "1.4.6"), ((5, 30), "1.4.7"), ((6, 0), "1.4.7")],
        removed_from: None,
    },
    CatalogApplication {
        name: "TensorFlow",
        versions: &[
            // Amazon EMR offers TensorFlow from 5.17.0 onward.
            ((5, 17), "1.9.0"),
            ((5, 24), "1.13.1"),
            ((5, 27), "1.14.0"),
            ((5, 30), "1.14.0"),
            ((5, 36), "2.4.1"),
            ((6, 0), "1.14.0"),
            ((6, 2), "2.4.1"),
            ((6, 9), "2.10.0"),
            ((6, 12), "2.11.0"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Tez",
        versions: &[
            // Amazon EMR offers Tez from 4.7.0 onward, so the oldest 4.x line that bundles
            // it among the ones the catalog publishes is 4.8.
            ((4, 8), "0.8.3"),
            ((5, 0), "0.8.4"),
            ((5, 20), "0.9.1"),
            ((5, 30), "0.9.2"),
            ((6, 0), "0.9.2"),
            ((6, 9), "0.10.2"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Trino",
        // Amazon EMR offers Trino under that name from 6.4.0 onward.
        versions: &[
            ((6, 4), "359"),
            ((6, 6), "367"),
            ((6, 9), "398"),
            ((6, 12), "414"),
            ((6, 15), "426"),
            ((7, 2), "435"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "Zeppelin",
        versions: &[
            // Amazon EMR offers Zeppelin under that name from 5.0.0 onward.
            ((5, 0), "0.6.1"),
            ((5, 4), "0.7.1"),
            ((5, 8), "0.7.2"),
            ((5, 14), "0.7.3"),
            ((5, 18), "0.8.0"),
            ((5, 21), "0.8.1"),
            ((5, 30), "0.8.2"),
            ((5, 36), "0.10.0"),
            ((6, 0), "0.9.0"),
            ((6, 9), "0.10.1"),
            ((7, 2), "0.11.1"),
        ],
        removed_from: None,
    },
    CatalogApplication {
        name: "ZooKeeper",
        versions: &[
            ((5, 0), "3.4.8"),
            ((5, 14), "3.4.10"),
            ((5, 20), "3.4.13"),
            ((5, 30), "3.4.14"),
            ((6, 0), "3.4.14"),
            ((6, 6), "3.5.10"),
            ((7, 0), "3.9.1"),
        ],
        removed_from: None,
    },
];

/// The version of the application `name` that the release `release_label` ships, or `None`
/// when that release bundles no such application and when the label names no release of this
/// catalog.
///
/// The one lookup behind both readers of it: `Cluster$Applications` report what the cluster's
/// release installs ([`super::installed_applications`]), and a Spark application running on
/// the cluster is told the version of the engine it is running on
/// ([`super::spark_runtime`]).
pub fn release_application_version(release_label: &str, name: &str) -> Option<&'static str> {
    let (major, minor, _) = catalog_release(release_label)?;
    APPLICATION_CATALOG
        .iter()
        .find(|application| application.name.eq_ignore_ascii_case(name))
        .and_then(|application| application.version_at(major, minor))
}

/// The Amazon Linux 2 releases offered for an Amazon EMR release built on it (Amazon
/// EMR 5.30.0 through 6.x), oldest first. `OSRelease$Label` documents the format: "as
/// shown in Amazon Linux 2 Release Notes. For example, 2.0.20220218.1." — the first
/// entry here is the release that doc names, and the others are later Amazon Linux 2
/// release-note labels.
///
/// This is a SNAPSHOT of a catalog Amazon Linux keeps publishing to, so what the list
/// pins is the rule — the releases of the generation the Amazon EMR release is built on,
/// in a stable order, each in the documented dotted form — and not a count.
pub(super) const AMAZON_LINUX_2_OS_RELEASES: &[&str] =
    &["2.0.20220218.1", "2.0.20230808.0", "2.0.20231116.0"];

/// The Amazon Linux 2023 releases offered for an Amazon EMR release built on it (Amazon
/// EMR 7.x), in the same dotted release-note form.
pub(super) const AMAZON_LINUX_2023_OS_RELEASES: &[&str] = &["2023.4.20240429.0", "2023.6.20241010.0"];

/// The first Amazon EMR major release built on Amazon Linux 2023 rather than Amazon
/// Linux 2.
pub(super) const FIRST_AMAZON_LINUX_2023_EMR_MAJOR: u32 = 7;

/// The oldest Amazon EMR release built on Amazon Linux 2, and so the oldest that offers a
/// choice of Amazon Linux release at all: every release of the 5.x series before it runs
/// the Amazon Linux generation that preceded Amazon Linux 2, which `OSRelease$Label`'s
/// documented form does not name.
pub const FIRST_AMAZON_LINUX_2_RELEASE: (u32, u32, u32) = (5, 30, 0);

/// The Amazon Linux releases available for an Amazon EMR release, by the generation it is
/// built on — and NONE for a release published before the Amazon Linux 2 generation, which
/// offers no Amazon Linux release to choose between.
pub(super) fn os_releases_for(release: (u32, u32, u32)) -> &'static [&'static str] {
    if release < FIRST_AMAZON_LINUX_2_RELEASE {
        &[]
    } else if release.0 >= FIRST_AMAZON_LINUX_2023_EMR_MAJOR {
        AMAZON_LINUX_2023_OS_RELEASES
    } else {
        AMAZON_LINUX_2_OS_RELEASES
    }
}

/// The label of the release `major.minor.patch`, as Amazon EMR spells it.
pub fn release_label(major: u32, minor: u32, patch: u32) -> String {
    format!("emr-{major}.{minor}.{patch}")
}

/// The `(major, minor, patch)` release an `emr-<major>.<minor>.<patch>` label names, or
/// `None` when the label is not in that form.
pub fn release_version(label: &str) -> Option<(u32, u32, u32)> {
    let mut parts = label.strip_prefix("emr-")?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// The release `label` names, when the catalog offers it.
///
/// A label has to be spelled exactly as [`release_label`] renders it — so a padded or
/// differently cased variant names no release — AND name a patch release published on
/// one of the lines in [`RELEASE_LINES`]. A release family whose labels are NOT of that
/// form resolves here too, because it is one catalog: the Spark family carries its own
/// prefix and its own lines ([`spark_release`]), and a pre-GA release carries a qualifier
/// after the version ([`pre_release`]).
pub fn catalog_release(label: &str) -> Option<(u32, u32, u32)> {
    if let Some(release) = spark_release(label).or_else(|| pre_release(label)) {
        return Some(release);
    }
    let (major, minor, patch) = release_version(label)?;
    if label != release_label(major, minor, patch) {
        return None;
    }
    RELEASE_LINES
        .iter()
        .find(|line| line.major == major && line.minor == minor && line.patches.contains(&patch))
        .map(|_| (major, minor, patch))
}

/// EVERY release label the catalog publishes, NEWEST release first — the order
/// `ListReleaseLabels` reports its first page in ("the first page of results are
/// determined by other filtering parameters or by the latest version",
/// `ListReleaseLabelsInput$NextToken` doc).
///
/// Every family of the catalog is enumerated, not only the `emr-<major>.<minor>.<patch>`
/// one: a pre-GA label follows the release of the version it preceded, oldest qualifier
/// first, and a family with a prefix of its own ([`spark_release_labels`]) follows the
/// series, each family newest release first. A label this list does not carry is a release
/// no caller can discover.
pub fn catalog_release_labels() -> Vec<String> {
    // `(release, qualifier)`, where the empty qualifier is the release itself: a release
    // is listed before the pre-GA labels of the same version, since they preceded it.
    let mut releases: Vec<((u32, u32, u32), &'static str)> = RELEASE_LINES
        .iter()
        .flat_map(|line| {
            line.patches
                .iter()
                .map(move |&patch| ((line.major, line.minor, patch), ""))
        })
        .chain(
            PRE_RELEASE_LABELS
                .iter()
                .map(|pre_release| (pre_release.release(), pre_release.qualifier)),
        )
        .collect();
    releases.sort_unstable_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(right.1)));
    releases
        .into_iter()
        .map(|((major, minor, patch), qualifier)| {
            if qualifier.is_empty() {
                release_label(major, minor, patch)
            } else {
                pre_release_label(major, minor, patch, qualifier)
            }
        })
        .chain(spark_release_labels())
        .collect()
}

/// The applications the release line `(major, minor)` bundles, as the wire shapes, in
/// application-name order so every caller sees the same list in the same order.
pub fn release_line_applications(major: u32, minor: u32) -> Vec<SimplifiedApplication> {
    let mut applications: Vec<SimplifiedApplication> = APPLICATION_CATALOG
        .iter()
        .filter_map(|application| {
            Some(SimplifiedApplication {
                name: Some(application.name.to_string()),
                version: Some(application.version_at(major, minor)?.to_string()),
            })
        })
        .collect();
    applications.sort_by(|left, right| left.name.cmp(&right.name));
    applications
}

/// What `DescribeReleaseLabel` reports for one release of the catalog.
#[derive(Debug, Clone)]
pub struct ReleaseLabelRecord {
    pub label: String,
    pub applications: Vec<SimplifiedApplication>,
    pub available_os_releases: Vec<OSRelease>,
}

/// The catalog's entry for `label`, or `None` when it names no release this emulator
/// offers.
pub fn release_label_record(label: &str) -> Option<ReleaseLabelRecord> {
    if let Some(record) = spark_release_label_record(label) {
        return Some(record);
    }
    let release = catalog_release(label)?;
    let (major, minor, _) = release;
    Some(ReleaseLabelRecord {
        // `catalog_release` resolves a label only in the ONE spelling the catalog
        // publishes it under, so the label is reported back as it was asked for — which
        // is what reports a pre-GA label with the qualifier it carries.
        label: label.to_string(),
        applications: release_line_applications(major, minor),
        available_os_releases: os_releases_for(release)
            .iter()
            .map(|&label| OSRelease {
                label: Some(label.to_string()),
            })
            .collect(),
    })
}
