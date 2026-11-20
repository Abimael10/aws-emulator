// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListCertificates`: the paginated reader over the
//! account's X.509 device-certificate store.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 `GET /certificates` request signed for the `iot` signing name — so what is
//! asserted is what an AWS SDK client would see, not an internal function's return
//! value.
//!
//! The rule under test: a paginated listing reports ONE PAGE of the records its store
//! holds, in the order the operation documents, each entry projected from the record the
//! create persisted, and returns a continuation token only when records remain, which
//! resumes that same listing exactly once; the listing writes nothing. Here that means
//! `ListCertificates` reports every certificate of the account as the modelled
//! `Certificate` — `certificateArn`, `certificateId`, `status`, `creationDate` and, only
//! for a certificate whose mode is not the default one, `certificateMode` — and no
//! certificate material — ordered by creation date, ascending
//! when `ascendingOrder` is true and descending otherwise
//! (`smithy_query(query="operation",operation="ListCertificates")`), with at most
//! `pageSize` entries per page and the documented default page size of 25 when the
//! request names none, and `nextMarker` present only while certificates remain.
//!
//! Each table spans what the rule generalises over rather than one recorded request: the
//! three forms `ascendingOrder` can take on the wire (absent — the modelled `default
//! false` — explicitly false, explicitly true), every page size from one to past the
//! number of certificates held plus the modelled maximum, the number of certificates in
//! the store from none upward, and the statuses the stored records carry.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::Value;
use service_com_amazonaws_iot_awsiotservice::state::{
    certificate_arn, service_state, RegisteredCertificate, CERTIFICATE_MODE_DEFAULT,
    CERTIFICATE_STATUS_ACTIVE, CERTIFICATE_STATUS_INACTIVE, DEFAULT_CERTIFICATE_PAGE_SIZE,
    FIRST_CERTIFICATE_CUSTOMER_VERSION, MAX_LIST_PAGE_SIZE,
};
use service_com_amazonaws_iot_awsiotservice::state::ca_certificates::CERTIFICATE_MODE_SNI_ONLY;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service, and how the service picks the
/// region its ARNs are scoped to. The signature itself is never checked here — the
/// harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The reported `certificates` list, or `None` when the body carries no list.
    fn certificates(&self) -> Option<&Vec<Value>> {
        self.body.get("certificates").and_then(Value::as_array)
    }

    /// The `certificateId`s the page reports, in the order it reports them.
    fn reported_ids(&self) -> Vec<String> {
        self.certificates()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        entry
                            .get("certificateId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `nextMarker` the page carries, when it carries one.
    fn next_marker(&self) -> Option<String> {
        self.text("nextMarker")
    }
}

/// One certificate the store holds, as the create and `DescribeCertificate` report it.
#[derive(Debug, Clone)]
struct Issued {
    certificate_id: String,
    status: &'static str,
    /// The `creationDate` `DescribeCertificate` reports, which is the ordering key of
    /// every certificate listing.
    creation_date: f64,
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it. The engine is kept so a case can
/// record a certificate whose mode no implemented operation mints.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, path: &str) -> Reply {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::new())
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, with `setAsActive`
    /// bound as an `@httpQuery` parameter.
    async fn issue(&self, set_as_active: bool) -> Issued {
        let created = self
            .call(
                "POST",
                &format!("/keys-and-certificate?setAsActive={set_as_active}"),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case setAsActive={set_as_active}: CreateKeysAndCertificate must succeed, \
             got {created:?}"
        );
        let certificate_id = created.text("certificateId").unwrap_or_else(|| {
            panic!(
                "case setAsActive={set_as_active}: the create must report a certificateId, \
                 got {created:?}"
            )
        });
        let described = self.describe(&certificate_id).await;
        let creation_date = described
            .body
            .get("certificateDescription")
            .and_then(|description| description.get("creationDate"))
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                panic!(
                    "case {certificate_id:?}: DescribeCertificate must report a creationDate, \
                     got {described:?}"
                )
            });
        Issued {
            certificate_id,
            status: if set_as_active {
                CERTIFICATE_STATUS_ACTIVE
            } else {
                CERTIFICATE_STATUS_INACTIVE
            },
            creation_date,
        }
    }

    async fn describe(&self, certificate_id: &str) -> Reply {
        self.call("GET", &format!("/certificates/{certificate_id}"))
            .await
    }

    /// Record one certificate in the account's store directly, in `mode`.
    ///
    /// It is the only way to hold a certificate whose `certificateMode` is not the one
    /// `CreateKeysAndCertificate` resolves for what it generates: the operations that
    /// register a certificate of the caller's own in another mode are still scaffolds, so
    /// no wire path writes such a record yet.
    fn record_certificate_in_mode(&self, index: usize, mode: &str, now: SystemTime) -> String {
        // A `certificateId` is `length 64..64, pattern ^(0x)?[a-fA-F0-9]+$`
        // (`smithy_query(query="operation",operation="DescribeCertificate")`), so the
        // index is rendered as hex and padded to the one length the model admits.
        let certificate_id = format!("{:0>64}", format!("{index:x}"));
        let state = service_state(&*self.engine).expect("the IoT state store");
        state.lock().certificates.insert(
            certificate_id.clone(),
            RegisteredCertificate {
                certificate_id: certificate_id.clone(),
                certificate_arn: certificate_arn(REGION, &certificate_id),
                certificate_pem: String::new(),
                status: CERTIFICATE_STATUS_ACTIVE.to_string(),
                owned_by: ACCOUNT_ID.to_string(),
                creation_date: now,
                last_modified_date: now,
                customer_version: FIRST_CERTIFICATE_CUSTOMER_VERSION,
                generation_id: format!("recorded-in-{mode}"),
                not_before: now,
                not_after: now,
                certificate_mode: mode.to_string(),
            },
        );
        certificate_id
    }

    /// `ListCertificates`: `GET /certificates`, with every input member bound as an
    /// `@httpQuery` parameter (`ascendingOrder` under the wire name
    /// `isAscendingOrder`).
    async fn list(&self, order: Order, page_size: Option<i32>, marker: Option<&str>) -> Reply {
        let mut query: Vec<String> = Vec::new();
        if let Some(ascending) = order.wire_value() {
            query.push(format!("isAscendingOrder={ascending}"));
        }
        if let Some(page_size) = page_size {
            query.push(format!("pageSize={page_size}"));
        }
        if let Some(marker) = marker {
            query.push(format!("marker={marker}"));
        }
        let path = if query.is_empty() {
            "/certificates".to_string()
        } else {
            format!("/certificates?{}", query.join("&"))
        };
        self.call("GET", &path).await
    }

    /// Every page of one listing, walked to exhaustion: the ids in the order they were
    /// reported, and the length of each page. The deadline is the number of pages, not a
    /// clock: a listing of `held` certificates at `page_size` each cannot take more than
    /// `held + 1` pages, so a pager that fails to advance is caught instead of looping.
    async fn walk(&self, order: Order, page_size: Option<i32>, held: usize) -> (Vec<String>, Vec<usize>) {
        let label = order.label();
        let mut ids: Vec<String> = Vec::new();
        let mut page_lengths: Vec<usize> = Vec::new();
        let mut marker: Option<String> = None;
        for page_number in 0..=held.saturating_add(1) {
            let page = self.list(order, page_size, marker.as_deref()).await;
            assert_eq!(
                page.status, 200,
                "case {label:?} pageSize={page_size:?} page {page_number}: the listing must \
                 succeed, got {page:?}"
            );
            let reported = page.reported_ids();
            page_lengths.push(reported.len());
            ids.extend(reported);
            marker = page.next_marker();
            if marker.is_none() {
                return (ids, page_lengths);
            }
        }
        panic!(
            "case {label:?} pageSize={page_size:?}: the listing never ran out of pages over \
             {held} certificates, reported {ids:?}"
        );
    }
}

