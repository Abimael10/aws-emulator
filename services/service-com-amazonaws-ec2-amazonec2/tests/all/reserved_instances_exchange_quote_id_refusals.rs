// SPDX-License-Identifier: CC0-1.0

//! The ONE answer-order-and-wording rule BOTH halves of the reserved-instances exchange-quote
//! pair follow for the `reservedInstanceId` member, pinned over the pair at once
//! (`GetReservedInstancesExchangeQuote` and `AcceptReservedInstancesExchangeQuote`, driven over
//! the `ec2Query` wire through the crate's shared [`super::ec2_query`] harness).
//!
//! The rule, in one sentence: a `reservedInstanceId` the request carries is read in two steps
//! and the `DryRun` probe is answered ahead of both — a value outside the Reserved Instances
//! id form names nothing in any account and is refused HTTP 400
//! `InvalidReservedInstancesID.NotFound` with EC2's malformed-id sentence around the value the
//! REQUEST sent, a value inside that form the account holds no purchase under is refused the
//! exchange API's own HTTP 400 `InvalidReservedInstanceId`, and "checks whether you have the
//! required permissions for the action, without actually making the request … the error
//! response is DryRunOperation" ($DryRun) means a probe of either value is answered HTTP 412
//! `DryRunOperation` with its wholly fixed sentence instead.
//!
//! The existing suites [`super::reserved_instances_exchange_quotes`] and
//! [`super::reserved_instances_exchanges`] cover one operation each; this one asserts the rule
//! is the SAME rule on both sides of the pair, so neither side can drift from it on its own.
//!
//! The rows differ along what the rule generalises over: which operation of the pair answers,
//! what makes the value unreadable as an id (no id form at all, another EC2 resource's id
//! form, a UUID field of the wrong length, a non-hexadecimal digit, a value carrying the XML
//! markup characters, a value longer than the echo bound), whether the value is instead a
//! well-formed id nothing is held under, where in the list the unreadable value sits, and
//! whether the probe flag is set, absent or explicitly false — so no row is the specification.

use std::time::SystemTime;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_RESERVED_INSTANCE_ID,
    INVALID_RESERVED_INSTANCES_ID_NOT_FOUND, MAX_ECHOED_RESOURCE_ID_CHARS,
    RESERVED_INSTANCE_STATE_ACTIVE, ReservedInstancesOfferingTerms, ReservedInstancesSpec,
    encode_reserved_instances_offering_id,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for: a Reserved Instance is reported in a zone of the
/// Region the request was signed for.
const REGION: &str = "us-east-1";

/// The form-field prefix ec2Query flattens `ReservedInstanceIds` into: the member's `@xmlName`
/// (`smithy_query(query="operation",operation="GetReservedInstancesExchangeQuote,AcceptReservedInstancesExchangeQuote")`
/// gives `ReservedInstanceId` for both).
const IDS: &str = "ReservedInstanceId";

/// The form-field prefix `TargetConfigurations` is flattened into, from the same place.
const TARGETS: &str = "TargetConfiguration";

/// A one-year term, in seconds — the units `ReservedInstances$Duration` is reported in.
const ONE_YEAR_SECONDS: i64 = 365 * 24 * 60 * 60;

/// The offering class a purchase and an offering must carry to be exchanged: the
/// `OfferingClassType` enum's `convertible` member.
const CONVERTIBLE: &str = "convertible";

/// The offering type every purchase and offering here carries.
const ALL_UPFRONT: &str = "All Upfront";

/// The product description every purchase and offering here carries.
const LINUX_UNIX: &str = "Linux/UNIX";

/// The two operations of the exchange-quote pair, as a client names them in `Action`. The rule
/// under test is one rule for both, so every row is driven through each of them.
const PAIR: [&str; 2] = [
    "GetReservedInstancesExchangeQuote",
    "AcceptReservedInstancesExchangeQuote",
];

/// EC2's dry-run code and its wholly fixed sentence — no part of it is built from the request.
const DRY_RUN_OPERATION: &str = "DryRunOperation";
const DRY_RUN_SENTENCE: &str = "Request would have succeeded, but DryRun flag is set.";

/// The `ReservedInstancesOfferingId` of one published Convertible offering, so that in every
/// row the `reservedInstanceId` is the only thing at fault.
fn published_offering_id() -> String {
    encode_reserved_instances_offering_id(&ReservedInstancesOfferingTerms {
        instance_type: "m5.large".to_string(),
        availability_zone: Some("us-east-1a".to_string()),
        product_description: LINUX_UNIX.to_string(),
        offering_class: CONVERTIBLE.to_string(),
        offering_type: ALL_UPFRONT.to_string(),
        duration_seconds: ONE_YEAR_SECONDS,
        fixed_price: 600.0,
        usage_price: 0.0,
    })
}

