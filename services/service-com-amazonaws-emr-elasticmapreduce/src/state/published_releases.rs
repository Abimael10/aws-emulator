// SPDX-License-Identifier: CC0-1.0

//! The INVARIANT over the release catalog's labels: a label resolves to a release exactly
//! when the catalog publishes it.
//!
//! One concern of this service's state, beside the catalog tables themselves
//! ([`super::release_catalog`]), which stay the ONE source of what Amazon EMR publishes. Two
//! readings of those tables have to agree, because a caller uses them together: the labels
//! `ListReleaseLabels` enumerates for it to choose a `ReleaseLabel` from
//! ([`catalog_release_labels`]), and the resolution a cluster launch puts the label it chose
//! through ([`catalog_release`]). A label the listing publishes but the resolution refuses is a
//! release no caller can launch though the service offered it — the launch answers "the
//! supplied release label is invalid" for a release that is not; a label the resolution accepts
//! but the listing omits is a release no caller can discover. Every family of the catalog has
//! to be in both — the `emr-<major>.<minor>.<patch>` series, the pre-GA labels
//! ([`PRE_RELEASE_LABELS`]) and the Spark family ([`SPARK_RELEASE_LINES`]) — which is what the
//! case below holds them to, over the whole space of labels those forms can spell within the
//! catalog's bounds.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::*;

    /// The bounds of the label space swept below: past every major, minor and patch component
    /// the catalog's own labels reach, so no published label falls outside the sweep and every
    /// published label has its unpublished neighbours swept beside it.
    const MAX_SWEPT_MAJOR: u32 = 9;
    const MAX_SWEPT_MINOR: u32 = 40;
    const MAX_SWEPT_PATCH: u32 = 7;

    /// Every label the catalog's forms can spell inside those bounds: a release of the
    /// `emr-<major>.<minor>.<patch>` series, one of the Spark family's own form, and a pre-GA
    /// label under each qualifier the catalog uses and one it does not.
    fn swept_labels() -> Vec<String> {
        let mut labels = Vec::new();
        for major in 0..=MAX_SWEPT_MAJOR {
            for minor in 0..=MAX_SWEPT_MINOR {
                for patch in 0..=MAX_SWEPT_PATCH {
                    labels.push(release_label(major, minor, patch));
                    labels.push(spark_release_label(major, minor, patch));
                    for qualifier in ["beta", "beta2", "beta3"] {
                        labels.push(pre_release_label(major, minor, patch, qualifier));
                    }
                }
            }
        }
        labels
    }

    #[test]
    fn a_label_resolves_to_a_release_exactly_when_the_catalog_publishes_it() {
        let published: BTreeSet<String> = catalog_release_labels().into_iter().collect();
        assert!(
            published.len() > 100,
            "the catalog publishes the Amazon EMR releases, got {} labels",
            published.len()
        );

        // The sweep has to cover the catalog, or the agreement below would hold only where
        // nobody looked.
        let swept: BTreeSet<String> = swept_labels().into_iter().collect();
        let outside: Vec<&String> = published
            .iter()
            .filter(|label| !swept.contains(*label))
            .collect();
        assert!(
            outside.is_empty(),
            "every published label must fall inside the swept label space, got {outside:?}"
        );

        for label in &swept {
            let publishes = published.contains(label);
            let resolves = catalog_release(label).is_some();
            assert_eq!(
                resolves, publishes,
                "case {label:?}: a label resolves to a release exactly when the catalog \
                 publishes it, got resolves={resolves} published={publishes}"
            );
        }
    }
}
