// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DBInstance$CACertificateIdentifier` across an instance's life.
//!
//! RULE: the member names the CA of the server certificate the instance HAS
//! (`smithy_query(query="doc",shape="DBInstance",member="CACertificateIdentifier")`: "the
//! identifier of the CA certificate for this DB instance"), so an instance whose delete has
//! been accepted reports none — the certificate goes with the instance that is being
//! removed — and the member comes back EMPTY from that instant on, while
//! `CertificateDetails` still describes the certificate the instance ran under.
//!
//! A client that harvests the CA of a live instance must read the identifier it was issued
//! under; one that reads the teardown envelope must not be told the deleted instance still
//! holds a certificate.
//!
//! The rows span what the rule generalises over: engine families, an instance created under
//! the region's default CA as well as one that named its own, and both forms of the delete
//! (with a final snapshot and skipping it). Every call goes over the wire through the one
//! mounting helper this test target has, and every assertion reads the XML the emulator
//! answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text, the_instance};
use crate::lifecycle_poll_loops::{poll_until, read_instance};

/// The class every instance below is created with: the subject here is the certificate the
/// instance holds, which no compute class changes.
const CLASS: &str = "db.t3.medium";

/// The CA a DB instance created without a `CACertificateIdentifier` is issued under, which
/// is what `DescribeCertificates` reports as the default for new launches.
const DEFAULT_CA: &str = "rds-ca-rsa2048-g1";

/// One row of the certificate table.
struct Row {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The `CACertificateIdentifier` the create names, or `None` for one that takes the
    /// region's default.
    requested_ca: Option<&'static str>,
    /// Whether the delete takes a final snapshot rather than skipping it.
    final_snapshot: Option<&'static str>,
}

/// `CreateDBInstance`, as the `<DBInstance>` the create envelope carries.
async fn create_instance(router: &MultiServiceRouter, row: &Row) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", row.identifier.to_string()),
        ("Engine", row.engine.to_string()),
        ("DBInstanceClass", CLASS.to_string()),
    ];
    if let Some(ca) = row.requested_ca {
        params.push(("CACertificateIdentifier", ca.to_string()));
    }
    let body = expect_ok(row.case, call(router, "CreateDBInstance", &params).await);
    the_instance(row.case, &body)
}

/// The CA identifier an envelope reports, as the element itself: `None` when the member is
/// absent altogether, which is a different statement from the empty value.
fn reported_ca(instance: &str) -> Option<String> {
    optional_text(instance, "CACertificateIdentifier")
}

/// The `CAIdentifier` inside `CertificateDetails`, which describes the certificate itself.
fn certificate_details_ca(case: &str, envelope: &str, instance: &str) -> String {
    let found = subtrees(instance, "CertificateDetails");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: the {envelope} envelope must report exactly one CertificateDetails, \
         got {found:?}"
    );
    let details = found.into_iter().next().expect("checked just above");
    text(&details, "CAIdentifier")
}

/// RULE: every envelope of a live instance names the CA it was issued under; the envelope
/// that answers an accepted delete reports none, and so does every read while the instance
/// is still on its way out.
#[tokio::test]
async fn an_instance_reports_its_ca_certificate_until_its_delete_is_accepted() {
    let router = emulator();

    let rows: &[Row] = &[
        Row {
            case: "mysql under the region's default CA, delete skipping the final snapshot",
            identifier: "emu-cadel-mysql",
            engine: "mysql",
            requested_ca: None,
            final_snapshot: None,
        },
        Row {
            case: "postgres under a CA the request named",
            identifier: "emu-cadel-postgres",
            engine: "postgres",
            requested_ca: Some("rds-ca-ecc384-g1"),
            final_snapshot: None,
        },
        Row {
            case: "oracle-se2, deleted with a final snapshot",
            identifier: "emu-cadel-oracle",
            engine: "oracle-se2",
            requested_ca: None,
            final_snapshot: Some("emu-cadel-oracle-final"),
        },
    ];

    for row in rows {
        let issued_under = row.requested_ca.unwrap_or(DEFAULT_CA);
        let created = create_instance(&router, row).await;
        let described = {
            let (status, body) = read_instance(&router, row.identifier).await;
            the_instance(row.case, &expect_ok(row.case, (status, body)))
        };

        for (envelope, instance) in [("create", &created), ("describe", &described)] {
            let reported = reported_ca(instance);
            assert_eq!(
                reported.as_deref(),
                Some(issued_under),
                "case {:?}: the {envelope} envelope must report the CA the instance was \
                 issued under, got {reported:?}",
                row.case
            );
            let details = certificate_details_ca(row.case, envelope, instance);
            assert_eq!(
                details, issued_under,
                "case {:?}: the {envelope} envelope's CertificateDetails must describe that \
                 same certificate, got {details:?}",
                row.case
            );
        }

        // The delete is accepted: the instance reports `deleting` and holds no certificate
        // from this answer on.
        let mut params: Vec<(&str, String)> =
            vec![("DBInstanceIdentifier", row.identifier.to_string())];
        match row.final_snapshot {
            Some(snapshot) => {
                params.push(("FinalDBSnapshotIdentifier", snapshot.to_string()));
            }
            None => params.push(("SkipFinalSnapshot", "true".to_string())),
        }
        let deleted = the_instance(
            row.case,
            &expect_ok(row.case, call(&router, "DeleteDBInstance", &params).await),
        );
        let status = text(&deleted, "DBInstanceStatus");
        assert_eq!(
            status, "deleting",
            "case {:?}: an accepted delete reports the instance deleting, got {status:?}",
            row.case
        );
        let reported = reported_ca(&deleted);
        assert_eq!(
            reported.as_deref(),
            Some(""),
            "case {:?}: the delete envelope must report the CACertificateIdentifier of an \
             instance that holds no certificate any more, which is the member reported \
             EMPTY, got {reported:?}",
            row.case
        );
        let details = certificate_details_ca(row.case, "delete", &deleted);
        assert_eq!(
            details, issued_under,
            "case {:?}: the delete envelope's CertificateDetails still describes the \
             certificate the instance ran under, got {details:?}",
            row.case
        );

        // And so does every read while the instance is on its way out: the loop ends when
        // the identifier names no instance at all, and each answer it sees in between is
        // held to the same rule.
        let case = row.case;
        let (gone, _polls) = poll_until(
            case,
            &format!("{} leaving the store", row.identifier),
            || read_instance(&router, row.identifier),
            |http_status, body| {
                if http_status != 200 {
                    return true;
                }
                let instance = the_instance(case, body);
                if text(&instance, "DBInstanceStatus") == "deleting" {
                    let reported = reported_ca(&instance);
                    assert_eq!(
                        reported.as_deref(),
                        Some(""),
                        "case {case:?}: a read of an instance that is deleting must report \
                         no CA certificate, got {reported:?}"
                    );
                }
                false
            },
        )
        .await;
        assert!(
            gone.contains("DBInstanceNotFound"),
            "case {case:?}: the identifier of a deleted instance names no instance, got \
             {gone}"
        );
    }
}
