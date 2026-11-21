// SPDX-License-Identifier: CC0-1.0

//! `GetBenefitAllocation`, end to end over the wire.
//!
//! Every assertion goes through the real request path — the shared
//! [`crate::benefit_application_lifecycle::Harness`], i.e. `MultiServiceRouter::dispatch`
//! with `awsJson1_0` bodies, `POST /` and `X-Amz-Target:
//! PartnerCentralBenefitsService.<Operation>` — so what it pins is the emulator's HTTP
//! behaviour, not an internal helper.
//!
//! The rules under test, each stated over a CLASS of inputs and checked as a table whose
//! rows differ along what the rule generalises over:
//!
//! 1. **The report is the store**: the operation reports the one benefit allocation the
//!    request names, member for member from the stored record, and the ARN it reports is
//!    the allocation ARN of the region the request was signed for. Rows: every stored
//!    allocation of every seeded catalog, plus an allocation AWS granted for an
//!    application.
//! 2. **Either identifier form names the same record**: the bare `benalloc-...` id and an
//!    allocation ARN of that catalog are answered identically. Rows: every stored
//!    allocation of every seeded catalog, in both forms.
//! 3. **`FulfillmentDetail` is the member of the modelled `FulfillmentDetails` union the
//!    allocation's `FulfillmentType` names** — `CASH` is disbursed, `CREDITS` is issued as
//!    credit codes, `ACCESS` grants a privilege — and every monetary amount it carries is
//!    the stored one, in a modelled `CurrencyCode`. Rows: every `FulfillmentType` the model
//!    declares, an allocation partly issued, one issued in full, and one with nothing
//!    issued yet.
//! 4. **An identifier that names no allocation of the catalog is refused**
//!    `ResourceNotFoundException` (404) with the service's own lookup message. Rows: a
//!    well-formed id no grant ever minted, an allocation ARN of the other catalog, an ARN
//!    of another resource type of the same catalog (a benefit, an application), and an ARN
//!    of another service.
//! 5. **A catalog the service does not serve is refused** `ResourceNotFoundException` (404)
//!    with the catalog-lookup message, whatever the identifier. Rows: two catalogs the
//!    service does not serve, each with a stored id and with an unknown one.
//! 6. **Reading changes nothing a later caller sees, except materialising an expiry the
//!    clock already made due**: an allocation past its `ExpiresAt` is never reported
//!    `ACTIVE` and a credit code past its own is never reported redeemable, while two reads
//!    of any allocation are identical.
//! 7. **A change the clock made due is dated by the instant it FELL DUE**, the `ExpiresAt`
//!    the record holds and the latest one where a call materialises several, so `UpdatedAt`
//!    follows the stored state and not the instant of the read. Rows: every
//!    `FulfillmentType`, each status an allocation can be placed in, an expiry owed by the
//!    allocation, one owed by an issued credit code, both owed in either order, a term that
//!    closed one millisecond ago and terms still open; and the same over the sibling
//!    `ListBenefitAllocations`, which materialises the same expiries.
//! 8. **Whether a term has closed at an instant is ONE boundary**: the allocation's term
//!    and an issued credit code's term are judged by the same rule, each against its own
//!    `ExpiresAt`. Rows: an instant on either side of a term's end and at the end itself,
//!    and, over the wire, the two terms placed at different instants so that each of one
//!    closed and one open is reported on its own end — through this operation and through
//!    the sibling `ListBenefitAllocations`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::ops::types::{
    BENEFIT_ALLOCATION_STATUS_VALUES, CURRENCY_CODE_VALUES, FULFILLMENT_TYPE_VALUES,
};
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::state::{
    not_found_message, now_millis, service_state, term_closed, AllocatedCreditCode,
    AllocationFulfillment, BenefitAllocation, Issuance, Money, ALLOCATION_EXPIRED_REASON,
    ALLOCATION_RESOURCE,
    ALLOCATION_STATUS_ACTIVE, ALLOCATION_STATUS_FULFILLED, ALLOCATION_STATUS_INACTIVE,
    ALLOCATION_KIND, APPLICATION_RESOURCE, BENEFIT_RESOURCE, CATALOG_KIND,
    FULFILLMENT_TYPE_ACCESS, FULFILLMENT_TYPE_CASH, FULFILLMENT_TYPE_CREDITS, SEEDED_CATALOGS,
};

use crate::benefit_allocations::{granted, list, stored_allocations};
use crate::benefit_application_lifecycle::{error_code, instant, Harness, CATALOG};
use crate::resource_tagging::{application_arn, benefit_id, error_message, expected_arn};

/// The other catalog the service serves, so a row can name an ARN of a catalog that is not
/// the one the request asked about.
const OTHER_CATALOG: &str = "Sandbox";

/// An allocation id the modelled `BenefitAllocationIdentifier` admits and no grant ever
/// minted, so it names nothing.
const UNKNOWN_ALLOCATION: &str = "benalloc-00000000000000";

/// Which member of the modelled `FulfillmentDetails` union each `FulfillmentType` is
/// fulfilled through.
///
/// From the union's own member docs (`smithy_query(query="shape",shape="FulfillmentDetails"
/// )`): `DisbursementDetails` is "direct payments or reimbursements", `CreditDetails` is
/// "credit-based benefit fulfillment, including AWS promotional credits" and
/// `AccessDetails` is "service permissions or feature access". The union's fourth member,
/// `ConsumableDetails`, is produced by no `FulfillmentType` the model declares.
const DETAIL_MEMBER_BY_TYPE: &[(&str, &str)] = &[
    (FULFILLMENT_TYPE_CASH, "DisbursementDetails"),
    (FULFILLMENT_TYPE_CREDITS, "CreditDetails"),
    (FULFILLMENT_TYPE_ACCESS, "AccessDetails"),
];

/// One `GetBenefitAllocation` call that must answer 200.
pub(crate) async fn get(harness: &Harness, case: &str, catalog: &str, identifier: &str) -> Value {
    harness
        .ok(
            case,
            "GetBenefitAllocation",
            &json!({ "Catalog": catalog, "Identifier": identifier }),
        )
        .await
}

