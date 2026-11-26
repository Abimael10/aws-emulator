// SPDX-License-Identifier: CC0-1.0

//! `RedeemCredits`, end to end over the awsJson1_0 wire.
//!
//! The RULE under test: redeeming a promotional credit code this emulator ISSUED to the
//! calling account adds, exactly once per code, a credit to that account's credit ledger
//! carrying the amount and validity period of the promotion the code names — the ledger
//! `GetCredits` reports from — while a code nothing issued to the calling account, a code
//! already redeemed, and a code whose validity period has closed are each refused with
//! the modeled 400 `ValidationException` naming `promoCode`, and add nothing.
//!
//! An issued code is the AWS partner benefits service's own record: the modelled
//! `CreditCode$AwsCreditCode` is "the actual credit code string that can be redeemed in
//! the AWS billing console", and `CreditCode$Status` is "the current status of the credit
//! code (e.g., active, redeemed, expired)". So the codes are read — and spent — in the
//! store of the service that issues them, which this suite registers beside the billing
//! service on ONE shared engine, exactly as `core/router` does in production. Every
//! redemption is then checked from BOTH surfaces: the credit through `GetCredits`, and
//! the spent code through the issuer's own `GetBenefitAllocation`.
//!
//! The tables span what the rule generalises over: the code's amount (one cent, a whole
//! unit, a four-figure promotion, an amount with a non-zero fraction, a five-figure one),
//! its validity period (a day, a month, a year, a decade; closed a minute ago and closed
//! a month ago), which catalog and which benefit allocation issued it, the code the
//! emulator issues out of the box, and the forms a code that names no promotion of this
//! account takes.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state as billing_state;
use service_com_amazonaws_billing_awsbilling::EmulatedService as BillingService;
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::state as partner_state;
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::EmulatedService as PartnerBenefitsService;
use smithy_server_runtime::MultiServiceRouter;

/// The account every caller of this emulator authenticates as (`engine::EMULATOR_ACCOUNT_ID`),
/// which is the account a code is redeemed "on behalf of".
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// A second 12-digit account, for a code issued to somebody else.
const OTHER_ACCOUNT_ID: &str = "210987654321";

/// The billing service's sigv4 signing name and `X-Amz-Target` prefix
/// (`OPERATIONS.md`, "Wire protocol").
const BILLING_SIGNING_NAME: &str = "billing";
const BILLING_TARGET_PREFIX: &str = "AWSBilling";

/// The same two for the service that ISSUES promotional credit codes.
const PARTNER_SIGNING_NAME: &str = "partnercentral-benefits";
const PARTNER_TARGET_PREFIX: &str = "PartnerCentralBenefitsService";

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// Milliseconds in a day, for placing a code's validity period.
const DAY_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// Seconds in a day, for placing the period a `GetCredits` call asks about.
const DAY_SECS: i64 = 24 * 60 * 60;

/// The `creditType` the modeled `CreditData$creditType` doc names first ("Examples:
/// `Promotion`, `Refund`, `TrueUp`") — a credit a promotional code earned.
const PROMOTION_CREDIT_TYPE: &str = "Promotion";

/// The `CreditStatus` of a credit that participates in billing runs.
const CREDIT_STATUS_ENABLED: &str = "ENABLED";

/// The currency every amount this emulator's promotions are denominated in
/// (`partner_state::ALLOCATION_CURRENCY`).
const CURRENCY_CODE: &str = "USD";

/// The refusals the operation words, one per state a presented code can be in.
const NOT_ISSUED_MESSAGE: &str = "The promotional code you entered is not valid";
const ALREADY_REDEEMED_MESSAGE: &str =
    "The promotional code you entered has already been redeemed";
const EXPIRED_MESSAGE: &str = "The promotional code you entered has expired";

/// How close two instants must be to count as the same one: the ledger keeps an instant
/// to the second-with-fraction the wire carries, the code registry to the millisecond.
const INSTANT_TOLERANCE_SECS: f64 = 0.002;

