// SPDX-License-Identifier: CC0-1.0

//! What a bucket-configuration PUT does with the export destination's owning ACCOUNT ID.
//!
//! RULE: the account id of an export destination is an OPTIONAL element of the document S3
//! validates against its published schema, so it may be left out — "If no account ID is
//! provided, the owner is not validated before exporting data"
//! (`smithy_query(query="shape",shape="AnalyticsS3BucketDestination")`, same wording on
//! `InventoryS3BucketDestination$AccountId`) — or carry a value, which is STORED AS SUBMITTED
//! and reported back by every read of the configuration. The member targets
//! `com.amazonaws.s3#AccountId`, which `smithy_query(query="shape",shape="AccountId")`
//! reports as a plain `string` carrying no `length`, `pattern` or other constraint trait, so
//! the value's FORM is no part of the request contract: the write records the reference it
//! was given and resolves nothing, because the owner of the destination bucket is verified
//! when S3 WRITES an export, not when it records where to write one. An element the request
//! WROTE while leaving it empty is the one value that is neither: it names no account at all,
//! so the document did not validate against the published schema, and the call is refused
//! with S3's common `MalformedXML` (HTTP 400) carrying that code's fixed sentence, with
//! NOTHING stored — the bucket's configurations are exactly what they were, and the listing
//! still reports none.
//!
//! The rows span what the rule generalises over: both configuration kinds whose destination
//! carries that member (an analytics data export and an S3 Inventory destination), the three
//! states the member can be in — written empty (refused), left out and carrying a value (both
//! served, and read back verbatim) — and, for a value that IS stated, its FORM: the canonical
//! twelve digits beside shorter, longer, non-numeric, mixed, separated and much longer
//! spellings, none of which the model constrains. The empty-vs-omitted pair is what makes the
//! refusal about the member's VALUE rather than about the document shape; the form table is
//! what makes the acceptance about the CONTRACT rather than about one recorded spelling.
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
    bucket, configuration, get_analytics, put_analytics, reported_configurations, Configuration,
    DataExport, FOREIGN_ACCOUNT_ID,
};
use super::bucket_inventory_configuration::{
    get_inventory, put_inventory, reported_configurations as reported_inventory_configurations,
    Configuration as InventoryConfiguration, Destination,
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
/// nothing after a refused put and one entry per configuration a served put recorded.
async fn list_analytics(router: &MultiServiceRouter, name: &str) -> Answer {
    send(router, "GET", &format!("/{name}?analytics"), &[], "").await
}

/// The spellings an owning-account value can take, which is the dimension the rule
/// generalises over.
///
/// The member targets `com.amazonaws.s3#AccountId`, which
/// `smithy_query(query="shape",shape="AccountId")` reports as a plain `string` with no
/// constraint trait at all, and the member's own documentation states only what an ABSENT
/// value means. So the twelve-digit spelling of an account id is ONE row here rather than
/// the rule: every row is a value the published schema admits, and every row must therefore
/// be recorded and reported back exactly as the request wrote it. The rows differ along what
/// a form can differ in — length (shorter, longer, one character, far longer than any
/// account id), character class (digits, letters, mixed, upper case), and punctuation
/// (separators, an underscore).
fn account_id_forms() -> Vec<(&'static str, String)> {
    vec![
        (
            "the canonical twelve digits of another account",
            FOREIGN_ACCOUNT_ID.to_string(),
        ),
        ("a non-numeric owner token", "destination-owner".to_string()),
        ("fewer digits than an account id", "1234".to_string()),
        (
            "more digits than an account id",
            "1234567890123456".to_string(),
        ),
        ("digits and letters mixed", "0123abcd4567".to_string()),
        (
            "twelve digits written with separators",
            "1111-2222-3333".to_string(),
        ),
        ("a single character", "7".to_string()),
        (
            "an upper-case token carrying an underscore",
            "Owner_Account".to_string(),
        ),
        ("a value far longer than an account id", "o".repeat(64)),
    ]
}

#[tokio::test]
async fn an_analytics_export_account_id_is_recorded_whatever_form_it_takes() {
    let router = router();
    let name = "analytics-account-forms";
    bucket(&router, name).await;

    let forms = account_id_forms();
    let mut wanted_all: Vec<Configuration> = Vec::new();

    for (index, (case, account)) in forms.iter().enumerate() {
        let id = format!("account-form-{index}");
        let wanted = configuration(
            &id,
            None,
            Some(DataExport::new(
                "analytics-destination",
                Some(account.as_str()),
                Some("exports/"),
            )),
        );
        let put = put_analytics(&router, name, &wanted).await;
        assert_eq!(
            put.status, PUT_BUCKET_ANALYTICS_CONFIGURATION_SUCCESS_STATUS,
            "case {case:?}: the put must be served, got {} with {:?}",
            put.status, put.body
        );
        assert!(
            put.body.is_empty(),
            "case {case:?}: a served put answers an empty body, got {:?}",
            put.body
        );

        // The per-id read reports the configuration the put carried, down to the owning
        // account the destination stated.
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
        wanted_all.push(wanted);
    }

    // And the bucket's whole listing reports one entry per recorded configuration, each with
    // its own owning account — the same projection, over every row at once.
    let listed = list_analytics(&router, name).await;
    assert_eq!(
        listed.status, 200,
        "the listing must be served, got {} with {:?}",
        listed.status, listed.body
    );
    let reported = reported_configurations(&listed);
    for wanted in &wanted_all {
        assert!(
            reported.contains(wanted),
            "case {:?}: the listing must report the stored configuration, got {:?}",
            wanted.id,
            listed.body
        );
    }
    assert_eq!(
        reported.len(),
        wanted_all.len(),
        "the listing must report exactly the recorded configurations, got {:?}",
        listed.body
    );
}

#[tokio::test]
async fn a_second_analytics_put_replaces_the_owning_account_it_recorded() {
    let router = router();
    let name = "analytics-account-replacement";
    bucket(&router, name).await;
    let id = "account-replacement";

    // Each step is the configuration the bucket must hold after it: a destination with no
    // owning account, then one that states an owner of each form, then one that drops the
    // member again — a put replaces the configuration in full, so what the last put wrote is
    // what the read reports.
    let mut steps: Vec<(String, Option<String>)> =
        vec![("the member left out".to_string(), None)];
    steps.extend(
        account_id_forms()
            .into_iter()
            .map(|(case, account)| (case.to_string(), Some(account))),
    );
    steps.push(("the member left out again".to_string(), None));

    for (case, account) in &steps {
        let wanted = configuration(
            id,
            None,
            Some(DataExport::new(
                "analytics-destination",
                account.as_deref(),
                None,
            )),
        );
        let put = put_analytics(&router, name, &wanted).await;
        assert_eq!(
            put.status, PUT_BUCKET_ANALYTICS_CONFIGURATION_SUCCESS_STATUS,
            "case {case:?}: the put must be served, got {} with {:?}",
            put.status, put.body
        );
        let read = get_analytics(&router, name, id).await;
        assert_eq!(
            reported_configurations(&read),
            vec![wanted],
            "case {case:?}: the read must report what the LAST put recorded, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn an_inventory_destination_account_id_is_recorded_whatever_form_it_takes() {
    let router = router();
    let name = "inventory-account-forms";
    bucket(&router, name).await;

    // The enum members come from the crate's generated catalogs, so the document is one the
    // service accepts for every reason but the account id.
    let versions = INVENTORY_INCLUDED_OBJECT_VERSIONS_VALUES[0];
    let frequency = INVENTORY_FREQUENCY_VALUES[0];
    let format = INVENTORY_FORMAT_VALUES[0];

    for (index, (case, account)) in account_id_forms().iter().enumerate() {
        let id = format!("account-form-{index}");
        let wanted = InventoryConfiguration::new(
            &id,
            versions,
            frequency,
            Destination::new(format).owned_by(account),
        );
        let put = put_inventory(&router, name, &wanted).await;
        assert_eq!(
            put.status, PUT_BUCKET_INVENTORY_CONFIGURATION_SUCCESS_STATUS,
            "case {case:?}: the put must be served, got {} with {:?}",
            put.status, put.body
        );
        let read = get_inventory(&router, name, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the stored configuration must read back, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            reported_inventory_configurations(&read),
            vec![wanted],
            "case {case:?}: the read must report the configuration the put carried, got {:?}",
            read.body
        );
    }
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
