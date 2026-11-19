// SPDX-License-Identifier: CC0-1.0

//! The PUBLICATION DATE of a document Systems Manager publishes, through
//! `ListDocumentVersions`, `DescribeDocument` and `ListDocuments`.
//!
//! The rule under test: a document Systems Manager publishes exists in every account and
//! Region without anybody creating it, so a read resolves it to at least one `Active`
//! version; and because nobody in the account created it, its `CreatedDate` is the date
//! AWS published THAT document — a fact of the catalogue, carried per document, identical
//! in every Region and in every run of the emulator, and never a reading of the clock the
//! store started at.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct.
//!
//! The suites are tables whose rows span the whole published catalogue — every document
//! type, every schema version and every platform restriction it holds — rather than the
//! one document a recorded unit happened to resolve.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, router, REGION};

/// The catalogue the service serves (`crate::state::PUBLISHED_DOCUMENTS`), so each suite
/// below spans every entry of it rather than a few.
const CATALOGUE: &[service_com_amazonaws_ssm_amazonssm::state::PublishedDocument] =
    service_com_amazonaws_ssm_amazonssm::state::PUBLISHED_DOCUMENTS;

/// `ListDocumentVersions` for one name, returning `(status, body)`.
async fn list_versions(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "ListDocumentVersions", &json!({ "Name": name })).await
}

/// The one `DocumentVersionInfo` a published document lists, asserting the read succeeded
/// and reported exactly one version.
async fn the_one_version(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, response) = list_versions(router, name).await;
    assert_eq!(
        status, 200,
        "case {case}: ListDocumentVersions {name} must resolve a document AWS publishes, \
         got {status} {response}"
    );
    let versions = response["DocumentVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        versions.len(),
        1,
        "case {case}: {name} publishes one version, got {response}"
    );
    versions[0].clone()
}

/// The `CreatedDate` of a reported version or document, as the wire carries it (epoch
/// seconds with a fraction).
fn created_date(case: &str, reported: &Value) -> f64 {
    reported["CreatedDate"].as_f64().unwrap_or_else(|| {
        panic!("case {case}: a published document carries a CreatedDate, got {reported}")
    })
}

/// Every document of the published catalogue resolves on a FRESH emulator, with no create
/// of any kind, and reports one `Active` default version dated by its own publication
/// date — the date `DescribeDocument` and `ListDocuments` report for it too.
#[tokio::test]
async fn every_published_document_lists_an_active_version_dated_by_its_publication() {
    let router = router();
    let listed = list_documents(&router).await;

    for entry in CATALOGUE {
        let case = entry.name;
        let version = the_one_version(&router, case, entry.name).await;
        assert_eq!(
            (
                version["Name"].clone(),
                version["DocumentVersion"].clone(),
                version["IsDefaultVersion"].clone(),
                version["Status"].clone(),
                version["DocumentFormat"].clone(),
            ),
            (
                json!(entry.name),
                json!("1"),
                json!(true),
                json!("Active"),
                json!("JSON"),
            ),
            "case {case}: a published document lists one Active, default JSON version, \
             got {version}"
        );

        let date = created_date(case, &version);
        assert!(
            date > 0.0,
            "case {case}: expected the publication date, got {date}"
        );

        // The same date wherever the document is read from.
        let (status, described) = call(&router, "DescribeDocument", &json!({ "Name": entry.name })).await;
        assert_eq!(
            status, 200,
            "case {case}: DescribeDocument must resolve it too, got {status} {described}"
        );
        let described_date = created_date(case, &described["Document"]);
        assert_eq!(
            described_date, date,
            "case {case}: the describe must report the same CreatedDate as the version \
             listing, got {described_date} against {date}"
        );
        let identifier = listed.get(entry.name).unwrap_or_else(|| {
            panic!("case {case}: a listing must report every published document, got {listed:?}")
        });
        let identifier_date = created_date(case, identifier);
        assert_eq!(
            identifier_date, date,
            "case {case}: the listing must report the same CreatedDate, got \
             {identifier_date} against {date}"
        );
    }
}

/// A published document's date is a fact of the CATALOGUE, not a reading of the clock:
/// it is nowhere near the instant the store came into being, it carries no precision finer
/// than the millisecond, and the catalogue is dated per document rather than at one shared
/// instant (two documents AWS published together may share a date, but the whole catalogue
/// sharing one is the seeding clock).
#[tokio::test]
async fn a_publication_date_is_catalogue_data_and_not_the_stores_own_clock() {
    let seeded_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64();
    let router = router();

    let mut by_name: BTreeMap<&str, f64> = BTreeMap::new();
    for entry in CATALOGUE {
        let case = entry.name;
        let version = the_one_version(&router, case, entry.name).await;
        let date = created_date(case, &version);
        assert!(
            date > 0.0,
            "case {case}: expected the publication date, got {date}"
        );
        // Generous: a catalogue date is years from the instant this store was built, so
        // anything within a minute of it is the seeding clock rather than a published date.
        assert!(
            (seeded_at - date).abs() > 60.0,
            "case {case}: a document AWS published was not created when this store was \
             ({seeded_at}), got {date}"
        );
        // Systems Manager reports a document date at millisecond precision; the seeding
        // clock this used to be read from carries sub-millisecond digits.
        let micros = (date * 1_000_000.0).round() as i64;
        assert_eq!(
            micros % 1_000,
            0,
            "case {case}: expected a date with no precision finer than the millisecond, \
             got {date}"
        );
        by_name.insert(entry.name, date);
    }

    let distinct: std::collections::BTreeSet<i64> = by_name
        .values()
        .map(|date| (date * 1_000.0).round() as i64)
        .collect();
    // Amazon publishes its documents over years, so the catalogue carries many dates; one
    // instant shared by every entry is the store's own seeding clock, which is the defect
    // this pins. Two documents published together may legitimately share a date, so the
    // count is not required to be one per document.
    assert!(
        distinct.len() > 1,
        "the catalogue is dated per document rather than at one shared instant, got \
         {by_name:?}"
    );
}