/// One emulator holding the billing service and the service that issues promotional
/// credit codes on ONE shared engine — what `core/router` assembles in production, so a
/// redemption on one surface is visible from the other.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, and the fixed test credential
        // below is not this process's signing key.
        router.set_request_authentication(false);
        router.register_service(BillingService::new(Arc::clone(&engine)));
        router.register_service(PartnerBenefitsService::new(Arc::clone(&engine)));
        // The issuer refuses every request from an account that holds no active AWS
        // partner benefit, and its API models no partner registration, so the
        // entitlement its OWN reads need is recorded in process. It gates that
        // service's surface only: the codes themselves are in its store either way.
        partner_state::service_state(&*engine)
            .expect("the in-memory engine carries the issuer's state")
            .lock()
            .grant_partner_entitlement(ACCOUNT_ID);
        Self { router, engine }
    }

    /// Send one awsJson1_0 call to `signing_name`'s service and return its HTTP status
    /// and decoded body.
    async fn call(
        &self,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let decoded = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, decoded)
    }

    /// `RedeemCredits` with `promo_code`.
    async fn redeem(&self, promo_code: &str) -> (u16, Value) {
        self.call(
            BILLING_SIGNING_NAME,
            BILLING_TARGET_PREFIX,
            "RedeemCredits",
            &json!({ "promoCode": promo_code }),
        )
        .await
    }

    /// The credits `GetCredits` reports for this emulator's account over a period that
    /// covers every credit a redemption in this suite can mint.
    async fn credits(&self, case: &str) -> Vec<Value> {
        let start_date = epoch_secs(SystemTime::now()) as i64 - DAY_SECS;
        let (status, body) = self
            .call(
                BILLING_SIGNING_NAME,
                BILLING_TARGET_PREFIX,
                "GetCredits",
                &json!({ "accountId": ACCOUNT_ID, "startDate": start_date }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetCredits to answer 200, got {status} with {body:?}"
        );
        body["credits"].as_array().cloned().unwrap_or_default()
    }

    /// The status the ISSUER reports for the code `code` of the allocation `allocation_id`
    /// in `catalog` — the owning service's own read, over its own wire.
    async fn issued_code_status(&self, case: &str, promotion: &Promotion) -> String {
        let (status, body) = self
            .call(
                PARTNER_SIGNING_NAME,
                PARTNER_TARGET_PREFIX,
                "GetBenefitAllocation",
                &json!({ "Catalog": promotion.catalog, "Identifier": promotion.allocation_id }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetBenefitAllocation to answer 200, got {status} with \
             {body:?}"
        );
        let codes = body["FulfillmentDetail"]["CreditDetails"]["Codes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let found = codes
            .iter()
            .find(|entry| entry["AwsCreditCode"].as_str() == Some(promotion.code.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the issuer must still report the code {:?}, got {codes:?}",
                    promotion.code
                )
            });
        found["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// The ids, in store order, of the allocations of `catalog` that are fulfilled with
    /// credit codes — the records a code can be issued against.
    fn credit_allocations(&self, catalog: &str) -> Vec<String> {
        let state = partner_state::service_state(&*self.engine)
            .expect("the in-memory engine carries the issuer's state");
        let store = state.lock();
        store
            .catalogs
            .get(catalog)
            .expect("a catalog the issuer seeds")
            .allocations
            .values()
            .filter(|allocation| {
                matches!(
                    allocation.fulfillment,
                    Some(partner_state::AllocationFulfillment::Credits { .. })
                )
            })
            .map(|allocation| allocation.id.clone())
            .collect()
    }

    /// Issue one promotional credit code, the way AWS's own fulfillment workflow does:
    /// against a benefit allocation in the ISSUER's store, which is where every read of
    /// it — and the redemption that spends it — looks.
    ///
    /// `allocation_index` picks which of `catalog`'s credit allocations issues it, so two
    /// rows can draw their promotion from different allocations (and so describe
    /// themselves differently).
    fn issue_code(
        &self,
        catalog: &str,
        allocation_index: usize,
        code: &str,
        account_id: &str,
        minor_units: i64,
        expires_at_millis: i64,
    ) -> Promotion {
        let allocation_id = self
            .credit_allocations(catalog)
            .get(allocation_index)
            .expect("the issuer seeds at least that many credit allocations per catalog")
            .clone();
        let state = partner_state::service_state(&*self.engine)
            .expect("the in-memory engine carries the issuer's state");
        let mut store = state.lock();
        let allocation = store
            .catalogs
            .get_mut(catalog)
            .expect("a catalog the issuer seeds")
            .allocations
            .get_mut(&allocation_id)
            .expect("an allocation the issuer seeds");
        let description = allocation
            .description
            .clone()
            .unwrap_or_else(|| allocation.name.clone());
        let Some(partner_state::AllocationFulfillment::Credits { codes, .. }) =
            allocation.fulfillment.as_mut()
        else {
            panic!("a credit allocation is fulfilled with credit codes");
        };
        codes.push(partner_state::AllocatedCreditCode {
            aws_account_id: account_id.to_string(),
            code: code.to_string(),
            value: partner_state::Money::new(minor_units),
            status: partner_state::ALLOCATION_STATUS_ACTIVE.to_string(),
            issued_at_millis: expires_at_millis - DAY_MILLIS,
            expires_at_millis,
        });
        Promotion {
            catalog: catalog.to_string(),
            allocation_id,
            code: code.to_string(),
            description,
            expires_at_millis,
        }
    }

    /// The code the emulator issues out of the box, read from the ISSUER over its own
    /// wire: the first code of the first credit allocation of `catalog` that holds one.
    async fn issued_promotion(&self, catalog: &str) -> (Promotion, String) {
        for allocation_id in self.credit_allocations(catalog) {
            let (status, body) = self
                .call(
                    PARTNER_SIGNING_NAME,
                    PARTNER_TARGET_PREFIX,
                    "GetBenefitAllocation",
                    &json!({ "Catalog": catalog, "Identifier": allocation_id }),
                )
                .await;
            assert_eq!(
                status, 200,
                "expected GetBenefitAllocation to answer 200, got {status} with {body:?}"
            );
            let detail = &body["FulfillmentDetail"]["CreditDetails"];
            let Some(code) = detail["Codes"].as_array().and_then(|codes| codes.first()) else {
                continue;
            };
            let expires_at_millis = self.stored_expiry(
                catalog,
                &allocation_id,
                code["AwsCreditCode"].as_str().unwrap_or_default(),
            );
            return (
                Promotion {
                    catalog: catalog.to_string(),
                    allocation_id,
                    code: code["AwsCreditCode"]
                        .as_str()
                        .expect("the issuer reports the code text")
                        .to_string(),
                    description: body["Description"].as_str().unwrap_or_default().to_string(),
                    expires_at_millis,
                },
                code["Value"]["Amount"]
                    .as_str()
                    .expect("the issuer reports the code's value")
                    .to_string(),
            );
        }
        panic!("the issuer must hold at least one issued credit code in {catalog:?}");
    }

    /// When the stored code expires, on the issuer's own millisecond scale. Read from the
    /// store rather than from its `date-time` report, so the expectation needs no second
    /// timestamp parser.
    fn stored_expiry(&self, catalog: &str, allocation_id: &str, code: &str) -> i64 {
        let state = partner_state::service_state(&*self.engine)
            .expect("the in-memory engine carries the issuer's state");
        let store = state.lock();
        let allocation = store
            .catalogs
            .get(catalog)
            .expect("a catalog the issuer seeds")
            .allocations
            .get(allocation_id)
            .expect("an allocation the issuer seeds");
        let Some(partner_state::AllocationFulfillment::Credits { codes, .. }) =
            allocation.fulfillment.as_ref()
        else {
            panic!("a credit allocation is fulfilled with credit codes");
        };
        codes
            .iter()
            .find(|entry| entry.code == code)
            .expect("a code the issuer just reported")
            .expires_at_millis
    }
}

/// One promotion as this suite holds it: where the code lives, what it is called, and the
/// validity period it carries.
struct Promotion {
    catalog: String,
    allocation_id: String,
    code: String,
    description: String,
    expires_at_millis: i64,
}

/// An instant as the epoch seconds the wire carries for a modeled `Timestamp`.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The current instant on the issuer's millisecond scale.
fn now_millis() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .expect("an instant that fits")
}

/// The `__type` an awsJson error body carries, without its namespace prefix.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `creditId`s a `GetCredits` answer lists.
fn credit_ids(credits: &[Value]) -> Vec<String> {
    credits
        .iter()
        .map(|credit| credit["creditId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The one credit `after` holds that `before` did not.
fn minted_credit(case: &str, before: &[Value], after: &[Value]) -> Value {
    let known = credit_ids(before);
    let minted: Vec<Value> = after
        .iter()
        .filter(|credit| {
            !known.contains(&credit["creditId"].as_str().unwrap_or_default().to_string())
        })
        .cloned()
        .collect();
    assert_eq!(
        minted.len(),
        1,
        "case {case:?}: expected the redemption to add exactly one credit, got {minted:?} \
         after {known:?}"
    );
    minted.into_iter().next().expect("one minted credit")
}

/// Refuse-and-change-nothing: `RedeemCredits` answers 400 `ValidationException` with
/// `expected_message`, naming `promoCode`, and the ledger is exactly what it was.
async fn refused(emulator: &Emulator, case: &str, promo_code: &str, expected_message: &str) {
    let before = emulator.credits(case).await;
    let (status, body) = emulator.redeem(promo_code).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {body:?}"
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        expected_message,
        "case {case:?}: expected the refusal to say why, got {body:?}"
    );
    assert_eq!(
        body["reason"].as_str().unwrap_or_default(),
        "fieldValidationFailed",
        "case {case:?}: expected the modeled reason, got {body:?}"
    );
    assert_eq!(
        body["fieldList"][0]["name"].as_str().unwrap_or_default(),
        "promoCode",
        "case {case:?}: expected the refusal to name the member, got {body:?}"
    );
    let after = emulator.credits(case).await;
    assert_eq!(
        credit_ids(&before),
        credit_ids(&after),
        "case {case:?}: a refused redemption must mint nothing, got {after:?} after {before:?}"
    );
}

/// Assert the credit `credit` carries the terms of `promotion`, with `expected_amount` as
/// its amount, and that it was minted between `before` and `after`.
fn assert_credit(
    case: &str,
    credit: &Value,
    promotion: &Promotion,
    expected_amount: &str,
    before: f64,
    after: f64,
) {
    let credit_id = credit["creditId"].as_str().unwrap_or_default();
    assert!(
        !credit_id.is_empty()
            && credit_id.len() <= 32
            && credit_id.bytes().all(|byte| byte.is_ascii_digit()),
        "case {case:?}: expected a modeled CreditId (1..32 digits), got {credit_id:?}"
    );
    assert_eq!(
        credit["accountId"].as_str().unwrap_or_default(),
        ACCOUNT_ID,
        "case {case:?}: expected the credit to belong to the redeeming account, got {credit:?}"
    );
    assert_eq!(
        credit["creditType"].as_str().unwrap_or_default(),
        PROMOTION_CREDIT_TYPE,
        "case {case:?}: expected a promotional credit, got {credit:?}"
    );
    for member in ["initialAmount", "remainingAmount"] {
        assert_eq!(
            credit[member],
            json!({ "currencyCode": CURRENCY_CODE, "currencyAmount": expected_amount }),
            "case {case:?}: expected {member} to be the promotion's amount \
             {expected_amount:?}, got {credit:?}"
        );
    }
    assert_eq!(
        credit["creditStatus"].as_str().unwrap_or_default(),
        CREDIT_STATUS_ENABLED,
        "case {case:?}: expected the redeemed credit to participate in billing runs, got \
         {credit:?}"
    );
    assert_eq!(
        credit["description"].as_str().unwrap_or_default(),
        promotion.description,
        "case {case:?}: expected the promotion's own description, got {credit:?}"
    );
    let end_date = credit["endDate"].as_f64().unwrap_or_default();
    let expected_end = promotion.expires_at_millis as f64 / 1000.0;
    assert!(
        (end_date - expected_end).abs() < INSTANT_TOLERANCE_SECS,
        "case {case:?}: expected the credit to expire with the promotion ({expected_end}), \
         got {end_date}"
    );
    let start_date = credit["startDate"].as_f64().unwrap_or_default();
    assert!(
        start_date >= before - INSTANT_TOLERANCE_SECS
            && start_date <= after + INSTANT_TOLERANCE_SECS,
        "case {case:?}: expected the credit to become valid at the redemption \
         ([{before}, {after}]), got {start_date}"
    );
    // What the promotion does not define, the credit does not record.
    for absent in [
        "estimatedAmount",
        "exhaustDate",
        "applicableProductNames",
        "applicationType",
        "shareableAccounts",
        "creditSharingType",
        "costCategoryArn",
        "ruleName",
        "purchaseTypeApplications",
    ] {
        assert!(
            credit[absent].is_null(),
            "case {case:?}: the promotion records no {absent}, got {credit:?}"
        );
    }
}

/// One row of the redemption table: a promotion issued with these terms.
struct Row {
    case: &'static str,
    catalog: &'static str,
    allocation_index: usize,
    code: &'static str,
    minor_units: i64,
    /// The amount the ledger must carry, in the modeled `CurrencyAmount` decimal form.
    expected_amount: &'static str,
    /// How long the promotion stays valid, in days from the redemption.
    valid_for_days: i64,
}

/// RULE: redeeming a code the emulator issued to the calling account adds to that
/// account's ledger a credit carrying the promotion's own amount and validity period, and
/// the issuer then reports the code as redeemed.
#[tokio::test]
async fn a_redeemed_code_mints_the_promotions_credit_into_the_ledger() {
    // Rows differ along the amount (the smallest the currency has, a whole unit, a
    // four-figure promotion, one with a non-zero fraction, a five-figure one), the
    // validity period (a day to a decade), the catalog, and which allocation issued it.
    let rows = [
        Row {
            case: "one cent, valid for a day",
            catalog: "AWS",
            allocation_index: 0,
            code: "PROMOONECENT01",
            minor_units: 1,
            expected_amount: "0.01",
            valid_for_days: 1,
        },
        Row {
            case: "one whole unit, valid for a month",
            catalog: "AWS",
            allocation_index: 1,
            code: "PROMOONEUNIT02",
            minor_units: 100,
            expected_amount: "1.00",
            valid_for_days: 30,
        },
        Row {
            case: "a four-figure promotion, valid for a year",
            catalog: "Sandbox",
            allocation_index: 0,
            code: "PROMOFIGURES03",
            minor_units: 500_000,
            expected_amount: "5000.00",
            valid_for_days: 365,
        },
        Row {
            case: "an amount with a non-zero fraction",
            catalog: "Sandbox",
            allocation_index: 1,
            code: "PROMOFRACTIO04",
            minor_units: 123_456,
            expected_amount: "1234.56",
            valid_for_days: 7,
        },
        Row {
            case: "a five-figure promotion, valid for a decade",
            catalog: "AWS",
            allocation_index: 0,
            code: "PROMOLARGEST05",
            minor_units: 10_000_000,
            expected_amount: "100000.00",
            valid_for_days: 3650,
        },
    ];

    for row in rows {
        let emulator = Emulator::new();
        let promotion = emulator.issue_code(
            row.catalog,
            row.allocation_index,
            row.code,
            ACCOUNT_ID,
            row.minor_units,
            now_millis() + row.valid_for_days * DAY_MILLIS,
        );
        let before = emulator.credits(row.case).await;
        let at_least = epoch_secs(SystemTime::now());
        let (status, body) = emulator.redeem(&promotion.code).await;
        let at_most = epoch_secs(SystemTime::now());
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status} with {body:?}",
            row.case
        );
        assert_eq!(
            body,
            json!({}),
            "case {:?}: the modeled response carries no member, got {body:?}",
            row.case
        );

        let after = emulator.credits(row.case).await;
        let credit = minted_credit(row.case, &before, &after);
        assert_credit(
            row.case,
            &credit,
            &promotion,
            row.expected_amount,
            at_least,
            at_most,
        );

        assert_eq!(
            emulator.issued_code_status(row.case, &promotion).await,
            partner_state::ALLOCATION_STATUS_FULFILLED,
            "case {:?}: the issuer must report the spent code as redeemed",
            row.case
        );
    }
}

/// RULE, on the code this emulator issues out of the box: the whole flow a caller follows
/// — read the issued code from the service that issued it, redeem it in billing, read the
/// credit back — works with nothing seeded by the test.
#[tokio::test]
async fn the_code_the_emulator_issued_is_redeemable_end_to_end() {
    for catalog in partner_state::SEEDED_CATALOGS {
        let emulator = Emulator::new();
        let (promotion, issued_amount) = emulator.issued_promotion(catalog).await;
        let case = promotion.catalog.clone();
        assert_eq!(
            emulator.issued_code_status(&case, &promotion).await,
            partner_state::ALLOCATION_STATUS_ACTIVE,
            "case {case:?}: an issued code is handed over redeemable"
        );

        let before = emulator.credits(&case).await;
        let at_least = epoch_secs(SystemTime::now());
        let (status, body) = emulator.redeem(&promotion.code).await;
        let at_most = epoch_secs(SystemTime::now());
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body:?}"
        );

        let after = emulator.credits(&case).await;
        let credit = minted_credit(&case, &before, &after);
        assert_credit(
            &case,
            &credit,
            &promotion,
            &issued_amount,
            at_least,
            at_most,
        );
        assert_eq!(
            emulator.issued_code_status(&case, &promotion).await,
            partner_state::ALLOCATION_STATUS_FULFILLED,
            "case {case:?}: the issuer must report the spent code as redeemed"
        );
    }
}

