// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PAGE `GetClientCertificates` answers with, on an account that
//! holds more certificates than one default-sized page reports.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. [`Gateway`], `page_items` and `string_member` are this
//! crate's one wire-test harness, reused from `rest_api_lifecycle`.
//!
//! The rules under test, each over a SET of inputs rather than one call:
//!
//! * the account's client-certificate collection is cut to the size the request asked for,
//!   held inside the range the `limit` member publishes, and to the published DEFAULT size
//!   when the request asks for no size at all; the page reports the certificates at the
//!   front of the collection in generation order and carries on at the first certificate it
//!   did not report;
//! * walking the collection by that default size reaches every stored certificate exactly
//!   once, in that same order;
//! * listing is a READ: a listed certificate is the record its own `GetClientCertificate`
//!   reports, member for member, and the collection answers identically however many times
//!   it is listed.
//!
//! Source of the range: the `limit` member of this operation — "The maximum number of
//! returned results per page. The default value is 25 and the maximum value is 500"
//! (`smithy_query(query="operation", operation="GetClientCertificates", model_path=\
//! "services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/\
//! com-amazonaws-apigateway-backplanecontrolservice.json")`) — read here through the
//! constants the service itself pages by.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    DEFAULT_PAGE_LIMIT, MAX_CLIENT_CERTIFICATES, MAX_PAGE_LIMIT,
};

use crate::client_certificates::{generated, list_certificates};
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};

/// How many certificates the account is stood up with: one more than a default-sized page
/// reports, so the default cut leaves a certificate after it, and well inside the account's
/// published certificate quota ([`MAX_CLIENT_CERTIFICATES`]).
const COUNT: usize = DEFAULT_PAGE_LIMIT as usize + 1;

/// That count stands inside the account's published certificate quota, so every generate
/// the suite makes is admitted rather than refused at the quota.
const _: () = assert!(
    COUNT <= MAX_CLIENT_CERTIFICATES,
    "the suite must stand up fewer certificates than the account's quota"
);

/// The description the certificate generated at `index` carries.
fn description(index: usize) -> String {
    format!("collection page {index:02}")
}

