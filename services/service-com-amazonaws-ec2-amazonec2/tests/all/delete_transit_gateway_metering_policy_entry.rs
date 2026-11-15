// SPDX-License-Identifier: CC0-1.0

//! What `DeleteTransitGatewayMeteringPolicyEntry` records, driven over the wire this service
//! speaks (`ec2Query`).
//!
//! The rule under test is the one every EC2 delete of a numbered entry follows: one call takes
//! the entry the request names — the one the parent it names holds at that rule number, and no
//! other — OUT of the store and reports that record in the state its catalog gives an entry a
//! delete has removed, so the read of the parent's entries stops reporting it and the rule
//! number is free again; a request that names no entry of that parent is refused as an unusable
//! `PolicyRuleNumber` — an entry carries no id of its own — and one that names no parent at all
//! with that resource's own code, each removing nothing.
//!
//! Each row of the first table is a distinct input along what that rule generalises over: WHICH
//! rule number the entry carries (the lowest one a rule can take, a small one, a large one, and
//! the largest an `Integer` holds), which account its traffic is attributed to (every member of
//! the modelled payer catalog), and WHICH matching criteria it carries (none at all, a source
//! end only, a destination end only, both ends, a single port and a port range, an IPv4 and an
//! IPv6 block, a protocol number and every protocol) — the facts a delete must report back
//! unchanged apart from the state. The refusal table's rows are which member of the request is
//! unusable and how.
//!
//! The transit gateway and the metering policies these cases delete entries of are created over
//! the wire through `CreateTransitGateway` and `CreateTransitGatewayMeteringPolicy`, and the
//! entries through `CreateTransitGatewayMeteringPolicyEntry`, so every assertion is about what
//! the emulator itself stored.

use std::collections::BTreeMap;

use ec2::ops::types::{
    TRANSIT_GATEWAY_METERING_PAYER_TYPE_VALUES, TRANSIT_GATEWAY_METERING_POLICY_ENTRY_STATE_VALUES,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, INVALID_PARAMETER_VALUE,
    INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_MALFORMED,
    INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_NOT_FOUND,
    MIN_TRANSIT_GATEWAY_METERING_POLICY_RULE_NUMBER,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The ec2Query `Action` under test.
const ACTION: &str = "DeleteTransitGatewayMeteringPolicyEntry";

/// The create that stores the entry each case deletes.
const CREATE: &str = "CreateTransitGatewayMeteringPolicyEntry";

/// The read that reports a policy's stored entries back.
const GET: &str = "GetTransitGatewayMeteringPolicyEntries";

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// A member of `TransitGatewayMeteringPolicyEntryState`, from the modelled catalog
/// (`smithy_query(query="shape",shape="TransitGatewayMeteringPolicyEntryState")`, whose values
/// are `available | deleted`) rather than written out here.
fn entry_state(name: &str) -> &'static str {
    TRANSIT_GATEWAY_METERING_POLICY_ENTRY_STATE_VALUES
        .iter()
        .find(|value| **value == name)
        .unwrap_or_else(|| panic!("{name:?} is no TransitGatewayMeteringPolicyEntryState member"))
}

/// A member of `TransitGatewayMeteringPayerType`, from the modelled catalog
/// (`smithy_query(query="shape",shape="TransitGatewayMeteringPayerType")`, whose values are
/// `source-attachment-owner | destination-attachment-owner | transit-gateway-owner`).
fn payer_type(name: &str) -> &'static str {
    TRANSIT_GATEWAY_METERING_PAYER_TYPE_VALUES
        .iter()
        .find(|value| **value == name)
        .unwrap_or_else(|| panic!("{name:?} is no TransitGatewayMeteringPayerType member"))
}

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// The emulator with one transit gateway: the router to call and the engine to read the store
/// through, beside the gateway every metering policy below belongs to.
async fn seeded() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>, String) {
    let (router, engine) = super::ec2_query::emulator();
    let (status, body) = invoke(&router, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "CreateTransitGateway must succeed, got {status} with body {body}"
    );
    let gateway_id =
        element(&body, "transitGatewayId").expect("a created transit gateway reports its own id");
    (router, engine, gateway_id)
}