/// RULE: a code is redeemable exactly once — a second presentation of the same code is
/// refused and mints nothing, whichever of the codes this emulator issued it is.
#[tokio::test]
async fn a_promotional_code_is_redeemable_exactly_once() {
    let emulator = Emulator::new();
    let promotions = vec![
        emulator.issue_code(
            "AWS",
            0,
            "PROMOONCEONLY1",
            ACCOUNT_ID,
            250_000,
            now_millis() + 90 * DAY_MILLIS,
        ),
        emulator.issue_code(
            "Sandbox",
            1,
            "PROMOONCEONLY2",
            ACCOUNT_ID,
            1,
            now_millis() + DAY_MILLIS,
        ),
    ];

    for promotion in &promotions {
        let case = format!("{} in {}", promotion.code, promotion.catalog);
        let (status, body) = emulator.redeem(&promotion.code).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first redemption must succeed, got {status} with {body:?}"
        );
        // Every repeat is refused the same way, however many times it is tried.
        for attempt in 2..=3 {
            refused(
                &emulator,
                &format!("{case} attempt {attempt}"),
                &promotion.code,
                ALREADY_REDEEMED_MESSAGE,
            )
            .await;
        }
    }

    let credits = emulator.credits("once only").await;
    assert_eq!(
        credits.len(),
        promotions.len(),
        "the ledger must hold one credit per code redeemed, got {credits:?}"
    );
}