/// One page of the collection, with `query` appended to its URI (`""` for none), asserting
/// the 200 every page of a resolvable cursor answers. The request itself is the sibling
/// suite's one `GetClientCertificates` helper.
async fn page(gateway: &Gateway, query: &str, case: &str) -> Value {
    let answer = list_certificates(gateway, query).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from GetClientCertificates, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The `clientCertificateId` of every certificate on `page`, in page order.
fn reported_ids(page: &Value, case: &str) -> Vec<String> {
    page_items(page, case)
        .iter()
        .map(|item| string_member(item, "clientCertificateId", case))
        .collect()
}

/// Generate [`COUNT`] client certificates, answering their ids in generation order. The
/// generate itself is the sibling suite's one `GenerateClientCertificate` helper, which
/// asserts the modelled 201.
async fn generated_collection(gateway: &Gateway) -> Vec<String> {
    let mut ids = Vec::with_capacity(COUNT);
    for index in 0..COUNT {
        let case = format!("GenerateClientCertificate(#{index})");
        let body = generated(
            gateway,
            &case,
            json!({ "description": description(index) }),
        )
        .await;
        ids.push(string_member(&body, "clientCertificateId", &case));
    }
    ids
}

#[tokio::test]
async fn a_page_of_the_accounts_certificates_is_cut_to_the_size_the_request_asked_for() {
    let gateway = Gateway::new();
    let generated = generated_collection(&gateway).await;

    // Rows differ along the page size the request asks for: none at all (the published
    // default), below the published minimum, at it, inside the range, the default stated
    // outright, the whole collection, the published maximum, one past it and far past it.
    let rows: Vec<Option<i64>> = vec![
        None,
        Some(-1),
        Some(0),
        Some(1),
        Some(2),
        Some(i64::from(DEFAULT_PAGE_LIMIT)),
        Some(i64::try_from(COUNT).unwrap_or_default()),
        Some(i64::from(MAX_PAGE_LIMIT)),
        Some(i64::from(MAX_PAGE_LIMIT) + 1),
        Some(10_000),
    ];

    for requested in rows {
        let case = match requested {
            None => "GetClientCertificates(no limit)".to_string(),
            Some(size) => format!("GetClientCertificates(limit={size})"),
        };
        let query = match requested {
            None => String::new(),
            Some(size) => format!("?limit={size}"),
        };
        // The size the collection is cut to: the requested one held inside the published
        // range, and the published default for a request that asked for none.
        let served = usize::try_from(
            requested
                .unwrap_or(i64::from(DEFAULT_PAGE_LIMIT))
                .clamp(1, i64::from(MAX_PAGE_LIMIT)),
        )
        .unwrap_or(1);
        let held = served.min(COUNT);

        let body = page(&gateway, &query, &case).await;
        let ids = reported_ids(&body, &case);
        assert_eq!(
            ids,
            generated[..held].to_vec(),
            "case {case}: expected the first {held} certificate(s) in generation order, got \
             {ids:?}"
        );
        // A cursor is handed back exactly while the collection holds a certificate the page
        // did NOT report, and it is this service's own token: it spells no certificate id,
        // and resuming at it reports the records the page stopped before.
        let remaining = generated.get(held).is_some();
        let cursor = body["position"].as_str().map(str::to_owned);
        assert_eq!(
            cursor.is_some(),
            remaining,
            "case {case}: a cursor must be reported exactly while records remain, got {:?}",
            body["position"]
        );
        if let Some(cursor) = &cursor {
            assert!(
                generated.iter().all(|id| !cursor.contains(id.as_str())),
                "case {case}: a cursor spells no certificate's id, got {cursor:?}"
            );
            let resumed = page(&gateway, &format!("?position={cursor}&limit=500"), &case).await;
            let rest = reported_ids(&resumed, &case);
            assert_eq!(
                rest,
                generated[held..].to_vec(),
                "case {case}: resuming at the cursor expected {:?}, got {rest:?}",
                &generated[held..]
            );
        }
        // The served size travels with the cursor, and only for a request that asked for a
        // size: a default-sized page states none.
        let echoed = requested.and(cursor.as_ref()).map(|_| served);
        assert_eq!(
            body["limit"].as_i64().map(|size| size as usize),
            echoed,
            "case {case}: expected the echoed page size {echoed:?}, got {:?}",
            body["limit"]
        );
    }
}

#[tokio::test]
async fn a_default_sized_walk_reaches_every_stored_certificate_once_in_generation_order() {
    let gateway = Gateway::new();
    let generated = generated_collection(&gateway).await;

    let mut walked: Vec<String> = Vec::new();
    let mut position: Option<String> = None;
    loop {
        let case = format!("a default-sized walk at {position:?}");
        let query = match &position {
            None => String::new(),
            Some(cursor) => format!("?position={cursor}"),
        };
        let body = page(&gateway, &query, &case).await;
        let ids = reported_ids(&body, &case);
        assert!(
            ids.len() <= DEFAULT_PAGE_LIMIT as usize,
            "case {case}: a default-sized page holds at most {DEFAULT_PAGE_LIMIT} \
             certificate(s), got {}",
            ids.len()
        );
        assert!(
            !ids.is_empty(),
            "case {case}: a page of a collection that holds certificates holds some, got \
             {body:?}"
        );
        walked.extend(ids);
        match body["position"].as_str() {
            None => break,
            Some(cursor) => position = Some(cursor.to_owned()),
        }
    }
    assert_eq!(
        walked, generated,
        "case a default-sized walk: expected every certificate once, in generation order, \
         got {walked:?}"
    );
}

#[tokio::test]
async fn listing_the_collection_reports_the_stored_records_and_changes_none_of_them() {
    let gateway = Gateway::new();
    let generated = generated_collection(&gateway).await;

    // The whole collection on one page, so every record can be checked against its own
    // read.
    let case = "the whole collection";
    let first = page(&gateway, &format!("?limit={COUNT}"), case).await;
    let items = page_items(&first, case);
    assert_eq!(
        items.len(),
        COUNT,
        "case {case}: expected every certificate on the page, got {items:?}"
    );

    // Each listed certificate IS the record its own read reports, member for member, and
    // carries the description its generate set.
    for (index, item) in items.iter().enumerate() {
        let case = format!("the certificate at {index}");
        let id = string_member(item, "clientCertificateId", &case);
        assert_eq!(
            id, generated[index],
            "case {case}: expected the certificate generated at {index}, got {id:?}"
        );
        assert_eq!(
            item["description"].as_str(),
            Some(description(index).as_str()),
            "case {case}: expected the description its generate set, got {:?}",
            item["description"]
        );
        let read = gateway
            .call("GET", &format!("/clientcertificates/{id}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetClientCertificate, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            &read.body, item,
            "case {case}: the listed certificate and its own read are one record, got {:?} \
             beside {item:?}",
            read.body
        );
    }

    // Listing again — after every read above — answers the very same page: a listing
    // consumes no certificate and moves none.
    let again = page(&gateway, &format!("?limit={COUNT}"), "the collection again").await;
    assert_eq!(
        again, first,
        "case the collection again: expected the same page, got {again:?} after {first:?}"
    );
}
