// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for this service's RESERVED DB INSTANCE slice: the offering catalogue
//! `DescribeReservedDBInstancesOfferings` publishes, the purchase
//! `PurchaseReservedDBInstancesOffering` persists, and the `DescribeReservedDBInstances`
//! report that reads it back.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it (through `db_instances::emulator`, the one mounting
//! helper this test target has), and each request is a real awsQuery form POST carrying the
//! SigV4 credential scope the router keys on. The assertions read the XML the emulator
//! answers with, so a handler that computed nothing could not pass them.
//!
//! The rules under test:
//!
//! * the offering catalogue is PUBLISHED: an unfiltered report answers offerings, each
//!   carrying the whole member set of the shape, a real offering id describes on its own,
//!   and every selection member narrows the report the way its own documentation states it;
//! * an identifier of either family that names nothing is that family's own modelled 404 —
//!   an offering id that was never issued for the two offering operations, a reservation id
//!   that was never purchased for the report of reservations — rather than an empty page;
//! * a purchase PERSISTS a reservation carrying the terms of the offering it was bought
//!   from, which every later read of that Region reports, under each selection member that
//!   matches it, and the account's reserved-instance usage counts what it reserves.
//!
//! The tables below span what those rules generalise over: each selection member of the
//! catalogue with a value drawn from the catalogue itself, both spellings of each published
//! term (years and seconds), both deployment shapes, each payment option, purchases of one
//! and of several DB instances, with and without a caller-chosen identifier, with and
//! without tags, both published bounds of `MaxRecords` and a page boundary with the marker
//! that resumes it, the Region scope, and the account allowance plus one purchase past it.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    OFFERING_TYPE_ALL_UPFRONT, OFFERING_TYPE_NO_UPFRONT, OFFERING_TYPE_PARTIAL_UPFRONT,
    RESERVED_CURRENCY_CODE, RESERVED_DB_INSTANCE_QUOTA_PER_REGION, RESERVED_TERM_ONE_YEAR,
    RESERVED_TERM_THREE_YEARS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents, which the ARNs below are read
/// against.
const ACCOUNT: &str = "000000000000";

/// A second region, so the regional scope of a purchased reservation is observable.
const OTHER_REGION: &str = "eu-west-1";

/// The state an accepted purchase reports while Amazon RDS takes the payment.
const PAYMENT_PENDING: &str = "payment-pending";

/// The state a paid reservation holds its capacity in.
const ACTIVE: &str = "active";

/// An offering id of the published form (`ReservedDBInstancesOfferingId`'s own example is
/// `438012d3-4052-4cc7-b2e3-8d3372e0e706`) that this service never issued.
const UNISSUED_OFFERING_ID: &str = "438012d3-4052-4cc7-b2e3-8d3372e0e706";

/// One offering, as the report answers it.
#[derive(Debug, Clone)]
struct Offering {
    id: String,
    class: String,
    duration: String,
    fixed_price: f64,
    usage_price: f64,
    currency_code: String,
    product_description: String,
    offering_type: String,
    multi_az: String,
    recurring_charge: f64,
}

/// Every `<ReservedDBInstancesOffering>` of a response body, in the order it was reported.
fn offerings(body: &str) -> Vec<Offering> {
    subtrees(body, "ReservedDBInstancesOffering")
        .into_iter()
        .map(|entry| Offering {
            id: text(&entry, "ReservedDBInstancesOfferingId"),
            class: text(&entry, "DBInstanceClass"),
            duration: text(&entry, "Duration"),
            fixed_price: number(&entry, "FixedPrice"),
            usage_price: number(&entry, "UsagePrice"),
            currency_code: text(&entry, "CurrencyCode"),
            product_description: text(&entry, "ProductDescription"),
            offering_type: text(&entry, "OfferingType"),
            multi_az: text(&entry, "MultiAZ"),
            recurring_charge: number(&entry, "RecurringChargeAmount"),
        })
        .collect()
}

/// One numeric member of a reported entry.
fn number(entry: &str, member: &str) -> f64 {
    let reported = text(entry, member);
    reported
        .parse()
        .unwrap_or_else(|_| panic!("member {member} must be a number, got {reported:?} in {entry}"))
}