/// The three forms `ascendingOrder` takes on the wire, and the order each one asks for.
#[derive(Debug, Clone, Copy)]
enum Order {
    /// The member is absent: the modelled `default false`, so descending.
    Absent,
    /// `isAscendingOrder=false`.
    Descending,
    /// `isAscendingOrder=true`.
    Ascending,
}

impl Order {
    fn wire_value(self) -> Option<bool> {
        match self {
            Self::Absent => None,
            Self::Descending => Some(false),
            Self::Ascending => Some(true),
        }
    }

    /// Whether this form asks for the ascending creation-date order.
    fn is_ascending(self) -> bool {
        matches!(self, Self::Ascending)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Absent => "no isAscendingOrder",
            Self::Descending => "isAscendingOrder=false",
            Self::Ascending => "isAscendingOrder=true",
        }
    }
}

/// Every form of the order member, which every table below spans.
const ORDERS: [Order; 3] = [Order::Absent, Order::Descending, Order::Ascending];

/// The ids `held` must be reported in, in `order`: the creation date decides the order and
/// `ascendingOrder` its direction. The `certificateId` only makes that order total — it
/// breaks a tie between two certificates created in the same instant, the same way in both
/// directions — so it is compared ascending whichever direction the listing runs.
fn expected_ids(held: &[Issued], order: Order) -> Vec<String> {
    let mut sorted = held.to_vec();
    sorted.sort_by(|left, right| {
        let by_creation_date = if order.is_ascending() {
            left.creation_date.total_cmp(&right.creation_date)
        } else {
            right.creation_date.total_cmp(&left.creation_date)
        };
        by_creation_date.then_with(|| left.certificate_id.cmp(&right.certificate_id))
    });
    sorted
        .into_iter()
        .map(|issued| issued.certificate_id)
        .collect()
}

