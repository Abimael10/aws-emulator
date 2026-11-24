// SPDX-License-Identifier: CC0-1.0

//! The `CreationTime` a custom line item VERSION reports, end to end over the wire:
//! `CreateBillingGroup` -> `CreateCustomLineItem` -> `UpdateCustomLineItem` /
//! `DeleteCustomLineItem` -> `ListCustomLineItemVersions` / `ListCustomLineItems`.
//!
//! RULE: an element that reports one GENERATION of a resource reports that generation's
//! own instants, so `CustomLineItemVersionListElement$CreationTime` — "the time when the
//! custom line item version was created"
//! (`smithy_query(query="doc",shape="CustomLineItemVersionListElement",member="CreationTime")`)
//! — is the instant the write that OPENED that generation landed, which is the instant it
//! took effect (`StartTime`). Only the generation the create opened was created when the
//! ITEM was, so only its instant is the one the reads of the item itself report
//! (`CustomLineItemListElement$CreationTime`, "the time created"); a write that opens a
//! new generation leaves every earlier generation's instant where it was, and a write
//! that does not open one (a request applied to exactly the periods the generation in
//! effect holds, a bounded delete that only narrows it, a request that changes nothing)
//! leaves the instant that generation already had.
//!
//! The rows span what the rule generalises over rather than one recorded call: how many
//! writes the item has taken (none, one, two), whether a write opens a generation or
//! replaces the one in effect, which operation makes the write (update, delete), whether
//! the write names a billing period range at all and whether that range has an exclusive
//! end, and whether the write changed the item at all.
//!
//! A create happens NOW, so a world in which the item was created in a closed billing
//! period — the one world that tells an item-wide instant apart from a generation's at
//! all — is set up by backdating the stored record afterwards, the one thing these tests
//! cannot ask the service for. Every write under test is then a real call over the wire.