/// `DescribeReservedDBInstancesOfferings` with the parameters named.
async fn describe_offerings(
    router: &MultiServiceRouter,
    params: &[(&str, String)],
) -> (u16, String) {
    call(router, "DescribeReservedDBInstancesOfferings", params).await
}

/// The offerings an unfiltered report answers, which is the first page of the catalogue.
async fn published(router: &MultiServiceRouter, case: &str) -> Vec<Offering> {
    let body = expect_ok(case, describe_offerings(router, &[]).await);
    offerings(&body)
}

/// `DescribeReservedDBInstances` for one reservation, as the HTTP status and the body.
///
/// A function rather than a closure body so the future it answers OWNS the parameters it was
/// built from and a poll loop can call it again.
async fn read_reservation(router: &MultiServiceRouter, named: &str) -> (u16, String) {
    call(
        router,
        "DescribeReservedDBInstances",
        &[("ReservedDBInstanceId", named)],
    )
    .await
}

#[tokio::test]
async fn the_published_offering_catalogue_is_reported_and_describes_by_id() {
    let router = emulator();

    let catalogue = published(&router, "the unfiltered catalogue").await;
    assert!(
        !catalogue.is_empty(),
        "an unfiltered report must answer at least one published offering"
    );

    for offering in &catalogue {
        let case = format!("{} {} {}", offering.class, offering.duration, offering.offering_type);
        // Every member of the shape is reported, and the identity members are the ones the
        // catalogue is published along.
        assert!(
            !offering.id.is_empty() && !offering.class.is_empty(),
            "case {case:?}: an offering must report its id and its class, got {offering:?}"
        );
        assert_eq!(
            offering.currency_code, RESERVED_CURRENCY_CODE,
            "case {case:?}: expected the published currency, got {:?}",
            offering.currency_code
        );
        let duration: i32 = offering
            .duration
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: Duration must be seconds, got {offering:?}"));
        assert!(
            duration == RESERVED_TERM_ONE_YEAR || duration == RESERVED_TERM_THREE_YEARS,
            "case {case:?}: expected a published term in seconds, got {duration}"
        );
        assert!(
            matches!(offering.multi_az.as_str(), "true" | "false"),
            "case {case:?}: MultiAZ must be reported as a boolean, got {:?}",
            offering.multi_az
        );
        // The reservation has already paid for the hours it reserves, so the rate charged
        // for USING them is nothing; the term's cost is the upfront charge and the hourly
        // recurring charge, split as the payment option says.
        assert_eq!(
            offering.usage_price, 0.0,
            "case {case:?}: expected a zero UsagePrice, got {offering:?}"
        );
        let paid_upfront = offering.fixed_price > 0.0;
        let charged_hourly = offering.recurring_charge > 0.0;
        let expected = match offering.offering_type.as_str() {
            OFFERING_TYPE_ALL_UPFRONT => (true, false),
            OFFERING_TYPE_PARTIAL_UPFRONT => (true, true),
            OFFERING_TYPE_NO_UPFRONT => (false, true),
            other => panic!("case {case:?}: unexpected OfferingType {other:?}"),
        };
        assert_eq!(
            (paid_upfront, charged_hourly),
            expected,
            "case {case:?}: expected (upfront, hourly) = {expected:?}, got {offering:?}"
        );
    }

    // A real offering id describes on its own, and answers the SAME offering the catalogue
    // published — the id a caller read is the id the next call resolves.
    for offering in catalogue.iter().take(3) {
        let case = format!("the offering {}", offering.id);
        let body = expect_ok(
            &case,
            describe_offerings(
                &router,
                &[("ReservedDBInstancesOfferingId", offering.id.clone())],
            )
            .await,
        );
        let reported = offerings(&body);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: exactly one offering must be reported, got {body}"
        );
        assert_eq!(
            reported[0].id, offering.id,
            "case {case:?}: expected the offering asked for, got {:?}",
            reported[0]
        );
        assert_eq!(
            (
                reported[0].class.clone(),
                reported[0].duration.clone(),
                reported[0].offering_type.clone(),
                reported[0].fixed_price
            ),
            (
                offering.class.clone(),
                offering.duration.clone(),
                offering.offering_type.clone(),
                offering.fixed_price
            ),
            "case {case:?}: an offering must report the same terms on every read, got {:?}",
            reported[0]
        );
    }
}