/// Issue `count` certificates, alternating `setAsActive` so the store holds both statuses
/// a created certificate can carry.
async fn issue_certificates(emulator: &Emulator, count: usize) -> Vec<Issued> {
    let mut issued = Vec::new();
    for index in 0..count {
        issued.push(emulator.issue(index % 2 == 0).await);
    }
    issued
}

#[tokio::test]
async fn list_certificates_reports_every_certificate_in_creation_date_order() {
    let emulator = Emulator::new();
    let held = issue_certificates(&emulator, 4).await;

    for order in ORDERS {
        let label = order.label();
        let page = emulator.list(order, None, None).await;
        assert_eq!(
            page.status, 200,
            "case {label:?}: the listing must succeed, got {page:?}"
        );
        assert_eq!(
            page.reported_ids(),
            expected_ids(&held, order),
            "case {label:?}: expected the certificates in creation-date order, got {page:?}"
        );
        // Four certificates fit inside the default page, so nothing remains.
        assert!(
            page.next_marker().is_none(),
            "case {label:?}: a listing that returned every certificate carries no nextMarker, \
             got {page:?}"
        );
    }
}

#[tokio::test]
async fn an_empty_store_lists_no_certificates_and_no_marker() {
    let emulator = Emulator::new();

    for order in ORDERS {
        let label = order.label();
        let page = emulator.list(order, None, None).await;
        assert_eq!(
            page.status, 200,
            "case {label:?}: listing an empty store must succeed, got {page:?}"
        );
        assert_eq!(
            page.reported_ids(),
            Vec::<String>::new(),
            "case {label:?}: an account holding no certificate reports none, got {page:?}"
        );
        assert!(
            page.next_marker().is_none(),
            "case {label:?}: nothing remains after an empty page, got {page:?}"
        );
    }
}