/// One document means one date: the same published document reports the same
/// `CreatedDate` on a second read, through every Region's endpoint, and in a SECOND
/// emulator — so a restart cannot move it.
#[tokio::test]
async fn a_publication_date_is_the_same_in_every_region_and_in_a_second_emulator() {
    let first = router();
    let second = router();

    for entry in CATALOGUE {
        let case = entry.name;
        let date = created_date(case, &the_one_version(&first, case, entry.name).await);
        let again = created_date(case, &the_one_version(&first, case, entry.name).await);
        assert_eq!(
            again, date,
            "case {case}: a read must not move the publication date, got {again} \
             against {date}"
        );
        let restarted = created_date(case, &the_one_version(&second, case, entry.name).await);
        assert_eq!(
            restarted, date,
            "case {case}: a second emulator publishes the same catalogue, so the date is \
             unchanged by a restart, got {restarted} against {date}"
        );

        for region in ["us-east-1", "eu-central-1", "ap-southeast-2"] {
            let (status, response) = call_in_region(
                &first,
                region,
                "ListDocumentVersions",
                &json!({ "Name": entry.name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: a published document resolves through {region}, got {status} \
                 {response}"
            );
            let in_region = created_date(case, &response["DocumentVersions"][0]);
            assert_eq!(
                in_region, date,
                "case {case}: the publication date is the same in {region}, got \
                 {in_region} against {date}"
            );
        }
    }
}

/// The publication dates the catalogue of a real account carries for the documents whose
/// date it was recorded at, in epoch milliseconds.
///
/// Source: the `ssm` catalogue of the wire corpus `simparity-wire-aws-v1`
/// (`ListDocumentVersions` / `ListDocuments` over the Amazon-owned documents). A date is a
/// property of AWS's published document, so it is the same in every account and Region;
/// the rows below are the ones that corpus pins, and the suites above hold the rule for
/// every other entry of the catalogue.
const RECORDED_PUBLICATION_DATES_MILLIS: &[(&str, i64)] = &[
    ("AWS-ApplyAnsiblePlaybooks", 1_758_834_443_390),
    ("AWS-ConfigureAWSPackage", 1_706_038_480_989),
    ("AWS-ConfigureCloudWatch", 1_525_720_957_739),
    ("AWS-CreateImage", 1_533_661_478_787),
    ("AWS-GatherSoftwareInventory", 1_564_011_832_472),
    ("AWS-InstallWindowsUpdates", 1_781_017_685_843),
    ("AWS-PatchInstanceWithRollback", 1_761_232_679_723),
    ("AWS-RestartEC2Instance", 1_511_217_229_956),
    ("AWS-RunInspecChecks", 1_758_834_488_387),
    ("AWS-RunPatchBaseline", 1_786_631_569_844),
    ("AWS-RunPowerShellScript", 1_503_695_741_940),
    ("AWS-RunRemoteScript", 1_508_978_913_261),
    ("AWS-RunShellScript", 1_503_695_717_821),
    ("AWS-StartEC2Instance", 1_782_421_644_712),
    ("AWS-StartPortForwardingSession", 1_563_487_178_453),
    ("AWS-StartSSHSession", 1_562_087_817_013),
    ("AWS-StopEC2Instance", 1_531_242_313_476),
    ("AWS-UpdateLinuxAmi", 1_775_747_623_945),
    ("AWS-UpdateSSMAgent", 1_622_567_950_979),
];

/// The `CreatedDate` of a reported version or document in epoch MILLISECONDS, which is the
/// precision Systems Manager reports a document date at.
fn millis_of(case: &str, reported: &Value) -> i64 {
    (created_date(case, reported) * 1_000.0).round() as i64
}

/// Each document whose published date is known is served at THAT date, to the
/// millisecond.
#[tokio::test]
async fn the_known_publication_dates_are_served_as_the_catalogue_carries_them() {
    let router = router();
    for (name, expected_millis) in RECORDED_PUBLICATION_DATES_MILLIS {
        assert!(
            CATALOGUE.iter().any(|entry| entry.name == *name),
            "case {name}: the catalogue must publish a document the corpus pins a date for"
        );
        let version = the_one_version(&router, name, name).await;
        let millis = millis_of(name, &version);
        assert_eq!(
            millis, *expected_millis,
            "case {name}: expected the published date {expected_millis} in epoch \
             milliseconds, got {millis}"
        );
    }
}

/// A date the catalogue pins belongs to the DOCUMENT, not to the read that reported it:
/// every read of a published document — the version listing, the describe and the document
/// listing — reports that same date, and so does the same read through another Region's
/// endpoint. The rows span the document types the catalogue publishes (`Command`,
/// `Automation`, `Session`, `Policy`) and dates from a decade apart.
#[tokio::test]
async fn a_known_publication_date_is_reported_by_every_read_and_in_every_region() {
    let router = router();
    let listed = list_documents(&router).await;

    for (name, expected_millis) in RECORDED_PUBLICATION_DATES_MILLIS {
        let described = {
            let (status, response) = call(&router, "DescribeDocument", &json!({ "Name": name })).await;
            assert_eq!(
                status, 200,
                "case {name}: DescribeDocument must resolve a document AWS publishes, got \
                 {status} {response}"
            );
            millis_of(name, &response["Document"])
        };
        assert_eq!(
            described, *expected_millis,
            "case {name}: DescribeDocument must report the published date \
             {expected_millis} in epoch milliseconds, got {described}"
        );

        let identifier = listed.get(*name).unwrap_or_else(|| {
            panic!("case {name}: a listing must report every published document, got {listed:?}")
        });
        let in_listing = millis_of(name, identifier);
        assert_eq!(
            in_listing, *expected_millis,
            "case {name}: ListDocuments must report the published date {expected_millis} \
             in epoch milliseconds, got {in_listing}"
        );

        for region in ["us-east-1", "eu-west-3"] {
            let (status, response) =
                call_in_region(&router, region, "ListDocumentVersions", &json!({ "Name": name }))
                    .await;
            assert_eq!(
                status, 200,
                "case {name}: a published document resolves through {region}, got {status} \
                 {response}"
            );
            let in_region = millis_of(name, &response["DocumentVersions"][0]);
            assert_eq!(
                in_region, *expected_millis,
                "case {name}: the published date is {expected_millis} in epoch \
                 milliseconds through {region} too, got {in_region}"
            );
        }
    }
}

/// The catalogue is still a CLOSED set: a name under an AWS-reserved prefix that no
/// published document carries resolves to nothing, so no caller is handed a document AWS
/// does not publish.
#[tokio::test]
async fn an_unpublished_reserved_name_lists_no_versions() {
    let router = router();
    // (case, Name)
    let misses: &[(&str, &str)] = &[
        (
            "a name one letter past a published one",
            "AWS-ConfigureAWSPackages",
        ),
        (
            "a published name in another case: a document name is case-sensitive",
            "aws-startsshsession",
        ),
        ("a plausible but unpublished runbook", "AWS-RebootEC2Instance"),
        (
            "another reserved prefix the catalogue holds nothing under",
            "AWSSupport-NothingHere",
        ),
        ("a name no caller created", "MyOwnUnpublishedDocument"),
    ];
    for (case, name) in misses {
        let (status, response) = list_versions(&router, name).await;
        assert_eq!(
            status, 400,
            "case {case}: {name} names no document, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidDocument",
            "case {case}: expected InvalidDocument, got {response}"
        );
        assert!(
            response["DocumentVersions"].is_null(),
            "case {case}: a refusal carries no version list, got {response}"
        );
    }
}

/// Every `DocumentIdentifier` an unfiltered `ListDocuments` reports in [`REGION`], keyed
/// by name.
async fn list_documents(router: &MultiServiceRouter) -> BTreeMap<String, Value> {
    let (status, response) = call(router, "ListDocuments", &json!({ "MaxResults": 50 })).await;
    assert_eq!(
        status, 200,
        "ListDocuments must succeed in {REGION}, got {status} {response}"
    );
    let mut reported: BTreeMap<String, Value> = BTreeMap::new();
    let mut token = response["NextToken"].as_str().map(str::to_string);
    collect_identifiers(&response, &mut reported);
    // The catalogue is larger than one page, so the walk follows the token it hands back.
    while let Some(resume) = token.filter(|token| !token.is_empty()) {
        let (status, page) = call(
            router,
            "ListDocuments",
            &json!({ "MaxResults": 50, "NextToken": resume }),
        )
        .await;
        assert_eq!(
            status, 200,
            "a resumed ListDocuments must succeed, got {status} {page}"
        );
        collect_identifiers(&page, &mut reported);
        token = page["NextToken"].as_str().map(str::to_string);
    }
    reported
}

/// Fold one `ListDocuments` page into `reported`.
fn collect_identifiers(page: &Value, reported: &mut BTreeMap<String, Value>) {
    let identifiers = page["DocumentIdentifiers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for identifier in identifiers {
        let name = identifier["Name"].as_str().unwrap_or_default().to_string();
        reported.insert(name, identifier);
    }
}