/// One `GetBenefitAllocation` call that must be refused with `ResourceNotFoundException`
/// (404) carrying `expected_message`.
async fn refused(
    harness: &Harness,
    case: &str,
    catalog: &str,
    identifier: &str,
    expected_message: &str,
) {
    let (status, out) = harness
        .call(
            "GetBenefitAllocation",
            &json!({ "Catalog": catalog, "Identifier": identifier }),
        )
        .await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected 404, got {status} with {out:?}"
    );
    assert_eq!(
        error_code(&out),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {out:?}"
    );
    assert_eq!(
        error_message(&out),
        expected_message,
        "case {case:?}: expected the service's lookup message, got {out:?}"
    );
}

/// A string member of a response, or `None` when it omits it.
fn member(body: &Value, name: &str) -> Option<String> {
    body.get(name).and_then(Value::as_str).map(str::to_string)
}

/// The entries of a list member, or an empty list when the response omits it.
fn members(body: &Value, name: &str) -> Vec<String> {
    body.get(name)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Assert one reported `MonetaryValue` is the stored amount: the amount text the store
/// renders, and a currency the model declares.
fn assert_money(case: &str, label: &str, reported: &Value, expected: &Money) {
    let amount = member(reported, "Amount");
    assert_eq!(
        amount.as_deref(),
        Some(expected.amount_text().as_str()),
        "case {case:?}: expected {label} Amount {:?}, got {reported:?}",
        expected.amount_text()
    );
    let currency = member(reported, "CurrencyCode");
    assert_eq!(
        currency.as_deref(),
        Some(expected.currency_code.as_str()),
        "case {case:?}: expected {label} CurrencyCode {:?}, got {reported:?}",
        expected.currency_code
    );
    let currency = currency.unwrap_or_default();
    assert!(
        CURRENCY_CODE_VALUES.contains(&currency.as_str()),
        "case {case:?}: {label} CurrencyCode {currency:?} is not a modelled CurrencyCode"
    );
    // A two-decimal amount, which is what a minor unit of this currency renders as.
    let amount = amount.unwrap_or_default();
    let (units, cents) = amount
        .split_once('.')
        .unwrap_or_else(|| panic!("case {case:?}: {label} Amount {amount:?} carries no decimals"));
    assert!(
        !units.is_empty()
            && units.bytes().all(|b| b.is_ascii_digit())
            && cents.len() == 2
            && cents.bytes().all(|b| b.is_ascii_digit()),
        "case {case:?}: expected {label} Amount as digits with two decimals, got {amount:?}"
    );
}

/// Assert one reported `IssuanceDetail` is the stored issuance.
fn assert_issuance(case: &str, reported: &Value, expected: &Issuance) {
    assert_eq!(
        member(reported, "IssuanceId").as_deref(),
        Some(expected.id.as_str()),
        "case {case:?}: expected the stored IssuanceId, got {reported:?}"
    );
    assert_money(
        case,
        "IssuanceAmount",
        &reported["IssuanceAmount"],
        &expected.amount,
    );
    assert_eq!(
        instant(case, reported, "IssuedAt"),
        expected.issued_at_millis,
        "case {case:?}: expected the stored IssuedAt, got {reported:?}"
    );
}

/// Assert one reported `CreditCode` is the stored credit code.
fn assert_credit_code(case: &str, reported: &Value, expected: &AllocatedCreditCode) {
    assert_eq!(
        member(reported, "AwsAccountId").as_deref(),
        Some(expected.aws_account_id.as_str()),
        "case {case:?}: expected the stored AwsAccountId, got {reported:?}"
    );
    assert_eq!(
        member(reported, "AwsCreditCode").as_deref(),
        Some(expected.code.as_str()),
        "case {case:?}: expected the stored AwsCreditCode, got {reported:?}"
    );
    assert_money(case, "CreditCode Value", &reported["Value"], &expected.value);
    let status = member(reported, "Status").unwrap_or_default();
    assert_eq!(
        status, expected.status,
        "case {case:?}: expected the stored credit-code Status, got {reported:?}"
    );
    assert!(
        BENEFIT_ALLOCATION_STATUS_VALUES.contains(&status.as_str()),
        "case {case:?}: credit-code Status {status:?} is not a modelled BenefitAllocationStatus"
    );
    assert_eq!(
        instant(case, reported, "IssuedAt"),
        expected.issued_at_millis,
        "case {case:?}: expected the stored credit-code IssuedAt, got {reported:?}"
    );
    assert_eq!(
        instant(case, reported, "ExpiresAt"),
        expected.expires_at_millis,
        "case {case:?}: expected the stored credit-code ExpiresAt, got {reported:?}"
    );
}

/// Assert the reported `FulfillmentDetail` is the union member the allocation's stored
/// fulfillment record names, carrying that record's own values — and that it carries
/// exactly ONE member, since the modelled `FulfillmentDetails` is a union.
fn assert_fulfillment_detail(case: &str, body: &Value, allocation: &BenefitAllocation) {
    let reported = &body["FulfillmentDetail"];
    let Some(fulfillment) = allocation.fulfillment.as_ref() else {
        assert!(
            reported.is_null(),
            "case {case:?}: an allocation with no stored fulfillment record reports no \
             FulfillmentDetail, got {reported:?}"
        );
        return;
    };
    let expected_member = DETAIL_MEMBER_BY_TYPE
        .iter()
        .find(|(fulfillment_type, _)| *fulfillment_type == allocation.fulfillment_type)
        .map(|(_, detail_member)| *detail_member)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: {:?} names no FulfillmentDetails member",
                allocation.fulfillment_type
            )
        });
    let object = reported
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FulfillmentDetail, got {reported:?}"));
    assert_eq!(
        object.keys().collect::<Vec<&String>>(),
        vec![expected_member],
        "case {case:?}: expected exactly the union member {expected_member:?}, got {reported:?}"
    );

    let detail = &reported[expected_member];
    match fulfillment {
        AllocationFulfillment::Disbursement {
            disbursed,
            issuance,
        } => {
            assert_money(case, "DisbursedAmount", &detail["DisbursedAmount"], disbursed);
            match issuance {
                Some(issuance) => assert_issuance(case, &detail["IssuanceDetails"], issuance),
                None => assert!(
                    detail["IssuanceDetails"].is_null(),
                    "case {case:?}: nothing disbursed yet reports no IssuanceDetails, got {detail:?}"
                ),
            }
        }
        AllocationFulfillment::Credits {
            allocated,
            issued,
            codes,
        } => {
            assert_money(case, "AllocatedAmount", &detail["AllocatedAmount"], allocated);
            assert_money(case, "IssuedAmount", &detail["IssuedAmount"], issued);
            let reported_codes = detail["Codes"].as_array().unwrap_or_else(|| {
                panic!("case {case:?}: CreditDetails$Codes is @required, got {detail:?}")
            });
            assert_eq!(
                reported_codes.len(),
                codes.len(),
                "case {case:?}: expected {} credit codes, got {detail:?}",
                codes.len()
            );
            for (index, (reported_code, expected_code)) in
                reported_codes.iter().zip(codes).enumerate()
            {
                assert_credit_code(&format!("{case}/code {index}"), reported_code, expected_code);
            }
        }
        AllocationFulfillment::Access => assert_eq!(
            member(detail, "Description"),
            allocation.description.clone(),
            "case {case:?}: expected the allocation's own Description, got {detail:?}"
        ),
    }
}