/// RULE: only a code this emulator issued to the CALLING account names a promotion to
/// redeem; every other code is refused as invalid and mints nothing.
#[tokio::test]
async fn only_a_code_issued_to_the_calling_account_is_redeemable() {
    let emulator = Emulator::new();
    let mine = emulator.issue_code(
        "AWS",
        0,
        "PROMOFORMETOO1",
        ACCOUNT_ID,
        100_000,
        now_millis() + 30 * DAY_MILLIS,
    );
    let theirs = emulator.issue_code(
        "AWS",
        1,
        "PROMOFORSOMEBODYELSE",
        OTHER_ACCOUNT_ID,
        100_000,
        now_millis() + 30 * DAY_MILLIS,
    );

    // Rows: a code nothing issued, the lower-case and padded forms of a code that WAS
    // issued (a code is its exact text), a prefix of one, a code issued to another
    // account, and a code of the maximum modeled length that names no promotion.
    let rows: Vec<(String, String)> = vec![
        ("a code nothing issued".to_string(), "ZZZZZZZZZZZZZZ".to_string()),
        (
            "the lower-case form of an issued code".to_string(),
            mine.code.to_lowercase(),
        ),
        (
            "an issued code with a trailing space".to_string(),
            format!("{} ", mine.code),
        ),
        (
            "a prefix of an issued code".to_string(),
            mine.code[..mine.code.len() - 1].to_string(),
        ),
        ("a code issued to another account".to_string(), theirs.code.clone()),
        (
            "a code at the modeled maximum length".to_string(),
            "9".repeat(256),
        ),
    ];

    for (case, code) in rows {
        refused(&emulator, &case, &code, NOT_ISSUED_MESSAGE).await;
    }

    // The codes themselves are untouched: a refused redemption spends nothing.
    assert_eq!(
        emulator.issued_code_status("mine", &mine).await,
        partner_state::ALLOCATION_STATUS_ACTIVE,
        "a refused redemption must leave the issued code redeemable"
    );
    assert_eq!(
        emulator.issued_code_status("theirs", &theirs).await,
        partner_state::ALLOCATION_STATUS_ACTIVE,
        "a refused redemption must leave another account's code redeemable"
    );

    // And the code that IS this account's own still redeems.
    let (status, body) = emulator.redeem(&mine.code).await;
    assert_eq!(
        status, 200,
        "the account's own code must still redeem, got {status} with {body:?}"
    );
}

