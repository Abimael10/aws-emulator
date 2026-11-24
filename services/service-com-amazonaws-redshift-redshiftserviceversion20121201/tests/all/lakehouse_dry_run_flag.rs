// SPDX-License-Identifier: CC0-1.0

//! The `DryRun` flag of `ModifyLakehouseConfiguration`: what it decides, and
//! where that decision sits among the operation's preconditions.
//!
//! One rule is under test, over the whole class the flag ranges across rather
//! than over one recorded request: a request that asks for the modification to
//! be VALIDATED and not made is answered by the outcome of that validation —
//! every precondition is taken, nothing is written, and a request that would
//! have succeeded is REFUSED with the dry-run error code and its fixed sentence
//! instead of being answered with a configuration body — while a request that
//! does not ask for that is applied and answered with the configuration it
//! wrote. A precondition that refuses the request keeps its OWN fault whatever
//! the flag says, so a probe can tell "this would have worked" from "this would
//! not".
//!
//! Each case below drives a real HTTP request through the same
//! `MultiServiceRouter` the production router builds, over the shared harness of
//! [`crate::cluster_lifecycle`], so every assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Emulator, REGION};
use crate::lakehouse_configuration::{available_cluster, describe, modify};

/// What one row of the flag table expects of the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The request was only validated: refused with the dry-run code, no
    /// configuration reported and nothing written.
    DryRunRefusal,
    /// The request was applied: the configuration it wrote, reported back.
    Modification,
}

/// The catalog every row below registers against, written in the characters
/// `ModifyLakehouseConfigurationMessage$CatalogName` admits.
const CATALOG: &str = "dry-run_catalog";

#[tokio::test]
async fn the_flag_decides_between_the_dry_run_refusal_and_the_modification() {
    let emulator = Emulator::new();

    // The rows differ along the one thing the rule generalises over: the value
    // the flag carries — each of the two booleans the member admits, and its
    // absence. Each row drives its OWN cluster, so a row that must write
    // nothing cannot be read as a row that wrote and a row that must write
    // cannot inherit an earlier row's configuration.
    let rows: [(&str, &str, Option<&str>, Expected); 3] = [
        (
            "the flag set",
            "dryrun-flag-set",
            Some("true"),
            Expected::DryRunRefusal,
        ),
        (
            "the flag clear",
            "dryrun-flag-clear",
            Some("false"),
            Expected::Modification,
        ),
        (
            "no flag at all",
            "dryrun-flag-absent",
            None,
            Expected::Modification,
        ),
    ];

    for (_, identifier, _, _) in &rows {
        available_cluster(&emulator, identifier).await;
    }
    // The clock is STOPPED from here on, so the transient side of a
    // registration a row makes stays open however long the host takes over the
    // calls in it, and the advance past the span is what settles it.
    emulator.stop_clock();

    for (case, identifier, dry_run, expected) in rows {
        let mut params = vec![
            ("ClusterIdentifier", identifier),
            ("LakehouseRegistration", "Register"),
            ("CatalogName", CATALOG),
        ];
        if let Some(dry_run) = dry_run {
            params.push(("DryRun", dry_run));
        }

        let before = describe(&emulator, identifier).await;
        let answered = modify(&emulator, &params).await;
        let catalog_arn = state::catalog_arn(CATALOG, REGION);

        match expected {
            Expected::DryRunRefusal => {
                assert_eq!(
                    (answered.status, answered.error_code().as_deref()),
                    (412, Some("DryRunOperation")),
                    "case {case:?}: expected the dry-run refusal, got {} / {:?} with body {}",
                    answered.status,
                    answered.error_code(),
                    answered.body
                );
                assert_eq!(
                    answered.text("Message").as_deref(),
                    Some(state::DRY_RUN_WOULD_HAVE_SUCCEEDED),
                    "case {case:?}: expected the dry-run sentence, got {:?}",
                    answered.text("Message")
                );
                // Fixed text with no variable part: the refusal names neither
                // the cluster the request was about nor the catalog it would
                // have registered against.
                assert!(
                    !answered.body.contains(identifier) && !answered.body.contains(CATALOG),
                    "case {case:?}: expected a sentence with no request-derived part, got body {}",
                    answered.body
                );
                // And no configuration is reported at all — the validated
                // outcome travels as the refusal, not as a body.
                assert_eq!(
                    (
                        answered.text("ClusterIdentifier"),
                        answered.text("LakehouseRegistrationStatus"),
                        answered.text("CatalogArn"),
                    ),
                    (None, None, None),
                    "case {case:?}: expected no configuration in the refusal, got body {}",
                    answered.body
                );

                // Nothing was written: the cluster reports the configuration it
                // carried before the call.
                let after = describe(&emulator, identifier).await;
                assert_eq!(
                    (
                        after.text("LakehouseRegistrationStatus"),
                        after.text("CatalogArn"),
                    ),
                    (
                        before.text("LakehouseRegistrationStatus"),
                        before.text("CatalogArn"),
                    ),
                    "case {case:?}: expected the cluster unchanged, got body {}",
                    after.body
                );
                // Not even a change the clock would have owed: a validated
                // request started no registration for the span to settle.
                emulator.advance_clock(state::LAKEHOUSE_REGISTRATION_SPAN);
                let later = describe(&emulator, identifier).await;
                assert_eq!(
                    (
                        later.text("LakehouseRegistrationStatus"),
                        later.text("CatalogArn"),
                    ),
                    (
                        before.text("LakehouseRegistrationStatus"),
                        before.text("CatalogArn"),
                    ),
                    "case {case:?}: expected no registration to fall due past the span, got body {}",
                    later.body
                );
            }
            Expected::Modification => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: expected 200, got {} with body {}",
                    answered.status, answered.body
                );
                assert_eq!(
                    (
                        answered.text("ClusterIdentifier"),
                        answered.text("LakehouseRegistrationStatus"),
                        answered.text("CatalogArn"),
                    ),
                    (
                        Some(identifier.to_string()),
                        Some(state::LAKEHOUSE_REGISTRATION_REQUESTED.to_string()),
                        Some(catalog_arn.clone()),
                    ),
                    "case {case:?}: expected the configuration the request asked for, got body {}",
                    answered.body
                );
                // ROUND TRIP: both values are members of the cluster, so a
                // cluster read reports what this call stored.
                let after = describe(&emulator, identifier).await;
                assert_eq!(
                    (
                        after.text("LakehouseRegistrationStatus"),
                        after.text("CatalogArn"),
                    ),
                    (
                        Some(state::LAKEHOUSE_REGISTRATION_REQUESTED.to_string()),
                        Some(catalog_arn.clone()),
                    ),
                    "case {case:?}: expected a cluster read to report the stored configuration, \
                     got body {}",
                    after.body
                );
                // Past the span the registration it started settles, with no
                // mutating call in between.
                emulator.advance_clock(state::LAKEHOUSE_REGISTRATION_SPAN);
                let settled = describe(&emulator, identifier).await;
                assert_eq!(
                    (
                        settled.text("LakehouseRegistrationStatus"),
                        settled.text("CatalogArn"),
                    ),
                    (
                        Some(state::LAKEHOUSE_REGISTRATION_REGISTERED.to_string()),
                        Some(catalog_arn),
                    ),
                    "case {case:?}: expected the registration settled past its span, got body {}",
                    settled.body
                );
            }
        }
    }
}