use crate::pagination_token_opacity::period;
use crate::pricing_rule_association_periods::period_start_millis;
use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::{
    ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// An AWS-managed pricing plan every billing group here computes with: the
/// `PricingPlanFullArn` pattern enumerates it, so no plan has to be created first
/// (`smithy_query(query="shape",shape="PricingPlanFullArn")`).
const MANAGED_PRICING_PLAN: &str = "arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan";

/// A live emulator holding only this service, with the engine kept so a row can set up a
/// world a write cannot ask for.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    /// A router over ONE shared engine, with the emulated account designated the
    /// organization's PAYER account — the world in which this service's control plane is
    /// reachable at all (the denial every other caller gets is asserted in
    /// `tests/all/payer_account_gate.rs`).
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        engine
            .state::<BillingConductorState>()
            .lock()
            .payer_account_id = Some(::engine::emulator_account_id().to_string());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send a JSON body to one of this service's routes and return the status and the
    /// parsed body.
    async fn send(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://billingconductor.us-east-1.amazonaws.com{path}"
            ))
            .header("host", "billingconductor.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/billingconductor/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create a billing group and return its ARN.
    async fn billing_group(&self, case: &str) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/create-billing-group",
                &json!({
                    "Name": "payer",
                    "AccountGrouping": { "LinkedAccountIds": [] },
                    "ComputationPreference": { "PricingPlanArn": MANAGED_PRICING_PLAN },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the billing group create to answer HTTP 200, got {status} \
             with {body}"
        );
        member_str(case, &body, "Arn")
    }

    /// Create a custom line item applying from the PREVIOUS billing period on — the
    /// earliest period a create may put an item into — and return its ARN.
    async fn custom_line_item(&self, case: &str, group_arn: &str) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": "original",
                    "Description": "the original custom line item",
                    "BillingGroupArn": group_arn,
                    "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 1.0 } },
                    "BillingPeriodRange": { "InclusiveStartBillingPeriod": period(-1) },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the item create to answer HTTP 200, got {status} with {body}"
        );
        member_str(case, &body, "Arn")
    }

    /// One write under test, through the real wire, which this case requires to succeed.
    async fn write_ok(&self, case: &str, path: &str, arn: &str, request: &Value) {
        let mut body = json!({ "Arn": arn });
        for (member, value) in object(request) {
            body[member] = value;
        }
        let (status, response) = self.send("POST", path, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the write to {path} to answer HTTP 200, got {status} with \
             {response}"
        );
    }

    /// The versions `ListCustomLineItemVersions` reports for the item, oldest first.
    async fn versions(&self, case: &str, arn: &str) -> Vec<Value> {
        let (status, body) = self
            .send(
                "POST",
                "/list-custom-line-item-versions",
                &json!({ "Arn": arn }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the version read to answer HTTP 200, got {status} with {body}"
        );
        body.get("CustomLineItemVersions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The ITEM as `ListCustomLineItems` reports it for `billing_period`.
    async fn item(&self, case: &str, arn: &str, billing_period: &str) -> Value {
        let (status, body) = self
            .send(
                "POST",
                "/list-custom-line-items",
                &json!({ "BillingPeriod": billing_period, "Filters": { "Arns": [arn] } }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the item read to answer HTTP 200, got {status} with {body}"
        );
        body.get("CustomLineItems")
            .and_then(Value::as_array)
            .and_then(|elements| elements.first().cloned())
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the item to be reported for {billing_period}")
            })
    }

    /// Backdate the item's CREATION to `at`, as if the creating call had been made then:
    /// the item, and the single generation that create opened, both came into being at
    /// that instant. The wire cannot ask for it — a create happens now.
    fn created_at(&self, item: &str, at: i64) {
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let record = state
            .custom_line_items
            .get_mut(item)
            .expect("setup created_at: the item was created above");
        record.creation_time_millis = at;
        record.last_modified_time_millis = at;
        record.version_start_time_millis = at;
    }

    /// Backdate the write that opened the item's CURRENT generation to `at`, leaving the
    /// item's own creation instant and the generations already frozen alone: how a world
    /// whose generations came into being at KNOWN, distinct instants is set up, which the
    /// wire cannot ask for either.
    fn generation_opened_at(&self, item: &str, at: i64) {
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let record = state
            .custom_line_items
            .get_mut(item)
            .expect("setup generation_opened_at: the item was created above");
        record.version_start_time_millis = at;
    }
}

/// The members of a JSON object, or none when it is not one.
fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// A string member a response is required to carry.
fn member_str(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {body}"))
        .to_string()
}

/// An `Instant` member an element is required to carry, in epoch millis.
fn instant(case: &str, element: &Value, member: &str) -> i64 {
    element
        .get(member)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {element}"))
}

/// `POST /update-custom-line-item`.
const UPDATE_PATH: &str = "/update-custom-line-item";

/// `POST /delete-custom-line-item`.
const DELETE_PATH: &str = "/delete-custom-line-item";

/// A billing period range as a request body member spells one: the start inclusive, the
/// end exclusive and omitted when the range is open-ended.
fn period_range(start: &str, end: Option<&str>) -> Value {
    let mut range = json!({ "InclusiveStartBillingPeriod": start });
    if let Some(end) = end {
        range["ExclusiveEndBillingPeriod"] = json!(end);
    }
    range
}

/// One row of the table: the case, the writes to make in order (the route and the body
/// members beside `Arn`), and whether each version expected afterwards — oldest first —
/// was opened by a LATER write rather than by the create.
type Row = (
    &'static str,
    Vec<(&'static str, Value)>,
    Vec<GenerationOrigin>,
);

/// What opened the generation a reported version is: the create that minted the item, or
/// a later write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationOrigin {
    /// The create — so the generation came into being when the ITEM did.
    TheCreate,
    /// A write made after the create, which happened later than the backdated create.
    ALaterWrite,
}

/// THE RULE: each reported version carries the instant the write that opened THAT
/// generation landed, which is the instant it took effect — the item's own creation
/// instant only for the generation the create opened.
#[tokio::test]
async fn each_version_reports_the_instant_its_own_generation_came_into_being() {
    let rows: Vec<Row> = vec![
        (
            "no write at all: the item has only the generation its create opened",
            vec![],
            vec![GenerationOrigin::TheCreate],
        ),
        (
            "an update applied from the current billing period opens a generation",
            vec![(
                UPDATE_PATH,
                json!({ "BillingPeriodRange": period_range(&period(0), None) }),
            )],
            vec![GenerationOrigin::TheCreate, GenerationOrigin::ALaterWrite],
        ),
        (
            "an update naming no billing period range replaces the generation in effect",
            vec![(UPDATE_PATH, json!({ "Name": "renamed" }))],
            vec![GenerationOrigin::TheCreate],
        ),
        (
            "an update applied from the generation's own start period replaces it",
            vec![(
                UPDATE_PATH,
                json!({
                    "Description": "restated where it already applied",
                    "BillingPeriodRange": period_range(&period(-1), None),
                }),
            )],
            vec![GenerationOrigin::TheCreate],
        ),
        (
            "an update that changes nothing leaves the generation it found",
            vec![(UPDATE_PATH, json!({ "Name": "original" }))],
            vec![GenerationOrigin::TheCreate],
        ),
        (
            "a bounded delete narrows the one generation without opening another",
            vec![(
                DELETE_PATH,
                json!({ "BillingPeriodRange": period_range(&period(0), None) }),
            )],
            vec![GenerationOrigin::TheCreate],
        ),
        (
            "a rename in place and then a write that opens a generation",
            vec![
                (UPDATE_PATH, json!({ "Name": "renamed" })),
                (
                    UPDATE_PATH,
                    json!({ "BillingPeriodRange": period_range(&period(0), None) }),
                ),
            ],
            vec![GenerationOrigin::TheCreate, GenerationOrigin::ALaterWrite],
        ),
        (
            "a delete after a generation was opened leaves both instants alone",
            vec![
                (
                    UPDATE_PATH,
                    json!({ "BillingPeriodRange": period_range(&period(0), None) }),
                ),
                (
                    DELETE_PATH,
                    json!({
                        "BillingPeriodRange": period_range(&period(0), Some(&period(1))),
                    }),
                ),
            ],
            vec![GenerationOrigin::TheCreate, GenerationOrigin::ALaterWrite],
        ),
    ];

    for (case, writes, expected) in rows {
        let emulator = Emulator::new();
        let group = emulator.billing_group(case).await;
        let item = emulator.custom_line_item(case, &group).await;
        // The world the rule needs: an item created in a CLOSED billing period, so the
        // instant the item came into being is far from the instant any write here lands.
        let created = period_start_millis(&period(-1));
        emulator.created_at(&item, created);
        for (path, body) in &writes {
            emulator.write_ok(case, path, &item, body).await;
        }

        let reported = emulator.versions(case, &item).await;
        assert_eq!(
            reported.len(),
            expected.len(),
            "case {case:?}: expected {} versions, got {reported:?}",
            expected.len()
        );
        let mut previous = 0;
        for (index, (version, origin)) in reported.iter().zip(expected.iter()).enumerate() {
            let creation_time = instant(case, version, "CreationTime");
            let start_time = instant(case, version, "StartTime");
            assert_eq!(
                creation_time, start_time,
                "case {case:?}: expected version {index} to be created at the instant it took \
                 effect, got creation {creation_time} and start {start_time}"
            );
            match origin {
                GenerationOrigin::TheCreate => assert_eq!(
                    creation_time, created,
                    "case {case:?}: expected version {index} to carry the instant the create \
                     opened it ({created}), got {creation_time}"
                ),
                GenerationOrigin::ALaterWrite => assert!(
                    creation_time > created,
                    "case {case:?}: expected version {index} to carry the instant the later \
                     write opened it, after {created}, got {creation_time}"
                ),
            }
            assert!(
                creation_time >= previous,
                "case {case:?}: expected the versions ordered by the instant they came into \
                 being, got version {index} at {creation_time} after {previous}"
            );
            previous = creation_time;
        }

        // The ITEM's own `CreationTime` is "the time created" and stays the instant the
        // item was created, whichever generation a read of a billing period reports.
        let listed = emulator.item(case, &item, &period(-1)).await;
        assert_eq!(
            instant(case, &listed, "CreationTime"),
            created,
            "case {case:?}: expected the item's own creation instant to stay {created}, got \
             {listed}"
        );
    }
}

/// THE RULE on a FROZEN generation: it keeps the instant it came into being, so two
/// generations of one item report two different creation instants and a later write to
/// the item moves neither.
#[tokio::test]
async fn a_frozen_generation_keeps_the_instant_it_came_into_being() {
    let case = "two generations that came into being at known, distinct instants";
    let emulator = Emulator::new();
    let group = emulator.billing_group(case).await;
    let item = emulator.custom_line_item(case, &group).await;
    // Both instants are KNOWN, so each version's is asserted exactly: the create's is
    // backdated into the closed billing period the item applies from, and the write that
    // opens the second generation is backdated to the start of the current one.
    let created = period_start_millis(&period(-1));
    let opened = period_start_millis(&period(0));
    emulator.created_at(&item, created);
    emulator
        .write_ok(
            case,
            UPDATE_PATH,
            &item,
            &json!({
                "Description": "the second generation",
                "BillingPeriodRange": period_range(&period(0), None),
            }),
        )
        .await;
    emulator.generation_opened_at(&item, opened);

    let reported = emulator.versions(case, &item).await;
    assert_eq!(
        reported.len(),
        2,
        "case {case:?}: expected the frozen generation and the one in effect, got {reported:?}"
    );
    let instants: Vec<(i64, i64)> = reported
        .iter()
        .map(|version| {
            (
                instant(case, version, "CreationTime"),
                instant(case, version, "StartTime"),
            )
        })
        .collect();
    assert_eq!(
        instants,
        vec![(created, created), (opened, opened)],
        "case {case:?}: expected each generation to report the instant it came into being, got \
         {reported:?}"
    );

    // A write that opens no generation leaves both instants where they are: a bounded
    // delete narrows the generation in effect, and the frozen one is not touched at all.
    emulator
        .write_ok(
            case,
            DELETE_PATH,
            &item,
            &json!({ "BillingPeriodRange": period_range(&period(0), Some(&period(1))) }),
        )
        .await;
    let after = emulator.versions(case, &item).await;
    let after_instants: Vec<(i64, i64)> = after
        .iter()
        .map(|version| {
            (
                instant(case, version, "CreationTime"),
                instant(case, version, "StartTime"),
            )
        })
        .collect();
    assert_eq!(
        after_instants, instants,
        "case {case:?}: expected the narrowing delete to leave every generation's instant where \
         it was, got {after:?}"
    );

    // The instant the ITEM was created is the first generation's and not the second's, so
    // the reads of the item itself and the version history do not report one instant for
    // the other's member.
    let listed = emulator.item(case, &item, &period(-1)).await;
    assert_eq!(
        instant(case, &listed, "CreationTime"),
        created,
        "case {case:?}: expected the item's own creation instant, got {listed}"
    );
    assert_ne!(
        instants[1].0, created,
        "case {case:?}: expected the generation a later write opened to carry its OWN instant, \
         got the item's in {reported:?}"
    );
}