/// A metering policy of `gateway_id`, created over the wire, as its
/// `TransitGatewayMeteringPolicyId`.
async fn metering_policy(router: &MultiServiceRouter, gateway_id: &str) -> String {
    let (status, body) = invoke(
        router,
        "CreateTransitGatewayMeteringPolicy",
        &[param("TransitGatewayId", gateway_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateTransitGatewayMeteringPolicy must succeed, got {status} with body {body}"
    );
    element(&body, "transitGatewayMeteringPolicyId")
        .expect("a created metering policy reports its own id")
}

/// What a reported metering policy entry carries, read off the wire.
///
/// The whole record rather than a member at a time, so a delete can be asserted to report
/// exactly what the create stored — including the two instants, which a removal does not move.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    /// `policyRuleNumber`, the entry's own number as a string.
    rule_number: Option<String>,
    /// `meteredAccount`, the account the traffic is attributed to.
    metered_account: Option<String>,
    /// `state`, the entry's state.
    state: Option<String>,
    /// `updatedAt`, when the entry was last updated.
    updated_at: Option<String>,
    /// `updateEffectiveAt`, when its update takes effect.
    update_effective_at: Option<String>,
    /// The children of the entry's `meteringPolicyRule`, by element name.
    rule: BTreeMap<String, String>,
}

/// Every metering policy entry a response carries, in document order: the elements that have a
/// `policyRuleNumber` child, which is the one member every reported entry carries — so the one
/// entry a create or a delete answers and the `<item>`s of a read's set are read the same way.
fn reported_entries(case: &str, body: &str) -> Vec<Reported> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "policyRuleNumber").is_some())
        .map(|node| Reported {
            rule_number: child_text(node, "policyRuleNumber"),
            metered_account: child_text(node, "meteredAccount"),
            state: child_text(node, "state"),
            updated_at: child_text(node, "updatedAt"),
            update_effective_at: child_text(node, "updateEffectiveAt"),
            rule: child(node, "meteringPolicyRule")
                .map(|rule| {
                    rule.children()
                        .filter(roxmltree::Node::is_element)
                        .map(|member| {
                            (
                                member.tag_name().name().to_string(),
                                member.text().unwrap_or_default().to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// The ONE entry a response carries, or a panic naming the case.
fn reported_entry(case: &str, body: &str) -> Reported {
    let entries = reported_entries(case, body);
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected exactly one metering policy entry, got {count} in {body}",
        count = entries.len()
    );
    entries.into_iter().next().expect("one entry")
}

/// The rule numbers the store holds for `metering_policy_id`, in stored order.
fn stored_rule_numbers(engine: &engine::InMemoryEngine, metering_policy_id: &str) -> Vec<i32> {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_metering_policy_entries
        .get(metering_policy_id)
        .map(|entries| entries.keys().copied().collect())
        .unwrap_or_default()
}

/// The rule numbers the READ reports for `metering_policy_id`, in the order it reported them.
async fn read_rule_numbers(
    case: &str,
    router: &MultiServiceRouter,
    metering_policy_id: &str,
) -> Vec<String> {
    let (status, body) = invoke(
        router,
        GET,
        &[param("TransitGatewayMeteringPolicyId", metering_policy_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {GET} must succeed, got {status} with body {body}"
    );
    reported_entries(case, &body)
        .into_iter()
        .filter_map(|entry| entry.rule_number)
        .collect()
}

/// One entry a case deletes: the rule number and metered account it was created with, and each
/// traffic-matching criterion it carries.
struct Case {
    /// What the row is, for the assertion messages.
    name: &'static str,
    /// The `PolicyRuleNumber` the entry is created at and deleted by.
    rule_number: i32,
    /// The `MeteredAccount` the create names, a member of the modelled payer catalog.
    metered_account: &'static str,
    /// The `SourceCidrBlock` the create names.
    source_cidr: Option<&'static str>,
    /// The `SourcePortRange` the create names.
    source_port_range: Option<&'static str>,
    /// The `DestinationCidrBlock` the create names.
    destination_cidr: Option<&'static str>,
    /// The `DestinationPortRange` the create names.
    destination_port_range: Option<&'static str>,
    /// The `Protocol` the create names.
    protocol: Option<&'static str>,
}

/// The entry every row below starts from: a rule number and a metered account, no criteria.
const CLEAN: Case = Case {
    name: "",
    rule_number: 0,
    metered_account: "",
    source_cidr: None,
    source_port_range: None,
    destination_cidr: None,
    destination_port_range: None,
    protocol: None,
};

/// The deletes this suite is written against.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "the lowest rule number, with no matching criterion at all",
            rule_number: MIN_TRANSIT_GATEWAY_METERING_POLICY_RULE_NUMBER,
            metered_account: payer_type("source-attachment-owner"),
            ..CLEAN
        },
        Case {
            name: "a source end only, with one port and a protocol number",
            rule_number: 100,
            metered_account: payer_type("destination-attachment-owner"),
            source_cidr: Some("10.0.0.0/16"),
            source_port_range: Some("443"),
            protocol: Some("6"),
            ..CLEAN
        },
        Case {
            name: "a destination end only, with a port range",
            rule_number: 4096,
            metered_account: payer_type("transit-gateway-owner"),
            destination_cidr: Some("192.168.0.0/24"),
            destination_port_range: Some("1024-2048"),
            ..CLEAN
        },
        Case {
            name: "both ends, an IPv6 block at the source and every protocol",
            rule_number: 65535,
            metered_account: payer_type("source-attachment-owner"),
            source_cidr: Some("2001:db8::/32"),
            destination_cidr: Some("172.16.0.0/12"),
            destination_port_range: Some("0-65535"),
            protocol: Some("-1"),
            ..CLEAN
        },
        Case {
            name: "the largest rule number an Integer holds",
            rule_number: i32::MAX,
            metered_account: payer_type("transit-gateway-owner"),
            source_cidr: Some("10.1.2.0/24"),
            protocol: Some("17"),
            ..CLEAN
        },
    ]
}

/// The form fields one case's create carries, beside the policy and rule number it names.
fn create_params(metering_policy_id: &str, rule_number: i32, case: &Case) -> Vec<Param> {
    let mut params = vec![
        param("TransitGatewayMeteringPolicyId", metering_policy_id),
        param("PolicyRuleNumber", &rule_number.to_string()),
        param("MeteredAccount", case.metered_account),
    ];
    for (name, value) in [
        ("SourceCidrBlock", case.source_cidr),
        ("SourcePortRange", case.source_port_range),
        ("DestinationCidrBlock", case.destination_cidr),
        ("DestinationPortRange", case.destination_port_range),
        ("Protocol", case.protocol),
    ] {
        if let Some(value) = value {
            params.push(param(name, value));
        }
    }
    params
}

/// The rule number a case's neighbour entry takes: another entry of the SAME policy, so each
/// case also shows that a delete takes out the entry it names and no other.
fn neighbour_of(rule_number: i32) -> i32 {
    if rule_number == i32::MAX {
        rule_number - 1
    } else {
        rule_number + 1
    }
}

/// A delete reports the entry the store HELD, in the `deleted` state its catalog gives one a
/// delete has taken out of its policy, takes it out of the store and leaves every other entry
/// of that policy where it was — and the rule number it freed can be created at again, while a
/// second delete of it is refused.
///
/// One row per case, so no single request is the specification.
#[tokio::test]
async fn a_deleted_entry_is_reported_deleted_and_stops_resolving() {
    let (router, engine, gateway_id) = seeded().await;

    for case in cases() {
        let name = case.name;
        let metering_policy_id = metering_policy(&router, &gateway_id).await;
        let neighbour = neighbour_of(case.rule_number);

        // The entry under test, and a neighbour of the same policy that must survive the
        // delete.
        let (status, created_body) = invoke(
            &router,
            CREATE,
            &create_params(&metering_policy_id, case.rule_number, &case),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: {CREATE} must succeed, got {status} with body {created_body}"
        );
        let created = reported_entry(name, &created_body);
        let (status, body) = invoke(
            &router,
            CREATE,
            &create_params(&metering_policy_id, neighbour, &case),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: the neighbour create must succeed, got {status} with body {body}"
        );

        let delete_params = vec![
            param("TransitGatewayMeteringPolicyId", &metering_policy_id),
            param("PolicyRuleNumber", &case.rule_number.to_string()),
        ];
        let (status, body) = invoke(&router, ACTION, &delete_params).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
        );
        let deleted = reported_entry(name, &body);
        assert_eq!(
            deleted.state.as_deref(),
            Some(entry_state("deleted")),
            "case {name:?}: expected the deleted state, got {actual:?} in {body}",
            actual = deleted.state
        );
        // Everything else the entry carried is what the create stored: the rule number, the
        // account the traffic is attributed to, each matching criterion, and the two instants —
        // a removal updates none of them.
        assert_eq!(
            Reported {
                state: created.state.clone(),
                ..deleted.clone()
            },
            created,
            "case {name:?}: a delete must report the entry as it was stored, got {deleted:?} \
             against {created:?}"
        );

        // The store, and the read of it, hold only the neighbour from here on.
        assert_eq!(
            stored_rule_numbers(&engine, &metering_policy_id),
            vec![neighbour],
            "case {name:?}: the deleted entry must be gone from the store, which holds \
             {held:?}",
            held = stored_rule_numbers(&engine, &metering_policy_id)
        );
        let reported = read_rule_numbers(name, &router, &metering_policy_id).await;
        assert_eq!(
            reported,
            vec![neighbour.to_string()],
            "case {name:?}: the read must report only the neighbour, got {reported:?}"
        );

        // A second delete of the same entry is refused rather than answered as a deletion that
        // did nothing.
        let (status, body) = invoke(&router, ACTION, &delete_params).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {name:?}: expected HTTP {REFUSAL_STATUS} from a second delete, got {status} \
             with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_PARAMETER_VALUE,
            "case {name:?}: expected a rule number the policy holds nothing at to be refused as \
             an unusable parameter value, got {code:?} in {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, "Metering Policy Entry RuleNumber provided does not exists",
            "case {name:?}: expected the sentence AWS words for a rule number the policy holds \
             nothing at, got {message:?}"
        );

        // The rule number is free again: the delete removed the record, so a create may take
        // that number once more.
        let (status, body) = invoke(
            &router,
            CREATE,
            &create_params(&metering_policy_id, case.rule_number, &case),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: the rule number a delete freed must be creatable again, got \
             {status} with body {body}"
        );
        let mut held = stored_rule_numbers(&engine, &metering_policy_id);
        held.sort_unstable();
        let mut expected = vec![case.rule_number, neighbour];
        expected.sort_unstable();
        assert_eq!(
            held, expected,
            "case {name:?}: expected the policy to hold {expected:?} again, got {held:?}"
        );
    }
}

/// One refused delete: what the request names, and the answer it gets.
struct Refusal {
    /// What the row is, for the assertion messages.
    name: &'static str,
    /// The `TransitGatewayMeteringPolicyId` the request names, or the stored policy's.
    metering_policy_id: Option<&'static str>,
    /// The `PolicyRuleNumber` the request names.
    rule_number: i32,
    /// Whether the request asks for a dry run.
    dry_run: bool,
    /// The HTTP status the refusal carries.
    status: u16,
    /// The code the refusal carries.
    code: &'static str,
}

/// The stored entry every refusal row is asked against: the one the policy holds.
const STORED_RULE_NUMBER: i32 = MIN_TRANSIT_GATEWAY_METERING_POLICY_RULE_NUMBER;

/// The row every refusal below starts from: the stored policy, the stored entry, no dry run.
const CLEAN_REFUSAL: Refusal = Refusal {
    name: "",
    metering_policy_id: None,
    rule_number: STORED_RULE_NUMBER,
    dry_run: false,
    status: REFUSAL_STATUS,
    code: INVALID_PARAMETER_VALUE,
};

/// The refusals this suite is written against: four shapes of value that is not a metering
/// policy id, a well-formed id no policy of the account has, rule numbers below the lowest one,
/// rule numbers the policy holds no entry at, and a dry run — which is answered about
/// PERMISSION, so it wins even when the policy named cannot be resolved, but not over a request
/// whose own form is unusable.
fn refusals() -> Vec<Refusal> {
    vec![
        Refusal {
            name: "a policy id whose suffix is 16 hexadecimal digits",
            metering_policy_id: Some("tgw-mp-0123456789abcdef"),
            code: INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_MALFORMED,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a policy id whose suffix is not hexadecimal",
            metering_policy_id: Some("tgw-mp-not-hex"),
            code: INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_MALFORMED,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a value that is no resource id at all",
            metering_policy_id: Some("metering-policy-one"),
            code: INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_MALFORMED,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "an id of another resource of the same family",
            metering_policy_id: Some("tgw-attach-0123456789abcdef0"),
            code: INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_MALFORMED,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a well-formed policy id this account holds no policy for",
            metering_policy_id: Some("tgw-mp-0123456789abcdef0"),
            code: INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_NOT_FOUND,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a rule number below the lowest one",
            rule_number: MIN_TRANSIT_GATEWAY_METERING_POLICY_RULE_NUMBER - 1,
            code: INVALID_PARAMETER_VALUE,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a negative rule number",
            rule_number: -12,
            code: INVALID_PARAMETER_VALUE,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a rule number the policy holds no entry at",
            rule_number: STORED_RULE_NUMBER + 1,
            code: INVALID_PARAMETER_VALUE,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "the largest rule number an Integer holds, which the policy holds no entry at",
            rule_number: i32::MAX,
            code: INVALID_PARAMETER_VALUE,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a dry run of a delete that would have succeeded",
            dry_run: true,
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a dry run naming a policy this account holds none of",
            metering_policy_id: Some("tgw-mp-0123456789abcdef0"),
            dry_run: true,
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION,
            ..CLEAN_REFUSAL
        },
        Refusal {
            name: "a dry run whose rule number is below the lowest one",
            rule_number: MIN_TRANSIT_GATEWAY_METERING_POLICY_RULE_NUMBER - 1,
            dry_run: true,
            code: INVALID_PARAMETER_VALUE,
            ..CLEAN_REFUSAL
        },
    ]
}

/// A request this operation refuses answers that refusal's own code and removes NOTHING, so the
/// entry the policy held is still there afterwards.
#[tokio::test]
async fn an_unusable_delete_is_refused_and_removes_nothing() {
    let (router, engine, gateway_id) = seeded().await;
    let metering_policy_id = metering_policy(&router, &gateway_id).await;
    let stored = Case {
        name: "the stored entry",
        rule_number: STORED_RULE_NUMBER,
        metered_account: payer_type("transit-gateway-owner"),
        source_cidr: Some("10.0.0.0/8"),
        ..CLEAN
    };
    let (status, body) = invoke(
        &router,
        CREATE,
        &create_params(&metering_policy_id, STORED_RULE_NUMBER, &stored),
    )
    .await;
    assert_eq!(
        status, 200,
        "{CREATE} must succeed, got {status} with body {body}"
    );

    for refusal in refusals() {
        let name = refusal.name;
        let mut params = vec![
            param(
                "TransitGatewayMeteringPolicyId",
                refusal
                    .metering_policy_id
                    .unwrap_or(metering_policy_id.as_str()),
            ),
            param("PolicyRuleNumber", &refusal.rule_number.to_string()),
        ];
        if refusal.dry_run {
            params.push(param("DryRun", "true"));
        }

        let (status, body) = invoke(&router, ACTION, &params).await;
        assert_eq!(
            status, refusal.status,
            "case {name:?}: expected HTTP {expected} from {ACTION}, got {status} with body \
             {body}",
            expected = refusal.status
        );
        let code = error_code(&body);
        assert_eq!(
            code, refusal.code,
            "case {name:?}: expected the {expected:?} refusal, got {code:?} in {body}",
            expected = refusal.code
        );
        let held = stored_rule_numbers(&engine, &metering_policy_id);
        assert_eq!(
            held,
            vec![STORED_RULE_NUMBER],
            "case {name:?}: a refused delete must leave the policy's entry where it was, \
             holding {held:?}"
        );
    }
}

/// A rule number names one entry of ONE policy: the same number in another policy is a
/// different entry, so deleting it there leaves this one alone.
#[tokio::test]
async fn one_rule_number_names_one_entry_of_one_policy() {
    let (router, engine, gateway_id) = seeded().await;
    let first = metering_policy(&router, &gateway_id).await;
    let second = metering_policy(&router, &gateway_id).await;
    let shared = Case {
        name: "the entry both policies hold",
        rule_number: 7,
        metered_account: payer_type("source-attachment-owner"),
        destination_cidr: Some("10.9.0.0/16"),
        ..CLEAN
    };

    for policy_id in [&first, &second] {
        let (status, body) = invoke(
            &router,
            CREATE,
            &create_params(policy_id, shared.rule_number, &shared),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {policy_id:?}: {CREATE} must succeed, got {status} with body {body}"
        );
    }

    let (status, body) = invoke(
        &router,
        ACTION,
        &[
            param("TransitGatewayMeteringPolicyId", &first),
            param("PolicyRuleNumber", &shared.rule_number.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}",
        case = shared.name
    );

    assert!(
        stored_rule_numbers(&engine, &first).is_empty(),
        "case {case:?}: the first policy must hold no entry any more, holding {held:?}",
        case = shared.name,
        held = stored_rule_numbers(&engine, &first)
    );
    assert_eq!(
        stored_rule_numbers(&engine, &second),
        vec![shared.rule_number],
        "case {case:?}: the second policy's entry at the same rule number must survive, it \
         holds {held:?}",
        case = shared.name,
        held = stored_rule_numbers(&engine, &second)
    );
    let reported = read_rule_numbers(shared.name, &router, &second).await;
    assert_eq!(
        reported,
        vec![shared.rule_number.to_string()],
        "case {case:?}: the read of the second policy must still report its entry, got \
         {reported:?}",
        case = shared.name
    );
}