#[tokio::test]
async fn each_entry_is_the_stored_record_and_carries_no_certificate_material() {
    let emulator = Emulator::new();
    let held = issue_certificates(&emulator, 3).await;

    let page = emulator.list(Order::Ascending, None, None).await;
    let entries = page
        .certificates()
        .unwrap_or_else(|| panic!("the listing must report a certificates list, got {page:?}"))
        .clone();
    assert_eq!(
        entries.len(),
        held.len(),
        "expected {} entries, got {page:?}",
        held.len()
    );

    for issued in &held {
        let label = &issued.certificate_id;
        let entry = entries
            .iter()
            .find(|entry| {
                entry.get("certificateId").and_then(Value::as_str) == Some(label.as_str())
            })
            .unwrap_or_else(|| panic!("case {label:?}: the listing must report it, got {page:?}"));
        assert_eq!(
            entry.get("certificateArn").and_then(Value::as_str),
            Some(format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{label}").as_str()),
            "case {label:?}: expected the certificate's ARN, got {entry:?}"
        );
        assert_eq!(
            entry.get("status").and_then(Value::as_str),
            Some(issued.status),
            "case {label:?}: expected status {:?}, got {entry:?}",
            issued.status
        );
        assert_eq!(
            entry.get("certificateMode"),
            None,
            "case {label:?}: a certificate in the default mode has no mode to report in a \
             listing, got {entry:?}"
        );
        assert_eq!(
            entry.get("creationDate").and_then(Value::as_f64),
            Some(issued.creation_date),
            "case {label:?}: expected the creationDate DescribeCertificate reports ({}), \
             got {entry:?}",
            issued.creation_date
        );
        // The modelled `Certificate` carries the identity, the status, the mode and the
        // creation date and nothing else
        // (`smithy_query(query="shape",shape="Certificate")`), so a listing hands out no
        // certificate material.
        for absent in ["certificatePem", "keyPair", "ownedBy"] {
            assert!(
                entry.get(absent).is_none(),
                "case {label:?}: a listing entry carries no {absent}, got {entry:?}"
            );
        }
    }
    let rendered = page.body.to_string();
    assert!(
        !rendered.contains("PRIVATE KEY") && !rendered.contains("CERTIFICATE--"),
        "a certificate listing carries no certificate material, got {page:?}"
    );
}

#[tokio::test]
async fn every_page_size_walks_every_certificate_exactly_once() {
    let emulator = Emulator::new();
    let held = issue_certificates(&emulator, 5).await;

    // Every page size from one to past the number of certificates held, plus the
    // modelled maximum `PageSize` (`range 1..250`) and the absent member.
    let page_sizes: Vec<Option<i32>> = vec![
        Some(1),
        Some(2),
        Some(3),
        Some(5),
        Some(6),
        Some(MAX_LIST_PAGE_SIZE),
        None,
    ];
    for order in ORDERS {
        for page_size in &page_sizes {
            let label = order.label();
            let (ids, page_lengths) = emulator.walk(order, *page_size, held.len()).await;
            assert_eq!(
                ids,
                expected_ids(&held, order),
                "case {label:?} pageSize={page_size:?}: paging must walk the listing's order \
                 exactly once, got pages of {page_lengths:?}"
            );
            let bound = page_size.unwrap_or(DEFAULT_CERTIFICATE_PAGE_SIZE) as usize;
            for (page_number, length) in page_lengths.iter().enumerate() {
                assert!(
                    *length <= bound,
                    "case {label:?} pageSize={page_size:?}: page {page_number} holds {length} \
                     entries, more than the page bound {bound}"
                );
            }
            // Only the last page of a listing may be short: a `nextMarker` is returned
            // while certificates remain, not instead of filling a page.
            for (page_number, length) in page_lengths
                .iter()
                .enumerate()
                .take(page_lengths.len().saturating_sub(1))
            {
                assert_eq!(
                    *length, bound,
                    "case {label:?} pageSize={page_size:?}: page {page_number} is short at \
                     {length} entries while the listing continues"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_default_page_size_is_the_documented_twenty_five() {
    let emulator = Emulator::new();
    // One certificate past the documented default, so the default shows as a page
    // boundary: "The results are paginated with a default page size of 25".
    let default_page_size = DEFAULT_CERTIFICATE_PAGE_SIZE as usize;
    let held = issue_certificates(&emulator, default_page_size + 1).await;

    for order in ORDERS {
        let label = order.label();
        let first = emulator.list(order, None, None).await;
        assert_eq!(
            first.status, 200,
            "case {label:?}: the listing must succeed, got {first:?}"
        );
        assert_eq!(
            first.reported_ids().len(),
            default_page_size,
            "case {label:?}: expected a default page of {default_page_size} certificates, \
             got {first:?}"
        );
        let marker = first.next_marker().unwrap_or_else(|| {
            panic!("case {label:?}: certificates remain, so a nextMarker is due, got {first:?}")
        });

        let second = emulator.list(order, None, Some(&marker)).await;
        assert_eq!(
            second.status, 200,
            "case {label:?}: the resumed listing must succeed, got {second:?}"
        );
        let mut walked = first.reported_ids();
        walked.extend(second.reported_ids());
        assert_eq!(
            walked,
            expected_ids(&held, order),
            "case {label:?}: the two pages must be the whole listing, got {second:?}"
        );
        assert!(
            second.next_marker().is_none(),
            "case {label:?}: nothing remains after the last certificate, got {second:?}"
        );
    }
}

#[tokio::test]
async fn a_marker_this_listing_did_not_issue_is_refused() {
    let emulator = Emulator::new();
    let held = issue_certificates(&emulator, 3).await;

    // A marker issued by the ASCENDING listing names a position in that listing's order,
    // which means something else in the descending one, so each direction refuses the
    // other's marker.
    let ascending = emulator.list(Order::Ascending, Some(1), None).await;
    let ascending_marker = ascending
        .next_marker()
        .unwrap_or_else(|| panic!("the first page of three leaves a nextMarker, got {ascending:?}"));
    let descending = emulator.list(Order::Descending, Some(1), None).await;
    let descending_marker = descending.next_marker().unwrap_or_else(|| {
        panic!("the first page of three leaves a nextMarker, got {descending:?}")
    });

    // Every marker below satisfies the modelled `Marker` (`length 0..1024, pattern
    // ^[A-Za-z0-9+/]+={0,2}$`, `smithy_query(query="operation",operation="ListCertificates")`),
    // so each one reaches the handler and is refused for what it NAMES rather than for
    // its shape.
    let cases: Vec<(&str, Order, String)> = vec![
        (
            "an ascending marker on the descending listing",
            Order::Descending,
            ascending_marker.clone(),
        ),
        (
            "an ascending marker on the defaulted listing",
            Order::Absent,
            ascending_marker.clone(),
        ),
        (
            "a descending marker on the ascending listing",
            Order::Ascending,
            descending_marker,
        ),
        ("not hex at all", Order::Ascending, "zzzz".to_string()),
        ("hex of nothing this listing issued", Order::Ascending, "deadbeef".to_string()),
        (
            "a marker of another resource's listing",
            Order::Ascending,
            hex::encode("things\u{1f}thing-one"),
        ),
    ];
    for (label, order, marker) in cases {
        let refused = emulator.list(order, Some(1), Some(&marker)).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: expected the marker to be refused 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case {label:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert!(
            refused.certificates().is_none(),
            "case {label:?}: a refusal carries no certificates, got {refused:?}"
        );
    }

    // The refusals wrote nothing: every certificate is still listed, in order.
    let page = emulator.list(Order::Ascending, None, None).await;
    assert_eq!(
        page.reported_ids(),
        expected_ids(&held, Order::Ascending),
        "the refused markers must leave the store untouched, got {page:?}"
    );
}

#[tokio::test]
async fn listing_certificates_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    let held = issue_certificates(&emulator, 3).await;

    // `ListCertificates` reports the store; it must not re-time, re-mint or re-order it,
    // so two identical listings are byte-identical and every record still reads back.
    for order in ORDERS {
        for page_size in [None, Some(1), Some(2)] {
            let label = order.label();
            let first = emulator.list(order, page_size, None).await;
            let second = emulator.list(order, page_size, None).await;
            assert_eq!(
                first.body, second.body,
                "case {label:?} pageSize={page_size:?}: a listing must not change what the \
                 next one sees, got {first:?} then {second:?}"
            );
        }
    }

    for issued in &held {
        let label = &issued.certificate_id;
        let read = emulator.describe(label).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: every listed certificate still reads back, got {read:?}"
        );
        assert_eq!(
            read.body
                .get("certificateDescription")
                .and_then(|description| description.get("creationDate"))
                .and_then(Value::as_f64),
            Some(issued.creation_date),
            "case {label:?}: a listing must not re-time the certificate, got {read:?}"
        );
    }
}

/// RULE: a listing reports a certificate's `certificateMode` only where the certificate
/// is in a mode a registration had to ask for — `SNI_ONLY`, the mode of a certificate
/// "registered without an issuer CA", whose devices "must send the SNI extension when
/// connecting" — and reports no mode at all for one in `DEFAULT` mode, whose devices
/// "aren't required to send" it and which is what a certificate IoT generated itself is
/// (`smithy_query(query="shape",shape="Certificate")`, member `certificateMode`). The
/// single-certificate reader is the other way round: its contract is the whole record, so
/// it reports the stored mode whichever one it is.
#[tokio::test]
async fn a_listing_reports_a_certificate_mode_only_where_it_is_not_the_default_mode() {
    let emulator = Emulator::new();
    let now = SystemTime::now();
    let generated = emulator.issue(true).await.certificate_id;

    // The rows span the modes a stored certificate can be in
    // (`smithy_query(query="shape",shape="CertificateMode")`: DEFAULT, SNI_ONLY), with the
    // default mode in both the forms it is reached by — a certificate this service
    // generated, and a record registered in that mode.
    // (case, certificateId, the stored mode, the mode a listing reports)
    let rows: [(&str, String, &str, Option<&str>); 3] = [
        (
            "a certificate this service generated",
            generated,
            CERTIFICATE_MODE_DEFAULT,
            None,
        ),
        (
            "a certificate registered in the default mode",
            emulator.record_certificate_in_mode(1, CERTIFICATE_MODE_DEFAULT, now),
            CERTIFICATE_MODE_DEFAULT,
            None,
        ),
        (
            "a certificate registered without an issuer CA",
            emulator.record_certificate_in_mode(2, CERTIFICATE_MODE_SNI_ONLY, now),
            CERTIFICATE_MODE_SNI_ONLY,
            Some(CERTIFICATE_MODE_SNI_ONLY),
        ),
    ];

    let page = emulator
        .list(Order::Ascending, Some(MAX_LIST_PAGE_SIZE), None)
        .await;
    assert_eq!(
        page.status, 200,
        "the listing must report every stored certificate, got {page:?}"
    );
    let entries = page
        .certificates()
        .unwrap_or_else(|| panic!("the listing must report a certificates list, got {page:?}"))
        .clone();

    for (case, certificate_id, stored_mode, listed_mode) in &rows {
        let entry = entries
            .iter()
            .find(|entry| {
                entry.get("certificateId").and_then(Value::as_str) == Some(certificate_id.as_str())
            })
            .unwrap_or_else(|| panic!("case {case:?}: the listing must report it, got {page:?}"));
        assert_eq!(
            entry.get("certificateMode").and_then(Value::as_str),
            *listed_mode,
            "case {case:?}: expected the listing to report certificateMode \
             {listed_mode:?}, got {entry:?}"
        );
        let read = emulator.describe(certificate_id).await;
        assert_eq!(
            read.body
                .get("certificateDescription")
                .and_then(|description| description.get("certificateMode"))
                .and_then(Value::as_str),
            Some(*stored_mode),
            "case {case:?}: expected the reader of one certificate to report the stored \
             mode {stored_mode:?}, got {read:?}"
        );
    }
}