/// Assert one reported allocation is the stored record, member for member.
fn assert_reports_the_store(case: &str, body: &Value, allocation: &BenefitAllocation) {
    assert_eq!(
        member(body, "Id").as_deref(),
        Some(allocation.id.as_str()),
        "case {case:?}: expected the stored Id, got {body:?}"
    );
    assert_eq!(
        member(body, "Catalog").as_deref(),
        Some(allocation.catalog.as_str()),
        "case {case:?}: expected the stored Catalog, got {body:?}"
    );
    assert_eq!(
        member(body, "Arn"),
        Some(expected_arn(
            &allocation.catalog,
            ALLOCATION_RESOURCE,
            &allocation.id
        )),
        "case {case:?}: expected the allocation ARN of the signed region, got {body:?}"
    );
    assert_eq!(
        member(body, "Name").as_deref(),
        Some(allocation.name.as_str()),
        "case {case:?}: expected the stored Name, got {body:?}"
    );
    assert_eq!(
        member(body, "Description"),
        allocation.description.clone(),
        "case {case:?}: expected the stored Description, got {body:?}"
    );
    let status = member(body, "Status").unwrap_or_default();
    assert_eq!(
        status, allocation.status,
        "case {case:?}: expected the stored Status, got {body:?}"
    );
    assert!(
        BENEFIT_ALLOCATION_STATUS_VALUES.contains(&status.as_str()),
        "case {case:?}: Status {status:?} is not a modelled BenefitAllocationStatus"
    );
    assert_eq!(
        member(body, "StatusReason"),
        allocation.status_reason.clone(),
        "case {case:?}: expected the stored StatusReason, got {body:?}"
    );
    assert_eq!(
        member(body, "BenefitId").as_deref(),
        Some(allocation.benefit_id.as_str()),
        "case {case:?}: expected the stored BenefitId, got {body:?}"
    );
    assert_eq!(
        member(body, "BenefitApplicationId"),
        allocation.benefit_application_id.clone(),
        "case {case:?}: expected the stored BenefitApplicationId, got {body:?}"
    );
    let fulfillment_type = member(body, "FulfillmentType").unwrap_or_default();
    assert_eq!(
        fulfillment_type, allocation.fulfillment_type,
        "case {case:?}: expected the stored FulfillmentType, got {body:?}"
    );
    assert!(
        FULFILLMENT_TYPE_VALUES.contains(&fulfillment_type.as_str()),
        "case {case:?}: FulfillmentType {fulfillment_type:?} is not a modelled FulfillmentType"
    );
    assert_eq!(
        allocation.fulfillment_types(),
        std::slice::from_ref(&fulfillment_type),
        "case {case:?}: the types used for an allocation are the one it is fulfilled with, \
         got {:?}",
        allocation.fulfillment_types()
    );
    assert_eq!(
        members(body, "ApplicableBenefitIds"),
        allocation.applicable_benefit_ids,
        "case {case:?}: expected the stored ApplicableBenefitIds, got {body:?}"
    );
    for (reported, stored) in [
        ("CreatedAt", allocation.created_at_millis),
        ("UpdatedAt", allocation.updated_at_millis),
        ("StartsAt", allocation.starts_at_millis),
        ("ExpiresAt", allocation.expires_at_millis),
    ] {
        assert_eq!(
            instant(case, body, reported),
            stored,
            "case {case:?}: expected the stored {reported}, got {body:?}"
        );
    }
    assert_fulfillment_detail(case, body, allocation);
}

/// Grant one allocation of `CATALOG` as the outcome of a real benefit application, so the
/// tables below cover a record AWS's fulfillment workflow produced as well as the seeded
/// ones. Returns its id.
async fn granted_for_an_application(harness: &Harness, token: &str) -> String {
    let application = application_arn(harness, token, CATALOG, token, &[]).await;
    let benefit = benefit_id(harness, token, CATALOG).await;
    granted(harness, CATALOG, &benefit, Some(&application), token)
}

/// Rule 1: every stored allocation of every seeded catalog, and one AWS granted for an
/// application, is reported member for member from the record.
#[tokio::test]
async fn reports_the_stored_allocation_member_for_member_in_every_catalog() {
    let harness = Harness::new();
    let granted_id = granted_for_an_application(&harness, "tok-get-allocation").await;
    let mut seen = 0usize;

    for catalog in SEEDED_CATALOGS {
        let ids: Vec<String> = stored_allocations(&harness, catalog)
            .into_iter()
            .map(|allocation| allocation.id)
            .collect();
        assert!(
            !ids.is_empty(),
            "case {catalog:?}: the seeded catalog must hold allocations to report"
        );
        // Every read is taken first, so the store is then compared AFTER the expiries rule
        // 6 covers have been materialised and the two sides compare like for like.
        let mut bodies: Vec<(String, Value)> = Vec::new();
        for id in &ids {
            let case = format!("{catalog}/{id}");
            bodies.push((id.clone(), get(&harness, &case, catalog, id).await));
        }
        let stored = stored_allocations(&harness, catalog);
        for (id, body) in &bodies {
            let case = format!("{catalog}/{id}");
            let allocation = stored
                .iter()
                .find(|allocation| &allocation.id == id)
                .unwrap_or_else(|| panic!("case {case:?}: the store lost the allocation"));
            assert_reports_the_store(&case, body, allocation);
            seen += 1;
        }
        if *catalog == CATALOG {
            assert!(
                ids.contains(&granted_id),
                "case {catalog:?}: the granted allocation must be among the stored ones"
            );
        }
    }

    assert!(
        seen >= SEEDED_CATALOGS.len() * 4,
        "the table must cover every seeded allocation of both catalogs, covered {seen}"
    );
}

