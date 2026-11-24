// SPDX-License-Identifier: CC0-1.0

//! The key features each record of the per-category tier offers its customers.
//!
//! `highlights` is "[a] list of key features that the listing offers to customers"
//! (`smithy_query(query="operation",operation="GetListing")`), so the features a record
//! reports are features OF THAT RECORD: two different listings offer different things and
//! therefore state different features. They are stated per SUBJECT — the subcategory the
//! record is classified at, of which [`super::taxonomy_rows`] seeds exactly one record —
//! rather than per kind of product or per route the product is delivered through: how a
//! record is classified and how it is fulfilled are already reported by `categories` and
//! `fulfillmentOptionSummaries`, and a list derived from either is a restatement of those
//! members instead of a statement about the product.
//!
//! None of the sentences below names the category the record is classified under, the route
//! it is delivered through or the pricing it is sold under: a key feature says what the
//! product DOES.

/// The key features the record of one subcategory offers, as
/// `(taxonomy key, the features it states)`.
///
/// Keyed by [`CATEGORY_TAXONOMY`]'s own key, and covering every subcategory that taxonomy
/// declares (the test below holds the two together), because the per-category tier seeds one
/// record per subcategory: a feature stated here is one record's.
const SUBJECT_FEATURES: &[(&str, &[&str])] = &[
    (
        "operating-systems",
        &[
            "Hardened kernel images rebuilt weekly with the latest patches",
            "Long-term support branches with a published end-of-life date",
            "Boots on Graviton and x86 instances from one manifest",
        ],
    ),
    (
        "storage",
        &[
            "Volumes expand online without unmounting the filesystem",
            "Snapshots are deduplicated and encrypted at rest",
            "Cold blocks move to cheaper media automatically",
        ],
    ),
    (
        "data-analytics",
        &[
            "Interactive queries over billions of rows answer in seconds",
            "Materialised views refresh as new records land",
            "Every query reports what it scanned and what it cost",
        ],
    ),
    (
        "data-integration",
        &[
            "Change capture from relational sources without a nightly batch",
            "Schema drift is reconciled before a load fails",
            "A failed pipeline run replays from its last checkpoint",
        ],
    ),
    (
        "data-preparation",
        &[
            "Every column is profiled and its outliers flagged before a load",
            "Cleansing rules are versioned and reused across pipelines",
            "Row-level lineage from the raw file to the published table",
        ],
    ),
    (
        "data-visualization",
        &[
            "Dashboards refresh from a live connection without extracts",
            "A chart drills from its summary down to the rows behind it",
            "Reports are scheduled and delivered as PDF",
        ],
    ),
    (
        "network-infrastructure",
        &[
            "Routes are advertised over BGP to your own gateways",
            "Packet capture at line rate on any interface",
            "Failover between availability zones without a session drop",
        ],
    ),
    (
        "security",
        &[
            "Threat detection on east-west traffic inside your own VPC",
            "Findings are mapped to MITRE ATT&CK techniques",
            "Secrets are scanned in every commit before a merge",
        ],
    ),
    (
        "backup-recovery",
        &[
            "Point-in-time restore to any second of the retention window",
            "Every restore is verified by an automatic trial boot",
            "Copies are kept in a second region under their own key",
        ],
    ),
    (
        "high-performance-computing",
        &[
            "Schedules MPI jobs across thousands of cores",
            "Interconnect latency is reported per rank",
            "A long solver run checkpoints to a resumable file",
        ],
    ),
    (
        "migration",
        &[
            "Dependencies are discovered before the first server moves",
            "A running workload is replicated ahead of its cutover",
            "A cutover rolls back to the source on demand",
        ],
    ),
    (
        "crm",
        &[
            "One timeline of every customer interaction per account",
            "Forecasts update as a deal stage changes",
            "Territory and quota planning with approval history",
        ],
    ),
    (
        "collaboration-productivity",
        &[
            "Documents are co-edited with per-paragraph comments",
            "Meeting notes link to the tasks they created",
            "Guest access expires on the date its owner set",
        ],
    ),
    (
        "e-commerce",
        &[
            "A basket survives a session and follows the shopper between devices",
            "Catalogue search ranks by margin as well as by relevance",
            "Checkout accepts split payment and partial refunds",
        ],
    ),
    (
        "human-resources",
        &[
            "Onboarding tasks are generated from the signed offer",
            "Leave balances accrue by local statutory rules",
            "Reviews collect peer feedback anonymously",
        ],
    ),
    (
        "it-business-management",
        &[
            "Every asset is costed to the team that consumes it",
            "A change approval carries the risk review that justified it",
            "Each published catalogue entry ships with its own run-book",
        ],
    ),
    (
        "financial-services-data",
        &[
            "Instrument identifiers are cross-referenced across vendors",
            "Corporate actions are applied to historical series",
            "Daily files land before the opening bell",
        ],
    ),
    (
        "market-data",
        &[
            "Tick history for every trading session of the last decade",
            "Quotes are normalised across venues into one schema",
            "Intraday bars aggregated at one-minute resolution",
        ],
    ),
    (
        "reference-data",
        &[
            "Entity hierarchies are maintained with effective dates",
            "Identifier mappings are versioned, so every change is auditable",
            "A retired code stays resolvable after it is withdrawn",
        ],
    ),
    (
        "healthcare-life-sciences-data",
        &[
            "Patient records are de-identified before delivery",
            "Trial outcomes are coded to standard terminologies",
            "Claims and prescriptions are linked by a stable subject key",
        ],
    ),
    (
        "environmental-data",
        &[
            "Satellite observations are resampled onto a common grid",
            "Emission factors are updated as agencies publish them",
            "Every station reading carries its own quality flag",
        ],
    ),
    (
        "public-sector-data",
        &[
            "Census tables are joined to current administrative boundaries",
            "Procurement notices are published within a day of award",
            "Every table cites the agency that issued it",
        ],
    ),
    (
        "telecommunications-data",
        &[
            "Coverage maps derived from crowd-sourced measurements",
            "Call detail records aggregated by cell and hour",
            "Spectrum licence holdings tracked per operator",
        ],
    ),
    (
        "computer-vision",
        &[
            "Detects and tracks objects across video frames",
            "Trains on your own labelled images in your own account",
            "Runs on edge accelerators as well as on GPU instances",
        ],
    ),
    (
        "natural-language-processing",
        &[
            "Extracts entities and relations from long documents",
            "Summarises a thread into the decisions it recorded",
            "Twenty-six languages are supported out of the box",
        ],
    ),
    (
        "speech-recognition",
        &[
            "Transcribes streaming audio with speaker labels",
            "A custom vocabulary lifts accuracy on domain terms",
            "Punctuation and casing are restored automatically",
        ],
    ),
    (
        "structured-data-ml",
        &[
            "Trains gradient-boosted models over tabular columns",
            "Feature importance is reported beside every prediction",
            "Model drift is flagged against a held-out sample",
        ],
    ),
    (
        "application-development",
        &[
            "Scaffolds a service with its pipeline already wired",
            "Local and remote debugging against the same build",
            "Dependency upgrades arrive as reviewable patches",
        ],
    ),
    (
        "agile-lifecycle-management",
        &[
            "A backlog item traces from idea to the commit that closed it",
            "Sprint capacity is planned from measured throughput",
            "Release notes are drafted from the issues in the milestone",
        ],
    ),
    (
        "monitoring",
        &[
            "Alerts fire on a burn rate rather than on a single breach",
            "Traces, metrics and logs share one correlation id",
            "Every dashboard is declared in a file and reviewed as code",
        ],
    ),
    (
        "source-control",
        &[
            "A protected branch merges only after a reviewed change",
            "Large binaries are held out of band and fetched on checkout",
            "History is signed, so every commit has a verifiable author",
        ],
    ),
    (
        "testing",
        &[
            "A flaky specification is quarantined and reported on its own",
            "Coverage is compared against the base branch on every run",
            "Browser suites run in parallel against ephemeral environments",
        ],
    ),
    (
        "infrastructure-as-code",
        &[
            "A plan shows every resource that will change before it is applied",
            "State is locked while a change is being applied",
            "Drift from the declared template is reported each night",
        ],
    ),
    (
        "iot-analytics",
        &[
            "Device telemetry is downsampled as it ages",
            "Anomalies are scored per device rather than per fleet",
            "Queries span millions of sensors with no cluster of your own",
        ],
    ),
    (
        "iot-device-management",
        &[
            "A fleet-wide firmware rollout pauses on a failure rate",
            "Certificates rotate before they expire",
            "A device that reconnects resumes its pending jobs",
        ],
    ),
    (
        "iot-edge-computing",
        &[
            "Runs inference on the gateway while the uplink is down",
            "Readings are buffered locally and reconciled on reconnect",
            "One module deploys to ARM and x86 gateways alike",
        ],
    ),
    (
        "assessments",
        &[
            "A fixed-scope review that ends in a written findings report",
            "Workshops are led by certified architects",
            "Every recommendation is costed before it is proposed",
        ],
    ),
    (
        "implementation",
        &[
            "A named delivery lead from kickoff to handover",
            "Fixed-price milestones with acceptance criteria agreed up front",
            "Handover sessions are recorded for your own engineers",
        ],
    ),
    (
        "managed-services",
        &[
            "An on-call rota that answers within the agreed window",
            "Monthly reporting on every incident and its root cause",
            "Runbooks are written with you and handed over on exit",
        ],
    ),
    (
        "training",
        &[
            "Instructor-led courses delivered in your own account",
            "Hands-on labs that reset between cohorts",
            "Certification exams are included in the enrolment",
        ],
    ),
    (
        "financial-services",
        &[
            "Ledger entries are immutable and double-entry balanced",
            "Regulatory reports are generated from the books themselves",
            "Payment files are signed and acknowledged end to end",
        ],
    ),
    (
        "healthcare",
        &[
            "Clinical notes are coded as they are written",
            "Appointment reminders reach patients by their chosen channel",
            "An audit trail records every read of a patient chart",
        ],
    ),
    (
        "media-entertainment",
        &[
            "Ingests camera masters and transcodes to every delivery profile",
            "Rights windows are enforced per territory",
            "Subtitles are generated and reviewed in the same timeline",
        ],
    ),
    (
        "retail",
        &[
            "Shelf availability is predicted per store and per day",
            "Promotions are priced against competitor movements",
            "A return is reconciled back to the basket it came from",
        ],
    ),
    (
        "energy",
        &[
            "Load forecasts combine weather with metering history",
            "Outages are located from smart-meter last-gasp signals",
            "Turbine performance is compared against its own baseline",
        ],
    ),
    (
        "telecommunications",
        &[
            "A subscriber plan changes mid-cycle with prorated billing",
            "Network incidents are correlated with customer complaints",
            "Number portability completes within the working day",
        ],
    ),
    (
        "education-research",
        &[
            "Enrolment and grading share one student record",
            "Published research carries a citable identifier",
            "Library access is granted by institutional sign-in",
        ],
    ),
    (
        "public-sector",
        &[
            "Case files follow a statutory retention schedule",
            "Citizen identity is verified against national registers",
            "Every decision carries the officer and the policy behind it",
        ],
    ),
];