/// One refused request: the parameters it carries and the fault it keeps
/// whatever the flag says.
struct Precondition {
    case: &'static str,
    params: Vec<(&'static str, &'static str)>,
    status: u16,
    code: &'static str,
}

#[tokio::test]
async fn a_precondition_keeps_its_own_fault_whatever_the_flag_says() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "dryrun-precondition").await;
    // Stopped before the second cluster is created, so that one stays inside
    // its provisioning span however long the calls below take — which is what
    // makes the state fault observable at all.
    emulator.stop_clock();
    let provisioning = emulator.create_minimal("dryrun-provisioning").await;
    assert_eq!(
        provisioning.status, 200,
        "case seed the provisioning cluster: expected 200, got {} with body {}",
        provisioning.status, provisioning.body
    );

    // One row per precondition the operation enforces, so the rows differ along
    // WHICH dependency the request cannot resolve.
    let rows = [
        Precondition {
            case: "a cluster this account does not hold",
            params: vec![
                ("ClusterIdentifier", "dryrun-no-such-cluster"),
                ("LakehouseRegistration", "Register"),
            ],
            status: 404,
            code: "ClusterNotFound",
        },
        Precondition {
            case: "a cluster the service is still working on",
            params: vec![
                ("ClusterIdentifier", "dryrun-provisioning"),
                ("LakehouseRegistration", "Register"),
            ],
            status: 400,
            code: "InvalidClusterState",
        },
        Precondition {
            case: "an Identity Center application this account did not register",
            params: vec![
                ("ClusterIdentifier", "dryrun-precondition"),
                ("LakehouseIdcRegistration", "Associate"),
                (
                    "LakehouseIdcApplicationArn",
                    "arn:aws:redshift:us-west-2:000000000000:redshiftidcapplication:absent",
                ),
            ],
            status: 404,
            code: "RedshiftIdcApplicationNotExists",
        },
    ];

    for row in rows {
        // The same request under each value the flag ranges over: validating it
        // reports the precondition's refusal, and so does making it.
        for flag in [Some("true"), Some("false"), None] {
            let mut params = row.params.clone();
            if let Some(flag) = flag {
                params.push(("DryRun", flag));
            }
            let case = format!("{} with DryRun {flag:?}", row.case);
            let refused = modify(&emulator, &params).await;
            assert_eq!(
                (refused.status, refused.error_code().as_deref()),
                (row.status, Some(row.code)),
                "case {case:?}: expected {} {}, got {} / {:?} with body {}",
                row.status,
                row.code,
                refused.status,
                refused.error_code(),
                refused.body
            );
            // The precondition's own sentence, never the dry-run one: a request
            // a precondition refuses did not "would have succeeded".
            assert_ne!(
                refused.text("Message").as_deref(),
                Some(state::DRY_RUN_WOULD_HAVE_SUCCEEDED),
                "case {case:?}: expected the precondition's own sentence, got the dry-run one in \
                 body {}",
                refused.body
            );
        }
    }

    // And the cluster the rows above reached kept its configuration: none of
    // them wrote.
    let read = describe(&emulator, "dryrun-precondition").await;
    assert_eq!(
        (
            read.text("LakehouseRegistrationStatus"),
            read.text("CatalogArn"),
        ),
        (None, None),
        "case the cluster the refused requests named: expected no configuration written, got \
         body {}",
        read.body
    );
}