#[tokio::test]
async fn every_selection_member_narrows_the_offering_catalogue() {
    let router = emulator();
    let catalogue = published(&router, "the unfiltered catalogue").await;
    let sample = catalogue
        .first()
        .cloned()
        .expect("the catalogue publishes offerings");

    // Each value is drawn from the catalogue itself, so a row that reported nothing would
    // fail: the rows span every selection member the operation documents, both spellings of
    // a term, both deployment shapes and a product description that only PARTIALLY matches.
    let rows: Vec<SelectionRow> = vec![
        SelectionRow {
            case: "a DB instance class",
            params: vec![("DBInstanceClass", sample.class.clone())],
            holds: |reported, sample| reported.class == sample.class,
        },
        SelectionRow {
            case: "a term in seconds",
            params: vec![("Duration", RESERVED_TERM_THREE_YEARS.to_string())],
            holds: |reported, _| reported.duration == RESERVED_TERM_THREE_YEARS.to_string(),
        },
        SelectionRow {
            case: "the same term in years",
            params: vec![("Duration", "3".to_string())],
            holds: |reported, _| reported.duration == RESERVED_TERM_THREE_YEARS.to_string(),
        },
        SelectionRow {
            case: "the one-year term in years",
            params: vec![("Duration", "1".to_string())],
            holds: |reported, _| reported.duration == RESERVED_TERM_ONE_YEAR.to_string(),
        },
        SelectionRow {
            case: "an offering type",
            params: vec![("OfferingType", OFFERING_TYPE_NO_UPFRONT.to_string())],
            holds: |reported, _| reported.offering_type == OFFERING_TYPE_NO_UPFRONT,
        },
        SelectionRow {
            case: "Multi-AZ offerings only",
            params: vec![("MultiAZ", "true".to_string())],
            holds: |reported, _| reported.multi_az == "true",
        },
        SelectionRow {
            case: "Single-AZ offerings only",
            params: vec![("MultiAZ", "false".to_string())],
            holds: |reported, _| reported.multi_az == "false",
        },
        SelectionRow {
            case: "a product description",
            params: vec![("ProductDescription", sample.product_description.clone())],
            holds: |reported, sample| reported.product_description == sample.product_description,
        },
        SelectionRow {
            // "The results show offerings that partially match the filter value": the
            // leading characters of a published description select it.
            case: "a product description that only partially matches",
            params: vec![(
                "ProductDescription",
                sample.product_description.chars().take(3).collect(),
            )],
            holds: |reported, sample| {
                let prefix: String = sample.product_description.chars().take(3).collect();
                reported.product_description.contains(&prefix)
            },
        },
        SelectionRow {
            case: "a class and a term together",
            params: vec![
                ("DBInstanceClass", sample.class.clone()),
                ("Duration", RESERVED_TERM_ONE_YEAR.to_string()),
            ],
            holds: |reported, sample| {
                reported.class == sample.class
                    && reported.duration == RESERVED_TERM_ONE_YEAR.to_string()
            },
        },
    ];

    for row in rows {
        let case = row.case;
        let body = expect_ok(case, describe_offerings(&router, &row.params).await);
        let reported = offerings(&body);
        assert!(
            !reported.is_empty(),
            "case {case:?}: the catalogue publishes offerings for this selection, got {body}"
        );
        for offering in &reported {
            assert!(
                (row.holds)(offering, &sample),
                "case {case:?}: a reported offering must satisfy the selection, got {offering:?}"
            );
        }
    }

    // A selection the catalogue publishes nothing for is an EMPTY report rather than a
    // refusal: only an offering ID that was never issued is a fault.
    let body = expect_ok(
        "a class no offering is published for",
        describe_offerings(
            &router,
            &[("DBInstanceClass", "db.m5.24xlarge".to_string())],
        )
        .await,
    );
    assert!(
        offerings(&body).is_empty(),
        "a selection with no offering must answer an empty list, got {body}"
    );
}