/// RULE: a code whose validity period has closed is no longer redeemable — the issuer
/// reports it expired whether or not anybody looked at it, and redeeming it mints
/// nothing.
#[tokio::test]
async fn a_code_past_its_validity_period_is_not_redeemable() {
    // Rows: closed a minute ago, closed a month ago, and closed at the instant the
    // period names (the published bound: `ExpiresAt` is when the code "expires and can
    // no longer be redeemed").
    let rows = [
        ("closed a minute ago", "PROMOEXPIRED01", -60_000_i64),
        ("closed a month ago", "PROMOEXPIRED02", -30 * DAY_MILLIS),
        ("closed at this instant", "PROMOEXPIRED03", 0),
    ];

    for (case, code, offset_millis) in rows {
        let emulator = Emulator::new();
        let promotion = emulator.issue_code(
            "AWS",
            0,
            code,
            ACCOUNT_ID,
            100_000,
            now_millis() + offset_millis,
        );
        refused(&emulator, case, &promotion.code, EXPIRED_MESSAGE).await;
        assert_eq!(
            emulator.issued_code_status(case, &promotion).await,
            partner_state::ALLOCATION_STATUS_INACTIVE,
            "case {case:?}: the issuer must report a code past its validity period as expired"
        );
    }

    // And one still inside its period, for the same table: the bound is what decides.
    let emulator = Emulator::new();
    let promotion = emulator.issue_code(
        "AWS",
        0,
        "PROMOSTILLVALID",
        ACCOUNT_ID,
        100_000,
        now_millis() + Duration::from_secs(600).as_millis() as i64,
    );
    let (status, body) = emulator.redeem(&promotion.code).await;
    assert_eq!(
        status, 200,
        "case \"inside its validity period\": expected 200, got {status} with {body:?}"
    );
}