/// Seed one Convertible Reserved Instance that is in effect, answering the id it was minted
/// under — the value a probe that would otherwise have been served names.
fn seed(engine: &engine::InMemoryEngine) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_reserved_instances(ReservedInstancesSpec {
            instance_type: "m5.large".to_string(),
            availability_zone: Some("us-east-1a".to_string()),
            region: REGION.to_string(),
            instance_count: 2,
            duration_seconds: ONE_YEAR_SECONDS,
            offering_class: CONVERTIBLE.to_string(),
            offering_type: ALL_UPFRONT.to_string(),
            product_description: LINUX_UNIX.to_string(),
            state: RESERVED_INSTANCE_STATE_ACTIVE.to_string(),
            start: SystemTime::now(),
            fixed_price: 500.0,
            usage_price: 0.0,
        })
        .expect("the store has room for the one purchase this suite seeds")
}

/// One call of `action` naming `ids` and the one published offering, with `extra` form fields.
async fn exchange_call(
    router: &MultiServiceRouter,
    action: &str,
    ids: &[&str],
    extra: &[Param],
) -> (u16, String) {
    let mut params = indexed(IDS, ids);
    params.push(param(&format!("{TARGETS}.1.OfferingId"), &published_offering_id()));
    params.extend_from_slice(extra);
    call(router, REGION, action, &params).await
}

/// EC2's sentence for a reserved-instances id it cannot read as an id at all, around the value
/// the request sent: the fixed text is `The reserved-instances ID '` … `' is malformed`.
fn malformed_sentence(echoed: &str) -> String {
    format!("The reserved-instances ID '{echoed}' is malformed")
}

/// One row: the `reservedInstanceId` values a request carries, and the refusal the rule says
/// each operation of the pair answers for them when the probe flag is NOT set.
struct Row {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The values the request's `ReservedInstanceId.N` fields carry.
    ids: Vec<String>,
    /// The EC2 error code the refusal must carry.
    code: &'static str,
    /// The whole message the refusal must carry.
    message: String,
}

/// Every row of the table, built against the one seeded purchase's id.
fn rows() -> Vec<Row> {
    // A value far longer than any id EC2 issues: only the first MAX_ECHOED_RESOURCE_ID_CHARS
    // characters of a rejected value are echoed back, so the sentence quotes that much of it.
    let far_too_long = "x".repeat(4 * MAX_ECHOED_RESOURCE_ID_CHARS);
    // A well-formed Reserved Instances id — the hyphenated 8-4-4-4-12 form of a UUID — that
    // nothing is held under. Uppercase hexadecimal digits are part of that form, so this row
    // also pins that the FORM check reads a caller's value case-insensitively.
    let held_by_nobody = "7F4C1A20-9B3D-4E51-8A6F-2C0D5E7B9134";
    vec![
        Row {
            case: "a value of no id form at all",
            ids: vec!["not-a-reserved-instance-id".to_string()],
            code: INVALID_RESERVED_INSTANCES_ID_NOT_FOUND,
            message: malformed_sentence("not-a-reserved-instance-id"),
        },
        Row {
            case: "a value in another EC2 resource's id form",
            ids: vec!["ri-000000000000000000".to_string()],
            code: INVALID_RESERVED_INSTANCES_ID_NOT_FOUND,
            message: malformed_sentence("ri-000000000000000000"),
        },
        Row {
            case: "a UUID with a field of the wrong length",
            ids: vec!["7f4c1a20-9b3d-4e51-8a6f-2c0d5e7b913".to_string()],
            code: INVALID_RESERVED_INSTANCES_ID_NOT_FOUND,
            message: malformed_sentence("7f4c1a20-9b3d-4e51-8a6f-2c0d5e7b913"),
        },
        Row {
            case: "a UUID carrying a digit that is not hexadecimal",
            ids: vec!["7f4c1a20-9b3d-4e51-8a6f-2c0d5e7b913g".to_string()],
            code: INVALID_RESERVED_INSTANCES_ID_NOT_FOUND,
            message: malformed_sentence("7f4c1a20-9b3d-4e51-8a6f-2c0d5e7b913g"),
        },
        Row {
            // The value is echoed into an XML envelope, so a value carrying the markup
            // characters has to come back as the value the request sent rather than as markup.
            case: "a value carrying XML markup characters",
            ids: vec!["not-an-id-<&'\">".to_string()],
            code: INVALID_RESERVED_INSTANCES_ID_NOT_FOUND,
            message: malformed_sentence("not-an-id-<&'\">"),
        },
        Row {
            case: "a value longer than the echo bound",
            ids: vec![far_too_long],
            code: INVALID_RESERVED_INSTANCES_ID_NOT_FOUND,
            message: malformed_sentence(&"x".repeat(MAX_ECHOED_RESOURCE_ID_CHARS)),
        },
        Row {
            case: "an unreadable value after a well-formed one, refused before any lookup",
            ids: vec![
                held_by_nobody.to_string(),
                "not-a-reserved-instance-id".to_string(),
            ],
            code: INVALID_RESERVED_INSTANCES_ID_NOT_FOUND,
            message: malformed_sentence("not-a-reserved-instance-id"),
        },
        Row {
            case: "a well-formed id the account holds no purchase under",
            ids: vec![held_by_nobody.to_string()],
            code: INVALID_RESERVED_INSTANCE_ID,
            message: format!(" [{held_by_nobody}]"),
        },
    ]
}