/// The key features the record classified at the subcategory `category` offers.
///
/// Every subcategory the taxonomy declares states its own ([`SUBJECT_FEATURES`]), so this
/// lookup fails loudly rather than falling back the way the per-kind tables beside it do: a
/// subcategory added without features of its own would otherwise seed a record whose
/// `highlights` are another record's, which is a false statement about the record rather
/// than a missing one.
pub(super) fn features_of_subject(category: &str) -> &'static [&'static str] {
    SUBJECT_FEATURES
        .iter()
        .find(|(subject, _)| *subject == category)
        .map(|(_, features)| *features)
        .expect("every subcategory the per-category tier seeds states key features of its own")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::state::CATEGORY_TAXONOMY;

    /// RULE: every subcategory of the taxonomy states key features of its OWN, and no two
    /// subcategories state the same feature — the per-category tier seeds one record per
    /// subcategory, so a feature shared by two of them would be a feature shared by two
    /// records.
    #[test]
    fn every_subcategory_states_its_own_key_features() {
        let mut stated: BTreeMap<&str, &str> = BTreeMap::new();
        for (category, _, parent) in CATEGORY_TAXONOMY {
            if parent.is_none() {
                continue;
            }
            let features = features_of_subject(category);
            assert!(
                !features.is_empty() && features.len() <= 10,
                "case {category}: a record states between one and ten key features \
                 (HighlightList @length ..10), got {features:?}"
            );
            for feature in features {
                assert!(
                    !feature.trim().is_empty() && feature.chars().count() <= 1024,
                    "case {category}: a key feature is non-blank and within \
                     NonEmptyString @length(1..1024), got {feature:?}"
                );
                if let Some(held) = stated.insert(feature, category) {
                    panic!(
                        "case {category}: the key feature {feature:?} is already stated by \
                         {held}, so two records would state one feature"
                    );
                }
            }
        }
        assert!(
            stated.len() > 100,
            "case setup: the taxonomy's subcategories must each state features, got {} \
             features in all",
            stated.len()
        );
    }

    /// A key the taxonomy does not declare has no features to state, and fails loudly rather
    /// than seeding another record's.
    #[test]
    #[should_panic(expected = "states key features of its own")]
    fn a_subject_the_taxonomy_does_not_declare_states_nothing() {
        let _ = features_of_subject("no-such-subcategory");
    }
}