/// RULE: a `promoCode` outside the constraints the model states is refused at the request
/// boundary — in the modeled `ValidationException` shape — and mints nothing.
#[tokio::test]
async fn a_promo_code_outside_its_modeled_constraints_is_refused() {
    let emulator = Emulator::new();
    // The modeled `PromoCode` is `@length(1..256)` with pattern `[^<>"&\%|'\n\t]*`: the
    // rows are one below the minimum, one above the maximum, and one carrying a
    // character the pattern excludes.
    let rows = [
        ("the empty code", String::new()),
        ("one character past the maximum", "9".repeat(257)),
        ("a character the pattern excludes", "<PROMO>".to_string()),
    ];

    for (case, code) in rows {
        let before = emulator.credits(case).await;
        let (status, body) = emulator.redeem(&code).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        let after = emulator.credits(case).await;
        assert_eq!(
            credit_ids(&before),
            credit_ids(&after),
            "case {case:?}: a refused request must mint nothing, got {after:?}"
        );
    }
}

/// RULE: a redemption the ledger has no room for consumes nothing — the emulator's own
/// resource bound on the ledger ([`billing_state::MAX_CREDITS`]) is answered with the
/// modeled server error, and the code stays redeemable for when there is room.
#[tokio::test]
async fn a_redemption_the_ledger_has_no_room_for_consumes_nothing() {
    let emulator = Emulator::new();
    let promotion = emulator.issue_code(
        "AWS",
        0,
        "PROMOLEDGERFULL",
        ACCOUNT_ID,
        100_000,
        now_millis() + 30 * DAY_MILLIS,
    );
    // The ledger holds AWS's own credits too — they are minted by billing systems this
    // emulator does not run — so it is filled here to its bound, the way those systems
    // would, leaving no room for one more.
    let state = billing_state::service_state(&*emulator.engine)
        .expect("the in-memory engine carries the billing store's state");
    {
        let mut store = state.lock();
        for index in 0..billing_state::MAX_CREDITS {
            let credit_id = format!("9{index:0>6}");
            let stored = store.insert_credit(billing_state::Credit {
                credit_id,
                account_id: ACCOUNT_ID.to_string(),
                credit_type: "Refund".to_string(),
                initial_amount: billing_state::CreditAmount {
                    currency_code: CURRENCY_CODE.to_string(),
                    currency_amount: "1.00".to_string(),
                },
                remaining_amount: billing_state::CreditAmount {
                    currency_code: CURRENCY_CODE.to_string(),
                    currency_amount: "1.00".to_string(),
                },
                estimated_amount: None,
                applicable_product_names: Vec::new(),
                description: "a credit AWS's billing systems issued".to_string(),
                start_date: SystemTime::now(),
                end_date: None,
                exhaust_date: None,
                application_type: None,
                shareable_accounts: Vec::new(),
                account_has_credit_sharing_enabled: None,
                credit_console_visibility: None,
                credit_sharing_type: None,
                cost_category_arn: None,
                rule_name: None,
                credit_status: Some(CREDIT_STATUS_ENABLED.to_string()),
                purchase_type_applications: Vec::new(),
            });
            assert!(
                stored,
                "case \"filling the ledger\": entry {index} must be stored, up to the bound"
            );
        }
    }

    let (status, body) = emulator.redeem(&promotion.code).await;
    assert_eq!(
        status, 500,
        "case \"a full ledger\": expected the modeled server error, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "InternalServerException",
        "case \"a full ledger\": expected InternalServerException, got {body:?}"
    );
    assert_eq!(
        state.lock().credits.len(),
        billing_state::MAX_CREDITS,
        "case \"a full ledger\": the refused redemption must add no credit"
    );
    assert_eq!(
        emulator
            .issued_code_status("a full ledger", &promotion)
            .await,
        partner_state::ALLOCATION_STATUS_ACTIVE,
        "case \"a full ledger\": a refused redemption must leave the code redeemable"
    );
}