/// Rule 2: the bare id and the allocation ARN of the catalog are answered identically.
#[tokio::test]
async fn either_identifier_form_names_the_same_allocation() {
    let harness = Harness::new();
    granted_for_an_application(&harness, "tok-get-allocation-forms").await;

    for catalog in SEEDED_CATALOGS {
        for allocation in stored_allocations(&harness, catalog) {
            let case = format!("{catalog}/{}", allocation.id);
            let by_id = get(&harness, &case, catalog, &allocation.id).await;
            let arn = expected_arn(catalog, ALLOCATION_RESOURCE, &allocation.id);
            let by_arn = get(&harness, &case, catalog, &arn).await;
            assert_eq!(
                by_arn, by_id,
                "case {case:?}: the ARN form must be answered exactly as the id form, \
                 got {by_arn:?} and {by_id:?}"
            );
        }
    }
}

/// Rule 3: the `FulfillmentDetail` member is the one the allocation's `FulfillmentType`
/// names, and the table covers every `FulfillmentType` the model declares.
#[tokio::test]
async fn the_fulfillment_detail_is_the_union_member_the_fulfillment_type_names() {
    let harness = Harness::new();
    granted_for_an_application(&harness, "tok-get-allocation-detail").await;
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut issued_states: BTreeSet<bool> = BTreeSet::new();

    for catalog in SEEDED_CATALOGS {
        for allocation in stored_allocations(&harness, catalog) {
            let case = format!("{catalog}/{}", allocation.fulfillment_type);
            let body = get(&harness, &case, catalog, &allocation.id).await;
            // `assert_reports_the_store` asserts the same mapping over the whole record;
            // here it is the rule under test, over the store read after the call.
            let stored = stored_allocations(&harness, catalog);
            let allocation = stored
                .iter()
                .find(|stored| stored.id == allocation.id)
                .unwrap_or_else(|| panic!("case {case:?}: the store lost the allocation"));
            assert_fulfillment_detail(&case, &body, allocation);
            covered.insert(allocation.fulfillment_type.clone());
            issued_states.insert(match &allocation.fulfillment {
                Some(AllocationFulfillment::Disbursement { issuance, .. }) => issuance.is_some(),
                Some(AllocationFulfillment::Credits { codes, .. }) => !codes.is_empty(),
                _ => false,
            });
        }
    }

    for fulfillment_type in FULFILLMENT_TYPE_VALUES {
        assert!(
            covered.contains(*fulfillment_type),
            "case {fulfillment_type:?}: the table must cover every modelled FulfillmentType, \
             covered {covered:?}"
        );
    }
    assert_eq!(
        issued_states.len(),
        2,
        "the table must cover an allocation with an issuance and one with none, covered \
         {issued_states:?}"
    );
}

/// Rule 4: an identifier naming no allocation of the catalog is refused with the
/// allocation-lookup message.
#[tokio::test]
async fn an_identifier_that_names_no_allocation_of_the_catalog_is_refused() {
    let harness = Harness::new();
    let application = application_arn(&harness, "tok-get-allocation-404", CATALOG, "tok-404", &[])
        .await;
    let benefit = benefit_id(&harness, "a benefit of the catalog", CATALOG).await;
    let other_catalog_allocation = stored_allocations(&harness, OTHER_CATALOG)
        .first()
        .map(|allocation| allocation.id.clone())
        .expect("the other seeded catalog holds allocations");

    // (case, the identifier the request names)
    let rows: Vec<(&str, String)> = vec![
        (
            "a well-formed id no grant ever minted",
            UNKNOWN_ALLOCATION.to_string(),
        ),
        (
            "an allocation ARN of the other catalog",
            expected_arn(OTHER_CATALOG, ALLOCATION_RESOURCE, &other_catalog_allocation),
        ),
        (
            "the other catalog's allocation id",
            other_catalog_allocation.clone(),
        ),
        (
            "a benefit ARN of this catalog",
            expected_arn(CATALOG, BENEFIT_RESOURCE, &benefit),
        ),
        ("an application ARN of this catalog", application),
        (
            "an application ARN of an application that does not exist",
            expected_arn(CATALOG, APPLICATION_RESOURCE, "benappl-00000000000000"),
        ),
        (
            "an ARN of another service",
            "arn:aws:s3:::a-bucket-of-another-service".to_string(),
        ),
    ];

    for (case, identifier) in &rows {
        refused(
            &harness,
            case,
            CATALOG,
            identifier,
            &not_found_message(ALLOCATION_KIND, identifier),
        )
        .await;
    }
}

/// Rule 5: a catalog the service does not serve is refused with the catalog-lookup message,
/// whatever the identifier — the catalog is looked up before the allocation is.
#[tokio::test]
async fn a_catalog_the_service_does_not_serve_is_refused() {
    let harness = Harness::new();
    let stored = stored_allocations(&harness, CATALOG)
        .first()
        .map(|allocation| allocation.id.clone())
        .expect("the seeded catalog holds allocations");

    // The `CatalogName` pattern `^[A-Za-z0-9_-]+$` admits all of these; none is a catalog
    // this service serves (`SEEDED_CATALOGS`), including the lower-case spelling of one.
    for catalog in ["NotACatalog", "aws"] {
        for identifier in [stored.as_str(), UNKNOWN_ALLOCATION] {
            let case = format!("{catalog}/{identifier}");
            refused(
                &harness,
                &case,
                catalog,
                identifier,
                &not_found_message(CATALOG_KIND, catalog),
            )
            .await;
        }
    }
}