#[tokio::test]
async fn an_identifier_that_names_nothing_is_that_familys_own_fault() {
    let router = emulator();

    let rows: Vec<NotFoundRow> = vec![
        NotFoundRow {
            case: "a report of an offering that was never issued",
            action: "DescribeReservedDBInstancesOfferings",
            params: vec![(
                "ReservedDBInstancesOfferingId",
                UNISSUED_OFFERING_ID.to_string(),
            )],
            code: "ReservedDBInstancesOfferingNotFound",
            message: format!("ReservedDBInstanceOffering {UNISSUED_OFFERING_ID} not found."),
        },
        NotFoundRow {
            case: "a purchase of an offering that was never issued",
            action: "PurchaseReservedDBInstancesOffering",
            params: vec![(
                "ReservedDBInstancesOfferingId",
                UNISSUED_OFFERING_ID.to_string(),
            )],
            code: "ReservedDBInstancesOfferingNotFound",
            message: format!("ReservedDBInstanceOffering {UNISSUED_OFFERING_ID} not found."),
        },
        NotFoundRow {
            case: "a purchase of an offering id of another form",
            action: "PurchaseReservedDBInstancesOffering",
            params: vec![(
                "ReservedDBInstancesOfferingId",
                "not-an-offering".to_string(),
            )],
            code: "ReservedDBInstancesOfferingNotFound",
            message: "ReservedDBInstanceOffering not-an-offering not found.".to_string(),
        },
        NotFoundRow {
            case: "a report of a reservation that was never purchased",
            action: "DescribeReservedDBInstances",
            params: vec![("ReservedDBInstanceId", "emu-never-purchased".to_string())],
            code: "ReservedDBInstanceNotFound",
            message: "ReservedDBInstance emu-never-purchased not found.".to_string(),
        },
    ];

    for row in rows {
        let case = row.case;
        let body = expect_error(case, call(&router, row.action, &row.params).await, 404, row.code);
        assert_eq!(
            text(&body, "Message"),
            row.message,
            "case {case:?}: expected {:?}, got {body}",
            row.message
        );
    }

    // With nothing purchased, the unfiltered report of reservations is a 200 with an empty
    // list — the report an empty result set is.
    let body = expect_ok(
        "the empty reservation report",
        call(&router, "DescribeReservedDBInstances", &[] as &[(&str, &str)]).await,
    );
    assert!(
        body.contains("<DescribeReservedDBInstancesResult>")
            && texts(&body, "ReservedDBInstanceId").is_empty(),
        "expected the empty success envelope, got {body}"
    );
}