/// Both halves of the pair refuse an unreadable `reservedInstanceId` with EC2's code and
/// sentence, and a well-formed one nothing is held under with the exchange API's own code.
#[tokio::test]
async fn both_halves_of_the_pair_word_the_id_refusal_the_same_way() {
    let (router, _engine) = super::ec2_query::emulator();

    for action in PAIR {
        for row in rows() {
            let case = format!("{action}: {}", row.case);
            let ids: Vec<&str> = row.ids.iter().map(String::as_str).collect();
            // The flag absent, and the flag explicitly false: the probe is answered only when
            // the caller asked for one, so both must draw the refusal the value itself earns.
            for (flag, extra) in [
                ("no DryRun field", Vec::new()),
                ("DryRun=false", vec![param("DryRun", "false")]),
            ] {
                let case = format!("{case} with {flag}");
                let (status, body) = exchange_call(&router, action, &ids, &extra).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with body {body}"
                );
                // The refusal envelope is XML, and the value it quotes is the caller's: a value
                // carrying markup characters must be encoded into it rather than break it.
                super::wire::document(&case, &body);
                assert_eq!(
                    error_code(&body),
                    row.code,
                    "case {case:?}: expected the code {expected:?}, got {actual:?}",
                    expected = row.code,
                    actual = error_code(&body)
                );
                assert_eq!(
                    error_message(&body),
                    row.message,
                    "case {case:?}: expected the message {expected:?}, got {actual:?}",
                    expected = row.message,
                    actual = error_message(&body)
                );
            }
        }
    }
}

/// The probe is answered AHEAD of every refusal the `reservedInstanceId` would otherwise draw,
/// on both halves of the pair: whatever the value names, a `DryRun` request answers HTTP 412
/// `DryRunOperation` with its fixed sentence, and nothing is written.
#[tokio::test]
async fn the_dry_run_probe_is_answered_ahead_of_the_id_refusal_on_both_halves() {
    let (router, engine) = super::ec2_query::emulator();
    let held = seed(&engine);
    let before = purchases(&engine);

    for action in PAIR {
        // Rows differ in what the probe names: a purchase the account really holds — the call
        // that would have been served — and each kind of value the operation would otherwise
        // refuse.
        let mut probes: Vec<(String, Vec<String>)> = vec![(
            "a request that would have been served".to_string(),
            vec![held.clone()],
        )];
        probes.extend(
            rows()
                .into_iter()
                .map(|row| (row.case.to_string(), row.ids)),
        );

        for (what, ids) in probes {
            let case = format!("{action}: {what}");
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let (status, body) =
                exchange_call(&router, action, &ids, &[param("DryRun", "true")]).await;
            assert_eq!(
                status, 412,
                "case {case:?}: expected HTTP 412 from a DryRun, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                DRY_RUN_OPERATION,
                "case {case:?}: expected the code {DRY_RUN_OPERATION:?}, got {actual:?}",
                actual = error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                DRY_RUN_SENTENCE,
                "case {case:?}: expected the message {DRY_RUN_SENTENCE:?}, got {actual:?}",
                actual = error_message(&body)
            );
        }
    }

    assert_eq!(
        purchases(&engine),
        before,
        "a probe must leave every purchase as it was, got {actual:?}",
        actual = purchases(&engine)
    );
}

/// Every Reserved Instance the store holds, as `(id, (state, instanceCount))` — what a probe
/// and a refusal must both leave untouched.
fn purchases(
    engine: &engine::InMemoryEngine,
) -> std::collections::BTreeMap<String, (Option<String>, Option<i32>)> {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    store
        .reserved_instances
        .iter()
        .map(|(id, held)| (id.clone(), (held.state.clone(), held.instance_count)))
        .collect()
}
