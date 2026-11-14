// SPDX-License-Identifier: CC0-1.0

//! What a bucket-configuration PUT answers when the export destination's owning ACCOUNT ID
//! is written EMPTY.
//!
//! RULE: the account id of an export destination is an OPTIONAL element of the document S3
//! validates against its published schema, so it may be left out — "If no account ID is
//! provided, the owner is not validated before exporting data"
//! (`smithy_query(query="shape",shape="AnalyticsS3BucketDestination")`, same wording on
//! `InventoryS3BucketDestination$AccountId`) — or carry an account's id, which is stored as
//! submitted whichever account it names. An element the request WROTE while leaving it empty
//! is neither: it names no account, so the document did not validate against the published
//! schema, and the call is refused with S3's common `MalformedXML` (HTTP 400) carrying that
//! code's fixed sentence, with NOTHING stored — the bucket's configurations are exactly what
//! they were, and the listing still reports none.
//!
//! The rows span what the rule generalises over: both configuration kinds whose destination
//! carries that member (an analytics data export and an S3 Inventory destination), and the
//! three states the member can be in — written empty (refused), left out and naming a
//! 12-digit foreign account (both served, and read back verbatim). The empty-vs-omitted pair
//! is what makes the refusal about the member's VALUE rather than about the document shape.
//!
//! The wire harness, the document writers and the readers are the two suites' own
//! ([`super::bucket_analytics_configuration`], [`super::bucket_inventory_configuration`]),
//! reused rather than copied.

use service_com_amazonaws_s3_amazons3::ops::put_bucket_analytics_configuration::PUT_BUCKET_ANALYTICS_CONFIGURATION_SUCCESS_STATUS;
use service_com_amazonaws_s3_amazons3::ops::put_bucket_inventory_configuration::PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS;
use service_com_amazonaws_s3_amazons3::ops::types::{
    INVENTORY_FORMAT_VALUES, INVENTORY_FREQUENCY_VALUES, INVENTORY_INCLUDED_OBJECT_VERSIONS_VALUES,
};
use service_com_amazonaws_s3_amazons3::state::{MALFORMED_XML_CODE, MALFORMED_XML_MESSAGE};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_analytics_configuration::{
    bucket, configuration, get_analytics, put_analytics, reported_configurations, DataExport,
    FOREIGN_ACCOUNT_ID,
};
use super::bucket_inventory_configuration::{
    get_inventory, put_inventory, Configuration as InventoryConfiguration, Destination,
};
use super::bucket_lifecycle::{router, send, Answer};

/// The code and the sentence a document that did not validate is refused with, read off the
/// crate's own producers: the wording is pinned by the suites that own those constants, and
/// what this suite states is WHICH requests earn it.
fn assert_malformed(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        MALFORMED_XML_CODE,
        "case {case:?}: expected {MALFORMED_XML_CODE}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.error_message(),
        MALFORMED_XML_MESSAGE,
        "case {case:?}: expected the sentence {MALFORMED_XML_MESSAGE:?}, got {:?}",
        answer.error_message()
    );
}

/// `GET /<bucket>?analytics` — the bucket's whole analytics listing, which must report
/// nothing after a refused put.
async fn list_analytics(router: &MultiServiceRouter, name: &str) -> Answer {
    send(router, "GET", &format!("/{name}?analytics"), &[], "").await
}

#[tokio::test]
async fn an_analytics_export_account_id_written_empty_is_refused_and_stores_nothing() {
    let router = router();
    let name = "analytics-empty-account";
    bucket(&router, name).await;

    // (case, the BucketAccountId the destination carries, whether S3 records it)
    let rows: Vec<(&str, Option<&str>, bool)> = vec![
        ("the member written empty", Some(""), false),
        ("the member left out", None, true),
        (
            "a 12-digit account that is not this one",
            Some(FOREIGN_ACCOUNT_ID),
            true,
        ),
    ];

    for (index, (case, account, served)) in rows.iter().enumerate() {
        let id = format!("empty-account-{index}");
        let wanted = configuration(
            &id,
            None,
            Some(DataExport::new("analytics-destination", *account, None)),
        );
        let answer = put_analytics(&router, name, &wanted).await;
        if *served {
            assert_eq!(
                answer.status, PUT_BUCKET_ANALYTICS_CONFIGURATION_SUCCESS_STATUS,
                "case {case:?}: expected the put to be served, got {} with {:?}",
                answer.status, answer.body
            );
            let read = get_analytics(&router, name, &id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the stored configuration must read back, got {} with {:?}",
                read.status, read.body
            );
            assert_eq!(
                reported_configurations(&read),
                vec![wanted.clone()],
                "case {case:?}: the read must report the configuration the put carried, got {:?}",
                read.body
            );
        } else {
            assert_malformed(case, &answer);
            // Nothing was stored: neither the read of that id nor the bucket's listing knows
            // the configuration the refused put named.
            let read = get_analytics(&router, name, &id).await;
            assert_eq!(
                read.status, 404,
                "case {case:?}: a refused put must store nothing, got {} with {:?}",
                read.status, read.body
            );
            let listed = list_analytics(&router, name).await;
            assert!(
                !listed.elements("Id").contains(&id),
                "case {case:?}: the refused id must not be listed, got {:?}",
                listed.body
            );
        }
    }
}

#[tokio::test]
async fn an_inventory_destination_account_id_written_empty_is_refused_and_stores_nothing() {
    let router = router();
    let name = "inventory-empty-account";
    bucket(&router, name).await;

    // The three states of the same member, on the other configuration kind that carries it.
    // The enum members are taken from the crate's generated catalogs, so the document is one
    // the service accepts for every reason but the account id.
    let versions = INVENTORY_INCLUDED_OBJECT_VERSIONS_VALUES[0];
    let frequency = INVENTORY_FREQUENCY_VALUES[0];
    let format = INVENTORY_FORMAT_VALUES[0];

    let rows: Vec<(&str, Option<&str>, bool)> = vec![
        ("the member written empty", Some(""), false),
        ("the member left out", None, true),
        (
            "a 12-digit account that is not this one",
            Some(FOREIGN_ACCOUNT_ID),
            true,
        ),
    ];

    for (index, (case, account, served)) in rows.iter().enumerate() {
        let id = format!("empty-account-{index}");
        let destination = match account {
            Some(account) => Destination::new(format).owned_by(account),
            None => Destination::new(format),
        };
        let wanted = InventoryConfiguration::new(&id, versions, frequency, destination);
        let answer = put_inventory(&router, name, &wanted).await;
        if *served {
            assert_eq!(
                answer.status, PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
                "case {case:?}: expected the put to be served, got {} with {:?}",
                answer.status, answer.body
            );
            let read = get_inventory(&router, name, &id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the stored configuration must read back, got {} with {:?}",
                read.status, read.body
            );
        } else {
            assert_malformed(case, &answer);
            let read = get_inventory(&router, name, &id).await;
            assert_eq!(
                read.status, 404,
                "case {case:?}: a refused put must store nothing, got {} with {:?}",
                read.status, read.body
            );
        }
    }
}