/// Rule 6: a read materialises only the expiry the clock already made due — the allocation
/// past its `ExpiresAt` is never reported `ACTIVE`, a credit code past its own is never
/// reported redeemable, and a second read reports exactly what the first did.
#[tokio::test]
async fn a_read_materialises_a_due_expiry_and_changes_nothing_else() {
    let harness = Harness::new();

    // Every seeded allocation: the one whose term closed before state init reports the
    // expiry, those inside their term keep the status they were granted with, and two reads
    // agree in every case.
    let mut expired = 0usize;
    let mut live = 0usize;
    for allocation in stored_allocations(&harness, CATALOG) {
        let case = format!("seeded {}", allocation.id);
        let first = get(&harness, &case, CATALOG, &allocation.id).await;
        let second = get(&harness, &case, CATALOG, &allocation.id).await;
        assert_eq!(
            second, first,
            "case {case:?}: a second read must report exactly what the first did, \
             got {second:?} and {first:?}"
        );
        let status = member(&first, "Status").unwrap_or_default();
        let expires_at = instant(&case, &first, "ExpiresAt");
        if expires_at <= now_millis() {
            expired += 1;
            assert_ne!(
                status, ALLOCATION_STATUS_ACTIVE,
                "case {case:?}: an allocation past its ExpiresAt is never ACTIVE, got {first:?}"
            );
            if status == ALLOCATION_STATUS_INACTIVE {
                assert_eq!(
                    member(&first, "StatusReason").as_deref(),
                    Some(ALLOCATION_EXPIRED_REASON),
                    "case {case:?}: an expired allocation explains its status, got {first:?}"
                );
            }
        } else {
            live += 1;
            assert!(
                status == ALLOCATION_STATUS_ACTIVE || status == ALLOCATION_STATUS_FULFILLED,
                "case {case:?}: an allocation inside its term keeps the status it was \
                 granted with, got {status:?}"
            );
        }
    }
    assert!(
        expired > 0 && live > 0,
        "the table must cover an allocation past its term and one inside it, got \
         {expired} expired and {live} live"
    );

    // A credit code expires on the same rule, against its OWN ExpiresAt: the issued code of
    // the allocation below is placed past its term directly in the store (AWS's fulfillment
    // workflow is what issues a code, and no operation this emulator serves moves one), and
    // the read materialises the expiry the clock then owes.
    let with_codes = stored_allocations(&harness, CATALOG)
        .into_iter()
        .find(|allocation| {
            matches!(&allocation.fulfillment, Some(AllocationFulfillment::Credits { codes, .. }) if !codes.is_empty())
        })
        .expect("a seeded allocation has issued a credit code");
    let case = format!("expired code of {}", with_codes.id);
    let before = get(&harness, &case, CATALOG, &with_codes.id).await;
    for code in before["FulfillmentDetail"]["CreditDetails"]["Codes"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected credit codes, got {before:?}"))
    {
        assert_eq!(
            member(code, "Status").as_deref(),
            Some(ALLOCATION_STATUS_ACTIVE),
            "case {case:?}: a code inside its term is redeemable, got {code:?}"
        );
    }

    place_expiry(&harness, CATALOG, &with_codes.id, now_millis() - 1);
    let after = get(&harness, &case, CATALOG, &with_codes.id).await;
    for code in after["FulfillmentDetail"]["CreditDetails"]["Codes"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected credit codes, got {after:?}"))
    {
        assert_eq!(
            member(code, "Status").as_deref(),
            Some(ALLOCATION_STATUS_INACTIVE),
            "case {case:?}: a code past its ExpiresAt can no longer be redeemed, got {code:?}"
        );
    }
    assert_eq!(
        get(&harness, &case, CATALOG, &with_codes.id).await,
        after,
        "case {case:?}: once the expiry is materialised a further read changes nothing"
    );
}

/// Put one stored allocation's term end at `expires_at_millis`, and its credit codes' with
/// it, directly in the store.
///
/// AWS's own fulfillment workflow is what grants an allocation and issues its codes, and no
/// operation this emulator serves moves either, so a rule stated over a term that has
/// closed can only be exercised by placing the instant here. The behaviour under test is
/// still read and asserted over the wire.
fn place_expiry(harness: &Harness, catalog: &str, id: &str, expires_at_millis: i64) {
    let state = service_state(&**harness.engine())
        .expect("the in-memory engine carries this service's state");
    let mut store = state.lock();
    let allocation = store
        .catalogs
        .get_mut(catalog)
        .and_then(|catalog| catalog.allocations.get_mut(id))
        .unwrap_or_else(|| panic!("case {id:?}: names no stored allocation of {catalog:?}"));
    allocation.expires_at_millis = expires_at_millis;
    if let Some(AllocationFulfillment::Credits { codes, .. }) = allocation.fulfillment.as_mut() {
        for code in codes.iter_mut() {
            code.expires_at_millis = expires_at_millis;
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 7 — a materialised expiry is dated by the instant it fell due
// ---------------------------------------------------------------------------

/// Milliseconds in a day, so a row can place a term that closed a given number of days
/// before it reads the record.
const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// Which stored allocation a row places its term on: a CLASS of stored record rather than
/// one seeded id.
#[derive(Debug, Clone, Copy)]
enum Pick {
    /// The allocation fulfilled with this `FulfillmentType` that has issued no credit code.
    Type(&'static str),
    /// The allocation that has issued a credit code, which expires against its own
    /// `ExpiresAt`.
    IssuedCode,
}

/// The instant a row expects the report's `UpdatedAt` to carry.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The allocation's own `ExpiresAt`: the instant its expiry fell due.
    AllocationExpiry,
    /// The credit code's `ExpiresAt`: the instant the latest expiry this read materialised
    /// fell due.
    CodeExpiry,
    /// Nothing was owed, so `UpdatedAt` is still the instant of the last write the record
    /// saw.
    Unchanged,
}

/// One row of the table below: a stored allocation placed in `status`, with its own term
/// ending `allocation_expiry` after the row's base instant and its issued credit codes'
/// ending `code_expiry` after it (a negative offset is in the past, so that term has
/// closed), and the `UpdatedAt` the report must then carry.
struct Row {
    case: &'static str,
    pick: Pick,
    status: &'static str,
    allocation_expiry: i64,
    /// The codes' term, for a row whose allocation has issued one; `None` leaves the codes
    /// as the store holds them, which is what a row picking an allocation without any does.
    code_expiry: Option<i64>,
    expect: Expect,
}

/// The rows differ along everything the rule generalises over: which `FulfillmentType` the
/// allocation was fulfilled with, the status it is in, WHICH of the two expiries is owed,
/// which of two owed expiries fell due later, how long ago the term closed (down to one
/// millisecond) and whether anything is owed at all.
const EXPIRY_DATING_ROWS: &[Row] = &[
    Row {
        case: "CASH allocation, its term closed 35 days ago",
        pick: Pick::Type(FULFILLMENT_TYPE_CASH),
        status: ALLOCATION_STATUS_ACTIVE,
        allocation_expiry: -35 * MILLIS_PER_DAY,
        code_expiry: None,
        expect: Expect::AllocationExpiry,
    },
    Row {
        case: "ACCESS allocation, its term closed 400 days ago",
        pick: Pick::Type(FULFILLMENT_TYPE_ACCESS),
        status: ALLOCATION_STATUS_ACTIVE,
        allocation_expiry: -400 * MILLIS_PER_DAY,
        code_expiry: None,
        expect: Expect::AllocationExpiry,
    },
    Row {
        case: "CREDITS allocation with nothing issued, its term closed one millisecond ago",
        pick: Pick::Type(FULFILLMENT_TYPE_CREDITS),
        status: ALLOCATION_STATUS_ACTIVE,
        allocation_expiry: -1,
        code_expiry: None,
        expect: Expect::AllocationExpiry,
    },
    Row {
        case: "FULFILLED allocation, only its issued code's term closed",
        pick: Pick::IssuedCode,
        status: ALLOCATION_STATUS_FULFILLED,
        allocation_expiry: -50 * MILLIS_PER_DAY,
        code_expiry: Some(-5 * MILLIS_PER_DAY),
        expect: Expect::CodeExpiry,
    },
    Row {
        case: "both terms closed, the code's the later of the two",
        pick: Pick::IssuedCode,
        status: ALLOCATION_STATUS_ACTIVE,
        allocation_expiry: -30 * MILLIS_PER_DAY,
        code_expiry: Some(-3 * MILLIS_PER_DAY),
        expect: Expect::CodeExpiry,
    },
    Row {
        case: "both terms closed, the allocation's the later of the two",
        pick: Pick::IssuedCode,
        status: ALLOCATION_STATUS_ACTIVE,
        allocation_expiry: -2 * MILLIS_PER_DAY,
        code_expiry: Some(-40 * MILLIS_PER_DAY),
        expect: Expect::AllocationExpiry,
    },
    Row {
        case: "CASH allocation still inside its term",
        pick: Pick::Type(FULFILLMENT_TYPE_CASH),
        status: ALLOCATION_STATUS_ACTIVE,
        allocation_expiry: 30 * MILLIS_PER_DAY,
        code_expiry: None,
        expect: Expect::Unchanged,
    },
    Row {
        case: "allocation and issued code both inside their term",
        pick: Pick::IssuedCode,
        status: ALLOCATION_STATUS_ACTIVE,
        allocation_expiry: 10 * MILLIS_PER_DAY,
        code_expiry: Some(10 * MILLIS_PER_DAY),
        expect: Expect::Unchanged,
    },
    Row {
        case: "allocation already INACTIVE, so its closed term owes nothing",
        pick: Pick::Type(FULFILLMENT_TYPE_ACCESS),
        status: ALLOCATION_STATUS_INACTIVE,
        allocation_expiry: -10 * MILLIS_PER_DAY,
        code_expiry: None,
        expect: Expect::Unchanged,
    },
    Row {
        case: "FULFILLED allocation, both terms still open",
        pick: Pick::IssuedCode,
        status: ALLOCATION_STATUS_FULFILLED,
        allocation_expiry: 5 * MILLIS_PER_DAY,
        code_expiry: Some(5 * MILLIS_PER_DAY),
        expect: Expect::Unchanged,
    },
];

/// Rule 7: a report that materialises a change the clock already made due dates that change
/// at the instant it FELL DUE — the `ExpiresAt` the record already holds, the latest one
/// where the call materialises several — so `UpdatedAt` is a function of the stored state
/// and never of when the record happened to be read.
#[tokio::test]
async fn a_materialised_expiry_is_dated_by_the_instant_it_fell_due() {
    for row in EXPIRY_DATING_ROWS {
        let case = row.case;
        let harness = Harness::new();
        // Every instant this row places is relative to one taken BEFORE the read, so the
        // read's own instant is strictly later than any of them.
        let base = now_millis();
        let created_at = base - 500 * MILLIS_PER_DAY;
        let allocation = pick_allocation(&harness, case, CATALOG, row.pick);
        let allocation_expires_at = base + row.allocation_expiry;
        let code_expires_at = row.code_expiry.map(|offset| base + offset);
        place_term(
            &harness,
            case,
            CATALOG,
            &allocation.id,
            row.status,
            created_at,
            allocation_expires_at,
            code_expires_at,
        );

        let expected = match row.expect {
            Expect::AllocationExpiry => allocation_expires_at,
            Expect::CodeExpiry => code_expires_at
                .unwrap_or_else(|| panic!("case {case:?}: a code-dated row places a code term")),
            Expect::Unchanged => created_at,
        };

        let body = get(&harness, case, CATALOG, &allocation.id).await;
        let reported = instant(case, &body, "UpdatedAt");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected UpdatedAt {expected}, got {reported} in {body:?}"
        );
        assert!(
            reported < base,
            "case {case:?}: UpdatedAt must be the instant the change happened, not the \
             instant of the read; expected a value below {base}, got {reported}"
        );
        // The status follows the same rule: an `ACTIVE` allocation whose term has closed is
        // `INACTIVE`, and everything else keeps the status it was placed in.
        let expected_status = if row.status == ALLOCATION_STATUS_ACTIVE && row.allocation_expiry < 0
        {
            ALLOCATION_STATUS_INACTIVE
        } else {
            row.status
        };
        assert_eq!(
            member(&body, "Status").as_deref(),
            Some(expected_status),
            "case {case:?}: expected Status {expected_status:?}, got {body:?}"
        );
        // The store holds exactly what the report carried, so a reader that never calls
        // this operation sees the same instant...
        let stored = stored_allocations(&harness, CATALOG)
            .into_iter()
            .find(|stored| stored.id == allocation.id)
            .unwrap_or_else(|| panic!("case {case:?}: the allocation is still stored"));
        assert_eq!(
            stored.updated_at_millis, expected,
            "case {case:?}: expected the store to hold UpdatedAt {expected}, got {stored:?}"
        );
        // ... and reading again answers it unchanged: the value does not move with the clock.
        let again = get(&harness, case, CATALOG, &allocation.id).await;
        assert_eq!(
            again, body,
            "case {case:?}: a second read must report exactly what the first did, got \
             {again:?} and {body:?}"
        );
    }
}

/// The sibling report materialises the same expiries, so it dates them the same way: after
/// a `ListBenefitAllocations` the record carries the due instant, not the instant that list
/// was taken.
#[tokio::test]
async fn the_sibling_report_dates_a_materialised_expiry_by_the_due_instant_too() {
    // (case, which allocation, its term, its codes' term, the due instant expected)
    let rows: &[(&str, Pick, i64, Option<i64>, i64)] = &[
        (
            "the allocation's own term closed 12 days ago",
            Pick::Type(FULFILLMENT_TYPE_CASH),
            -12 * MILLIS_PER_DAY,
            None,
            -12 * MILLIS_PER_DAY,
        ),
        (
            "only an issued code's term closed, 4 days ago",
            Pick::IssuedCode,
            7 * MILLIS_PER_DAY,
            Some(-4 * MILLIS_PER_DAY),
            -4 * MILLIS_PER_DAY,
        ),
    ];

    for (case, pick, allocation_expiry, code_expiry, due) in rows {
        let harness = Harness::new();
        let base = now_millis();
        let created_at = base - 500 * MILLIS_PER_DAY;
        let allocation = pick_allocation(&harness, case, CATALOG, *pick);
        place_term(
            &harness,
            case,
            CATALOG,
            &allocation.id,
            ALLOCATION_STATUS_ACTIVE,
            created_at,
            base + allocation_expiry,
            code_expiry.map(|offset| base + offset),
        );

        // The listing is what materialises the expiry here; it reports no `UpdatedAt` of its
        // own, so the write it leaves behind is read back from the store and over the wire.
        list(&harness, case, &json!({ "Catalog": CATALOG })).await;
        let expected = base + due;
        let stored = stored_allocations(&harness, CATALOG)
            .into_iter()
            .find(|stored| stored.id == allocation.id)
            .unwrap_or_else(|| panic!("case {case:?}: the allocation is still stored"));
        assert_eq!(
            stored.updated_at_millis, expected,
            "case {case:?}: expected the listing to date the expiry at {expected}, got {stored:?}"
        );
        let body = get(&harness, case, CATALOG, &allocation.id).await;
        let reported = instant(case, &body, "UpdatedAt");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected UpdatedAt {expected}, got {reported} in {body:?}"
        );
        assert!(
            reported < base,
            "case {case:?}: UpdatedAt must be the instant the change happened, not the \
             instant of the listing; expected a value below {base}, got {reported}"
        );
    }
}

/// The stored allocation of `catalog` that `pick` names.
fn pick_allocation(
    harness: &Harness,
    case: &str,
    catalog: &str,
    pick: Pick,
) -> BenefitAllocation {
    let has_code = |allocation: &BenefitAllocation| {
        matches!(&allocation.fulfillment, Some(AllocationFulfillment::Credits { codes, .. }) if !codes.is_empty())
    };
    stored_allocations(harness, catalog)
        .into_iter()
        .find(|allocation| match pick {
            Pick::IssuedCode => has_code(allocation),
            Pick::Type(fulfillment_type) => {
                allocation.fulfillment_type == fulfillment_type && !has_code(allocation)
            }
        })
        .unwrap_or_else(|| panic!("case {case:?}: {catalog:?} holds no allocation matching {pick:?}"))
}

/// Place one stored allocation's status, its own term end and its issued codes' term end
/// directly in the store, with every other instant on the record at `created_at_millis`.
///
/// AWS's own fulfillment workflow is what grants an allocation and issues its codes, and no
/// operation this emulator serves moves either, so a rule stated over a term that has closed
/// can only be exercised by placing the instants here; the behaviour under test is still
/// read and asserted over the wire. Dating the record's last write at its creation is what
/// makes a materialised expiry the latest write the record has seen.
#[allow(clippy::too_many_arguments)]
fn place_term(
    harness: &Harness,
    case: &str,
    catalog: &str,
    id: &str,
    status: &str,
    created_at_millis: i64,
    expires_at_millis: i64,
    code_expires_at_millis: Option<i64>,
) {
    let state = service_state(&**harness.engine())
        .expect("the in-memory engine carries this service's state");
    let mut store = state.lock();
    let allocation = store
        .catalogs
        .get_mut(catalog)
        .and_then(|catalog| catalog.allocations.get_mut(id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: {id:?} names no stored allocation of {catalog:?}")
        });
    allocation.status = status.to_string();
    allocation.status_reason = None;
    allocation.created_at_millis = created_at_millis;
    allocation.starts_at_millis = created_at_millis;
    allocation.updated_at_millis = created_at_millis;
    allocation.expires_at_millis = expires_at_millis;
    if let Some(AllocationFulfillment::Credits { codes, .. }) = allocation.fulfillment.as_mut() {
        for code in codes.iter_mut() {
            code.issued_at_millis = created_at_millis;
            if let Some(code_expires_at_millis) = code_expires_at_millis {
                code.expires_at_millis = code_expires_at_millis;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 8 — the two terms are judged by ONE boundary
// ---------------------------------------------------------------------------

/// One question put to the expiry boundary: how far a term's end lies from the instant the
/// question is asked, and whether the term is closed then.
///
/// The rows differ along what the boundary generalises over — which side of the end the
/// instant falls on and how far from it — and include the two the clock-driven reads below
/// cannot place: the end instant itself, and the millisecond before it.
const TERM_BOUNDARY_ROWS: &[(&str, i64, bool)] = &[
    ("a term ending 400 days from now is open", 400 * MILLIS_PER_DAY, false),
    ("a term ending one millisecond from now is open", 1, false),
    ("a term ending at this very instant is closed", 0, true),
    ("a term that ended one millisecond ago is closed", -1, true),
    ("a term that ended 400 days ago is closed", -400 * MILLIS_PER_DAY, true),
];

/// Rule 8: the boundary that decides whether a term has closed at an instant is ONE rule,
/// so an allocation's term and an issued credit code's term answer it identically for the
/// same pair of instants — at the end instant itself included.
#[tokio::test]
async fn one_boundary_decides_an_allocation_term_and_a_credit_code_term_alike() {
    let harness = Harness::new();
    let case = "the allocation that has issued a credit code";
    let stored = pick_allocation(&harness, case, CATALOG, Pick::IssuedCode);
    let issued = issued_codes(case, &stored);

    // Two anchors for "now", so neither answer can come from the magnitude of the instants:
    // the epoch itself and a present-day instant.
    for now in [0_i64, now_millis()] {
        for (row, offset, closed) in TERM_BOUNDARY_ROWS {
            let case = format!("{row} (now {now})");
            let expires_at = now + offset;
            assert_eq!(
                term_closed(expires_at, now),
                *closed,
                "case {case:?}: expected term_closed({expires_at}, {now}) to be {closed}, \
                 got {}",
                term_closed(expires_at, now)
            );

            let mut allocation = stored.clone();
            allocation.expires_at_millis = expires_at;
            assert_eq!(
                allocation.has_expired(now),
                *closed,
                "case {case:?}: an allocation's term must be judged by the one boundary, \
                 expected {closed}, got {}",
                allocation.has_expired(now)
            );

            let mut code = issued[0].clone();
            code.expires_at_millis = expires_at;
            assert_eq!(
                code.has_expired(now),
                *closed,
                "case {case:?}: an issued code's term must be judged by the same boundary, \
                 expected {closed}, got {}",
                code.has_expired(now)
            );
        }
    }
}

/// One row of the wire table below: the allocation's own term and its issued codes' term
/// placed at DIFFERENT offsets from the row's base instant (a negative offset is in the
/// past, so that term has closed), and the status each must then be reported in.
struct SplitTermRow {
    case: &'static str,
    allocation_expiry: i64,
    code_expiry: i64,
    allocation_status: &'static str,
    code_status: &'static str,
}

/// The rows differ along which of the two terms has closed, so each term is judged on its
/// OWN end: one closed while the other is open, in either direction, both closed and
/// neither closed.
const SPLIT_TERM_ROWS: &[SplitTermRow] = &[
    SplitTermRow {
        case: "only the allocation's own term has closed",
        allocation_expiry: -9 * MILLIS_PER_DAY,
        code_expiry: 400 * MILLIS_PER_DAY,
        allocation_status: ALLOCATION_STATUS_INACTIVE,
        code_status: ALLOCATION_STATUS_ACTIVE,
    },
    SplitTermRow {
        case: "only the issued code's term has closed",
        allocation_expiry: 400 * MILLIS_PER_DAY,
        code_expiry: -1,
        allocation_status: ALLOCATION_STATUS_ACTIVE,
        code_status: ALLOCATION_STATUS_INACTIVE,
    },
    SplitTermRow {
        case: "both terms have closed",
        allocation_expiry: -1,
        code_expiry: -9 * MILLIS_PER_DAY,
        allocation_status: ALLOCATION_STATUS_INACTIVE,
        code_status: ALLOCATION_STATUS_INACTIVE,
    },
    SplitTermRow {
        case: "neither term has closed",
        allocation_expiry: 400 * MILLIS_PER_DAY,
        code_expiry: 400 * MILLIS_PER_DAY,
        allocation_status: ALLOCATION_STATUS_ACTIVE,
        code_status: ALLOCATION_STATUS_ACTIVE,
    },
];

/// Rule 8 over the wire, through `GetBenefitAllocation`: with the two terms ending at
/// different instants, the allocation is reported on its own end and each issued code on
/// its own, which is the one boundary asked twice.
#[tokio::test]
async fn a_read_judges_each_term_on_its_own_end() {
    for row in SPLIT_TERM_ROWS {
        let case = row.case;
        let harness = Harness::new();
        let base = now_millis();
        let allocation = pick_allocation(&harness, case, CATALOG, Pick::IssuedCode);
        place_term(
            &harness,
            case,
            CATALOG,
            &allocation.id,
            ALLOCATION_STATUS_ACTIVE,
            base - 500 * MILLIS_PER_DAY,
            base + row.allocation_expiry,
            Some(base + row.code_expiry),
        );

        let body = get(&harness, case, CATALOG, &allocation.id).await;
        assert_eq!(
            member(&body, "Status").as_deref(),
            Some(row.allocation_status),
            "case {case:?}: expected the allocation reported {:?}, got {body:?}",
            row.allocation_status
        );
        let codes = body["FulfillmentDetail"]["CreditDetails"]["Codes"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected credit codes, got {body:?}"))
            .clone();
        assert!(
            !codes.is_empty(),
            "case {case:?}: the row needs an issued code to judge, got {body:?}"
        );
        for code in &codes {
            assert_eq!(
                member(code, "Status").as_deref(),
                Some(row.code_status),
                "case {case:?}: expected the issued code reported {:?}, got {code:?}",
                row.code_status
            );
        }
    }
}

/// Rule 8 over the wire, through the sibling `ListBenefitAllocations`: the other report
/// materialises the same two expiries, so it judges each term on its own end as well.
#[tokio::test]
async fn the_sibling_report_judges_each_term_on_its_own_end_too() {
    for row in SPLIT_TERM_ROWS {
        let case = row.case;
        let harness = Harness::new();
        let base = now_millis();
        let allocation = pick_allocation(&harness, case, CATALOG, Pick::IssuedCode);
        place_term(
            &harness,
            case,
            CATALOG,
            &allocation.id,
            ALLOCATION_STATUS_ACTIVE,
            base - 500 * MILLIS_PER_DAY,
            base + row.allocation_expiry,
            Some(base + row.code_expiry),
        );

        let (summaries, _) = list(&harness, case, &json!({ "Catalog": CATALOG })).await;
        let summary = summaries
            .iter()
            .find(|summary| member(summary, "Id").as_deref() == Some(allocation.id.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the listing must report {:?}, got {summaries:?}",
                    allocation.id
                )
            });
        assert_eq!(
            member(summary, "Status").as_deref(),
            Some(row.allocation_status),
            "case {case:?}: expected the allocation listed {:?}, got {summary:?}",
            row.allocation_status
        );

        // The listing reports no credit code of its own, so the expiry it materialised on
        // the codes is read back from the store it wrote.
        let stored = stored_allocations(&harness, CATALOG)
            .into_iter()
            .find(|stored| stored.id == allocation.id)
            .unwrap_or_else(|| panic!("case {case:?}: the allocation is still stored"));
        for code in issued_codes(case, &stored) {
            assert_eq!(
                code.status, row.code_status,
                "case {case:?}: expected the issued code left {:?}, got {code:?}",
                row.code_status
            );
        }
    }
}

/// The credit codes a stored allocation has issued, which a row judging a code's own term
/// needs at least one of.
fn issued_codes(case: &str, allocation: &BenefitAllocation) -> Vec<AllocatedCreditCode> {
    let codes = match &allocation.fulfillment {
        Some(AllocationFulfillment::Credits { codes, .. }) => codes.clone(),
        other => panic!(
            "case {case:?}: expected an allocation fulfilled with credits, got {other:?}"
        ),
    };
    assert!(
        !codes.is_empty(),
        "case {case:?}: expected an allocation that has issued a credit code, got {codes:?}"
    );
    codes
}