/// One row of the selection table: the parameters the request carries, and what every
/// offering the report answers must satisfy.
struct SelectionRow {
    case: &'static str,
    params: Vec<(&'static str, String)>,
    /// Asked about each reported offering, beside the offering the row's values were drawn
    /// from.
    holds: fn(&Offering, &Offering) -> bool,
}

/// One row of the not-found table: the call, and the fault it must answer.
struct NotFoundRow {
    case: &'static str,
    action: &'static str,
    params: Vec<(&'static str, String)>,
    code: &'static str,
    message: String,
}

/// One row of the purchase round trip.
struct PurchaseCase {
    case: &'static str,
    /// The `ReservedDBInstanceId` the purchase names, or `None` to let this service mint one.
    reservation_id: Option<&'static str>,
    /// `DBInstanceCount`, or `None` for the documented default of 1.
    count: Option<i32>,
    /// The tags the purchase carries.
    tags: &'static [(&'static str, &'static str)],
    /// Which published offering it buys, as the offset into the catalogue.
    offering: usize,
}

const PURCHASES: &[PurchaseCase] = &[
    PurchaseCase {
        case: "a caller-chosen identifier, the default count and no tags",
        reservation_id: Some("emu-reservation-one"),
        count: None,
        tags: &[],
        offering: 0,
    },
    PurchaseCase {
        case: "several DB instances under a caller-chosen identifier, with two tags",
        reservation_id: Some("emu-reservation-many"),
        count: Some(3),
        tags: &[("team", "platform"), ("env", "test")],
        offering: 1,
    },
    PurchaseCase {
        case: "no identifier at all, so this service mints one",
        reservation_id: None,
        count: Some(2),
        tags: &[("team", "data")],
        offering: 2,
    },
];

#[tokio::test]
async fn a_purchase_persists_a_reservation_every_later_read_reports() {
    let router = emulator();
    let catalogue = published(&router, "the unfiltered catalogue").await;

    for row in PURCHASES {
        let offering = catalogue
            .get(row.offering)
            .unwrap_or_else(|| panic!("case {:?}: the catalogue publishes offerings", row.case));
        let mut params: Vec<(&str, String)> = vec![(
            "ReservedDBInstancesOfferingId",
            offering.id.clone(),
        )];
        if let Some(identifier) = row.reservation_id {
            params.push(("ReservedDBInstanceId", identifier.to_string()));
        }
        if let Some(count) = row.count {
            params.push(("DBInstanceCount", count.to_string()));
        }
        for (position, (key, value)) in row.tags.iter().enumerate() {
            params.push((
                match position {
                    0 => "Tags.Tag.1.Key",
                    _ => "Tags.Tag.2.Key",
                },
                (*key).to_string(),
            ));
            params.push((
                match position {
                    0 => "Tags.Tag.1.Value",
                    _ => "Tags.Tag.2.Value",
                },
                (*value).to_string(),
            ));
        }

        let purchased = expect_ok(
            row.case,
            call(&router, "PurchaseReservedDBInstancesOffering", &params).await,
        );
        let count = row.count.unwrap_or(1);
        let reservation_id = text(&purchased, "ReservedDBInstanceId");
        assert!(
            !reservation_id.is_empty(),
            "case {:?}: a purchase must report the identifier the reservation is tracked by, \
             got {purchased}",
            row.case
        );
        if let Some(named) = row.reservation_id {
            assert_eq!(
                reservation_id, named,
                "case {:?}: expected the identifier the purchase named, got {reservation_id:?}",
                row.case
            );
        }
        // The reservation's terms are the OFFERING's, with the upfront charge taken per
        // reserved DB instance.
        assert_eq!(
            text(&purchased, "ReservedDBInstancesOfferingId"),
            offering.id,
            "case {:?}: expected the offering it was bought from, got {purchased}",
            row.case
        );
        for (member, expected) in [
            ("DBInstanceClass", offering.class.clone()),
            ("Duration", offering.duration.clone()),
            ("CurrencyCode", offering.currency_code.clone()),
            ("ProductDescription", offering.product_description.clone()),
            ("OfferingType", offering.offering_type.clone()),
            ("MultiAZ", offering.multi_az.clone()),
            ("DBInstanceCount", count.to_string()),
            ("State", PAYMENT_PENDING.to_string()),
        ] {
            let reported = text(&purchased, member);
            assert_eq!(
                reported, expected,
                "case {:?}: expected {member} {expected:?}, got {reported:?} in {purchased}",
                row.case
            );
        }
        let charged = number(&purchased, "FixedPrice");
        let expected_charge = offering.fixed_price * f64::from(count);
        assert!(
            (charged - expected_charge).abs() < 0.01,
            "case {:?}: expected the offering's fixed price per reserved instance \
             ({expected_charge}), got {charged}",
            row.case
        );
        assert_eq!(
            number(&purchased, "RecurringChargeAmount"),
            offering.recurring_charge,
            "case {:?}: expected the offering's recurring charge, got {purchased}",
            row.case
        );
        assert_eq!(
            text(&purchased, "ReservedDBInstanceArn"),
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:ri:{reservation_id}"),
            "case {:?}: expected the published ARN form, got {purchased}",
            row.case
        );
        assert!(
            !text(&purchased, "LeaseId").is_empty(),
            "case {:?}: a reservation must report the lease it is held under, got {purchased}",
            row.case
        );
        assert!(
            optional_text(&purchased, "StartTime").is_some(),
            "case {:?}: a reservation must report when it started, got {purchased}",
            row.case
        );

        // The ROUND TRIP: the reservation the purchase persisted is what the report answers,
        // and the state the clock owes it settles without any mutating call of this test's.
        let settled = poll_until(
            row.case,
            "the reservation reaches the state its purchase owed it",
            || read_reservation(&router, &reservation_id),
            |status, body| status == 200 && text(body, "State") == ACTIVE,
        )
        .await
        .0;
        assert_eq!(
            text(&settled, "ReservedDBInstancesOfferingId"),
            offering.id,
            "case {:?}: the report must answer the reservation the purchase stored, got {settled}",
            row.case
        );
        let lease = text(&settled, "LeaseId");

        // And under every selection member that matches it.
        let selections: Vec<(&str, Vec<(&str, String)>)> = vec![
            (
                "by offering id",
                vec![("ReservedDBInstancesOfferingId", offering.id.clone())],
            ),
            (
                "by DB instance class",
                vec![("DBInstanceClass", offering.class.clone())],
            ),
            (
                "by offering type",
                vec![("OfferingType", offering.offering_type.clone())],
            ),
            ("by lease id", vec![("LeaseId", lease.clone())]),
            (
                "by term in years",
                vec![(
                    "Duration",
                    if offering.duration == RESERVED_TERM_THREE_YEARS.to_string() {
                        "3".to_string()
                    } else {
                        "1".to_string()
                    },
                )],
            ),
        ];
        for (selection, params) in selections {
            let body = expect_ok(
                &format!("{} {selection}", row.case),
                call(&router, "DescribeReservedDBInstances", &params).await,
            );
            assert!(
                texts(&body, "ReservedDBInstanceId").contains(&reservation_id),
                "case {:?}: the reservation must be reported {selection}, got {body}",
                row.case
            );
        }

        // A second purchase under the same identifier is the modelled already-exists fault
        // rather than a silent overwrite.
        if let Some(named) = row.reservation_id {
            let refused = expect_error(
                row.case,
                call(
                    &router,
                    "PurchaseReservedDBInstancesOffering",
                    &[
                        ("ReservedDBInstancesOfferingId", offering.id.clone()),
                        ("ReservedDBInstanceId", named.to_string()),
                    ],
                )
                .await,
                404,
                "ReservedDBInstanceAlreadyExists",
            );
            assert_eq!(
                text(&refused, "Message"),
                format!("ReservedDBInstance {named} already exists."),
                "case {:?}: expected the already-exists sentence naming it, got {refused}",
                row.case
            );
        }
    }

    // The account's reserved-instance usage counts the DB instances the purchases reserve:
    // 1 + 3 + 2.
    let attributes = expect_ok(
        "the account attributes",
        call(&router, "DescribeAccountAttributes", &[] as &[(&str, &str)]).await,
    );
    let reserved = subtrees(&attributes, "AccountQuota")
        .into_iter()
        .find(|quota| text(quota, "AccountQuotaName") == "ReservedDBInstances")
        .expect("the account publishes a ReservedDBInstances quota");
    assert_eq!(
        text(&reserved, "Used"),
        "6",
        "the reserved-instance usage must count what the purchases reserve, got {reserved}"
    );
    assert_eq!(
        text(&reserved, "Max"),
        RESERVED_DB_INSTANCE_QUOTA_PER_REGION.to_string(),
        "the published allowance must be the one a purchase is refused past, got {reserved}"
    );

    // RDS is regional: a reservation purchased in one Region is reported in that Region and
    // in no other.
    let elsewhere = expect_ok(
        "another region",
        call_in(
            &router,
            OTHER_REGION,
            "DescribeReservedDBInstances",
            &[] as &[(&str, &str)],
        )
        .await,
    );
    assert!(
        texts(&elsewhere, "ReservedDBInstanceId").is_empty(),
        "case {:?}: another region must report none of them, got {elsewhere}",
        "the region scope"
    );
}

#[tokio::test]
async fn a_purchase_past_the_account_allowance_is_the_quota_fault() {
    let router = emulator();
    let catalogue = published(&router, "the unfiltered catalogue").await;
    let offering = catalogue
        .first()
        .cloned()
        .expect("the catalogue publishes offerings");

    // The allowance itself is served: a purchase that reserves exactly it is accepted.
    expect_ok(
        "a purchase of the whole allowance",
        call(
            &router,
            "PurchaseReservedDBInstancesOffering",
            &[
                ("ReservedDBInstancesOfferingId", offering.id.clone()),
                ("ReservedDBInstanceId", "emu-reservation-full".to_string()),
                (
                    "DBInstanceCount",
                    RESERVED_DB_INSTANCE_QUOTA_PER_REGION.to_string(),
                ),
            ],
        )
        .await,
    );
    // One DB instance past it is the modelled quota fault, and nothing is stored for it.
    expect_error(
        "one DB instance past the allowance",
        call(
            &router,
            "PurchaseReservedDBInstancesOffering",
            &[
                ("ReservedDBInstancesOfferingId", offering.id.clone()),
                ("ReservedDBInstanceId", "emu-reservation-over".to_string()),
                ("DBInstanceCount", "1".to_string()),
            ],
        )
        .await,
        400,
        "ReservedDBInstanceQuotaExceeded",
    );
    expect_error(
        "the refused reservation stored nothing",
        call(
            &router,
            "DescribeReservedDBInstances",
            &[("ReservedDBInstanceId", "emu-reservation-over")],
        )
        .await,
        404,
        "ReservedDBInstanceNotFound",
    );
    // A count that reserves nothing is refused for the value it carries.
    expect_error(
        "a count below one",
        call(
            &router,
            "PurchaseReservedDBInstancesOffering",
            &[
                ("ReservedDBInstancesOfferingId", offering.id.clone()),
                ("DBInstanceCount", "0".to_string()),
            ],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
}

#[tokio::test]
async fn the_offering_catalogue_pages_at_the_published_bounds() {
    let router = emulator();

    // "Constraints: Minimum 20, maximum 100" — both published bounds are served, and a value
    // one past each is refused.
    for (case, max_records, expected) in [
        ("the published minimum", 20usize, 20usize),
        ("the published maximum", 100, 100),
    ] {
        let body = expect_ok(
            case,
            describe_offerings(&router, &[("MaxRecords", max_records.to_string())]).await,
        );
        let page = offerings(&body);
        assert_eq!(
            page.len(),
            expected,
            "case {case:?}: expected a page of {expected}, got {}",
            page.len()
        );
        // A marker is handed out while records remain, and the page it resumes carries none
        // of the records this one served.
        let marker = optional_text(&body, "Marker")
            .unwrap_or_else(|| panic!("case {case:?}: expected a marker, got {body}"));
        let next = expect_ok(
            case,
            describe_offerings(
                &router,
                &[
                    ("MaxRecords", max_records.to_string()),
                    ("Marker", marker),
                ],
            )
            .await,
        );
        let resumed = offerings(&next);
        assert!(
            !resumed.is_empty(),
            "case {case:?}: the resumed page must carry records, got {next}"
        );
        let served: Vec<String> = page.iter().map(|offering| offering.id.clone()).collect();
        for offering in &resumed {
            assert!(
                !served.contains(&offering.id),
                "case {case:?}: the resumed page must not repeat an offering, got {:?}",
                offering.id
            );
        }
    }

    for (case, max_records) in [
        ("one below the published minimum", "19"),
        ("one above the published maximum", "101"),
    ] {
        expect_error(
            case,
            describe_offerings(&router, &[("MaxRecords", max_records.to_string())]).await,
            400,
            "InvalidParameterValue",
        );
    }

    // A `Marker` this front door's codec could not have produced is refused rather than
    // resynchronised to the first page.
    expect_error(
        "a marker that is not a token",
        describe_offerings(&router, &[("Marker", "not-a-token".to_string())]).await,
        400,
        "InvalidParameterValue",
    );
}