/// RULE: the millisecond scale a promotional credit code is issued and expires on reads as an
/// instant of this store for EVERY figure it can carry — the scale is `i64`, and a figure this
/// platform's instant cannot be advanced to saturates rather than ending the request — so no
/// figure a code carries can stop the redemption that reads it from being served.
///
/// The rows span the scale: both of its bounds, the epoch itself, the figures either side of it,
/// and an ordinary instant, which is the one the conversion round-trips.
#[tokio::test]
async fn every_figure_the_millisecond_scale_carries_reads_as_an_instant() {
    let day: i64 = DAY_MILLIS;
    // The figures whose instant the scale fixes exactly: the epoch, the millisecond after it,
    // an ordinary instant, and the figures before the epoch — which name no instant of this
    // store and read as the epoch itself.
    let rows: &[(&str, i64, SystemTime)] = &[
        ("the epoch", 0, UNIX_EPOCH),
        ("one millisecond after the epoch", 1, UNIX_EPOCH + Duration::from_millis(1)),
        ("an ordinary instant", 30 * day, UNIX_EPOCH + Duration::from_millis(30 * day as u64)),
        ("one millisecond before the epoch", -1, UNIX_EPOCH),
        ("the lower bound of the scale", i64::MIN, UNIX_EPOCH),
    ];
    for (case, millis, expected) in rows {
        let read = billing_state::instant_from_millis(*millis);
        assert_eq!(
            read, *expected,
            "case {case:?}: expected {expected:?} for {millis}, got {read:?}"
        );
    }
    // And at the top of the scale, where the figure may be past what this platform's instant
    // can hold: every one of them still READS as an instant of this store — at or after the
    // epoch — rather than ending the call.
    for (case, millis) in [
        ("the upper bound of the scale", i64::MAX),
        ("a figure a thousand years of milliseconds past it", i64::MAX / 1_000),
        ("the largest whole second of the scale", i64::MAX - (i64::MAX % 1_000)),
    ] {
        let read = billing_state::instant_from_millis(millis);
        assert!(
            read >= UNIX_EPOCH,
            "case {case:?}: expected an instant at or after the epoch for {millis}, got {read:?}"
        );
    }

    // And a code carrying such a figure is REDEEMED rather than ending the call: the credit it
    // mints is in the ledger afterwards.
    let emulator = Emulator::new();
    let promotion = emulator.issue_code(
        "AWS",
        0,
        "PROMOUNBOUNDEDEXPIRY",
        ACCOUNT_ID,
        100_000,
        i64::MAX,
    );
    let before = emulator.credits("an unbounded expiry").await;
    let (status, body) = emulator.redeem(&promotion.code).await;
    assert_eq!(
        status, 200,
        "case \"an unbounded expiry\": expected the redemption to be served, got {status} with \
         {body:?}"
    );
    let after = emulator.credits("an unbounded expiry").await;
    assert_eq!(
        credit_ids(&after).len(),
        credit_ids(&before).len() + 1,
        "case \"an unbounded expiry\": the redemption must add the credit it answered with, got \
         {after:?}"
    );
}
