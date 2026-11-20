// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteTransitGatewayPolicyTable`, driven over `ec2Query` (README.md
//! / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: deleting a transit gateway policy table starts that table's deletion
//! in the store — it is answered in the `deleting` member of its own state catalog, with the
//! clock owing it the terminal `deleted`, which is the REMOVAL of the table and of everything
//! stored as a part of it (its entries), so every later reader stops resolving that id and
//! the bytes those records held go back to the store's budget; a table an attachment is still
//! associated with, an id that resolves to no table of the account, a value that is no policy
//! table id at all, a request that leaves the id out and a dry run are each refused with
//! their own code while nothing is removed.
//!
//! Each row of the success table is a distinct input along what that rule generalises over:
//! how many tags the deleted table carries (none, one, several including an empty value, and
//! as many as EC2 documents per resource) and how many entries are stored in it (none through
//! three). The refusal rows are one per way a named policy table id can fail to resolve, one
//! for a table that is still associated, one for the id left out altogether, and one for a
//! dry run of a request that would have succeeded.

use std::time::Duration;

use ec2::ops::types::{
    RESOURCE_TYPE_VALUES, TRANSIT_GATEWAY_POLICY_TABLE_STATE_VALUES, TransitGatewayVpcAttachment,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, MAX_TAGS_PER_RESOURCE,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE, TransitGatewayAttachmentView,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, element, elements, error_code, indexed, unescaped};
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The state a delete puts a transit gateway policy table into. Pinned to the modelled
/// catalog by the assertion in [`a_delete_takes_the_table_and_its_entries_out_of_the_store`],
/// which requires it to be one of `TransitGatewayPolicyTableState`'s members.
const DELETING: &str = "deleting";

/// How long this suite is willing to wait for a move the clock owes: far longer than the
/// emulator's own span, so a loaded host cannot fail a row that is only slow.
const MOVE_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again while waiting for one.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One call of `action` signed for the Region above.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// The same call, requiring HTTP 200, and the body it answered.
async fn invoke_ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One `TransitGatewayPolicyTableId=` request member.
fn table_param(policy_table_id: &str) -> Vec<(String, String)> {
    vec![param("TransitGatewayPolicyTableId", policy_table_id)]
}

/// The `ResourceType` a `TagSpecifications` entry of the policy table create names, read out
/// of the modelled `ResourceType` catalog (`smithy_query(query="shape",shape="ResourceType")`)
/// rather than written out here.
fn policy_table_resource_type() -> &'static str {
    RESOURCE_TYPE_VALUES
        .iter()
        .find(|value| **value == "transit-gateway-policy-table")
        .expect("the modelled ResourceType catalog names the transit gateway policy table")
}

/// The `TagSpecifications` form fields that tag the policy table a create makes.
fn tag_params(tags: &[(String, String)]) -> Vec<(String, String)> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecifications.1.ResourceType",
        policy_table_resource_type(),
    )];
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        params.push(param(
            &format!("TagSpecifications.1.Tag.{position}.Key"),
            key,
        ));
        params.push(param(
            &format!("TagSpecifications.1.Tag.{position}.Value"),
            value,
        ));
    }
    params
}

/// A transit gateway created through the wire, as its id.
async fn transit_gateway(case: &str, router: &MultiServiceRouter) -> String {
    let body = invoke_ok(case, router, "CreateTransitGateway", &[]).await;
    element(&body, "transitGatewayId").expect("a created gateway reports its id")
}

/// An additional route table of `transit_gateway_id`, as its id: an entry of a policy table
/// names the route table its rule routes matching traffic to.
async fn route_table(case: &str, router: &MultiServiceRouter, transit_gateway_id: &str) -> String {
    let body = invoke_ok(
        case,
        router,
        "CreateTransitGatewayRouteTable",
        &[param("TransitGatewayId", transit_gateway_id)],
    )
    .await;
    element(&body, "transitGatewayRouteTableId").expect("a created route table reports its id")
}

/// A policy table of `transit_gateway_id` created through the wire with `tags`: its id and the
/// creation time the create reported for it.
async fn policy_table(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
    tags: &[(String, String)],
) -> (String, String) {
    let mut request = vec![param("TransitGatewayId", transit_gateway_id)];
    request.extend(tag_params(tags));
    let body = invoke_ok(case, router, "CreateTransitGatewayPolicyTable", &request).await;
    (
        element(&body, "transitGatewayPolicyTableId").expect("a created policy table reports its id"),
        element(&body, "creationTime").expect("a created policy table reports its creation time"),
    )
}

/// One entry of `policy_table_id` at `rule_number`, created through the wire.
async fn policy_table_entry(
    case: &str,
    router: &MultiServiceRouter,
    policy_table_id: &str,
    target_route_table_id: &str,
    rule_number: i32,
) {
    invoke_ok(
        case,
        router,
        "CreateTransitGatewayPolicyTableEntry",
        &[
            param("TransitGatewayPolicyTableId", policy_table_id),
            param("PolicyRuleNumber", &rule_number.to_string()),
            param("TargetRouteTableId", target_route_table_id),
            param("PolicyRule.SourceCidrBlock", "10.0.0.0/16"),
            param(
                "PolicyRule.DestinationCidrBlock",
                &format!("10.{rule_number}.0.0/16"),
            ),
        ],
    )
    .await;
}

/// One VPC attachment of `transit_gateway_id` seeded straight into the store, as its minted
/// id: the attachment-creating operations are their own issues, and an association needs an
/// attachment the store holds.
fn seed_attachment(
    engine: &engine::InMemoryEngine,
    transit_gateway_id: &str,
    vpc_id: &str,
) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_transit_gateway_attachment(TransitGatewayAttachmentView::Vpc(
            TransitGatewayVpcAttachment {
                transit_gateway_id: Some(transit_gateway_id.to_string()),
                vpc_id: Some(vpc_id.to_string()),
                vpc_owner_id: Some(engine::emulator_account_id().to_string()),
                state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
                ..Default::default()
            },
        ))
        .expect("the store holds the seeded attachment")
}

/// Everything about the store this suite compares before and after a call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    /// The policy tables the store holds, in its own stable order.
    policy_tables: Vec<String>,
    /// The (policy table, rule number) pairs of every entry it holds.
    entries: Vec<(String, i32)>,
    /// The policy tables the clock owes a removal.
    owed_removals: Vec<String>,
    /// The caller-supplied bytes the store has charged.
    stored_bytes: usize,
}

/// The store as it stands right now.
fn snapshot(engine: &engine::InMemoryEngine) -> Snapshot {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    Snapshot {
        policy_tables: store.transit_gateway_policy_tables.keys().cloned().collect(),
        entries: store
            .transit_gateway_policy_tables
            .iter()
            .flat_map(|(policy_table_id, record)| {
                record
                    .entries
                    .keys()
                    .map(move |rule_number| (policy_table_id.clone(), *rule_number))
            })
            .collect(),
        owed_removals: store
            .transit_gateway_policy_table_delete_at
            .keys()
            .cloned()
            .collect(),
        stored_bytes: store.stored_bytes(),
    }
}

/// What `DescribeTransitGatewayPolicyTables` answers for one policy table id.
async fn described(router: &MultiServiceRouter, policy_table_id: &str) -> (u16, String) {
    invoke(
        router,
        "DescribeTransitGatewayPolicyTables",
        &indexed("TransitGatewayPolicyTableIds", &[policy_table_id]),
    )
    .await
}

/// Look again until the describe of `policy_table_id` refuses it, or a generous deadline
/// passes.
///
/// The CONDITION is what is waited for, never a span of the clock, so the suite cannot depend
/// on how fast the host applies a removal the clock owes.
async fn poll_until_gone(case: &str, router: &MultiServiceRouter, policy_table_id: &str) {
    let deadline = tokio::time::Instant::now() + MOVE_DEADLINE;
    loop {
        let (status, body) = described(router, policy_table_id).await;
        if status == 400 {
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_TRANSIT_GATEWAY_POLICY_TABLE_ID_NOT_FOUND,
                "case {case:?}: a removed policy table must be refused with its own \
                 not-found code, got {body}"
            );
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {policy_table_id} must stop being reported once its removal falls \
             due, and the describe still answered {status} with body {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `count` distinct tags, for the row that asks for as many as EC2 documents per resource.
fn many_tags(count: usize) -> Vec<(String, String)> {
    (1..=count)
        .map(|nth| (format!("key-{nth}"), format!("value-{nth}")))
        .collect()
}

/// One (key, value) pair, as the table rows spell their tags.
fn tag(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One delete, as a row of the table.
struct Case {
    /// The case name every assertion of the row quotes.
    name: &'static str,
    /// The tags the deleted table was created with.
    tags: Vec<(String, String)>,
    /// How many entries are stored in the table when the delete arrives.
    entries: i32,
}

/// Deleting a policy table answers that table in `deleting`, and the clock then takes it and
/// its entries out of the store: their bytes go back to the budget, every later reader of
/// that id refuses it, and every other record of the account is left where it was.
#[tokio::test]
async fn a_delete_takes_the_table_and_its_entries_out_of_the_store() {
    assert!(
        TRANSIT_GATEWAY_POLICY_TABLE_STATE_VALUES.contains(&DELETING),
        "the state a delete answers with must be a member of the modelled catalog {:?}",
        TRANSIT_GATEWAY_POLICY_TABLE_STATE_VALUES
    );

    let cases = vec![
        Case {
            name: "no tags, holding no entry",
            tags: Vec::new(),
            entries: 0,
        },
        Case {
            name: "one tag, holding one entry",
            tags: vec![tag("Name", "policy")],
            entries: 1,
        },
        Case {
            name: "three tags, holding two entries",
            // An empty tag value is a value EC2 accepts, so one row carries it.
            tags: vec![tag("Env", "prod"), tag("Tier", "edge"), tag("Owner", "")],
            entries: 2,
        },
        Case {
            name: "as many tags as EC2 documents per resource, holding three entries",
            tags: many_tags(MAX_TAGS_PER_RESOURCE),
            entries: 3,
        },
    ];

    for case in cases {
        let Case {
            name,
            tags,
            entries,
        } = case;
        let (router, engine) = super::ec2_query::emulator();
        let transit_gateway_id = transit_gateway(name, &router).await;
        let target_route_table_id = route_table(name, &router, &transit_gateway_id).await;

        // ANOTHER policy table of the same gateway, holding an entry of its own: the delete
        // under test must leave it exactly where it is, so it is part of the baseline.
        let (untouched_table, _) = policy_table(name, &router, &transit_gateway_id, &[]).await;
        policy_table_entry(name, &router, &untouched_table, &target_route_table_id, 7).await;
        let before = snapshot(&engine);

        let (policy_table_id, created_at) =
            policy_table(name, &router, &transit_gateway_id, &tags).await;
        for rule_number in 1..=entries {
            policy_table_entry(
                name,
                &router,
                &policy_table_id,
                &target_route_table_id,
                rule_number,
            )
            .await;
        }

        let (status, body) = invoke(
            &router,
            "DeleteTransitGatewayPolicyTable",
            &table_param(&policy_table_id),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from the delete, got {status} with body {body}"
        );

        // The answer is the table the delete was asked for, as the store now holds it.
        assert_eq!(
            element(&body, "transitGatewayPolicyTableId").as_deref(),
            Some(policy_table_id.as_str()),
            "case {name:?}: expected the table the request named, got {body}"
        );
        assert_eq!(
            element(&body, "transitGatewayId").as_deref(),
            Some(transit_gateway_id.as_str()),
            "case {name:?}: expected the gateway the table belonged to, got {body}"
        );
        assert_eq!(
            element(&body, "state").as_deref(),
            Some(DELETING),
            "case {name:?}: expected the state of a delete, got {body}"
        );
        assert_eq!(
            element(&body, "creationTime").as_deref(),
            Some(created_at.as_str()),
            "case {name:?}: expected the creation time of the stored record, got {body}"
        );
        assert!(
            as_timestamp(&created_at).is_some(),
            "case {name:?}: expected a modelled timestamp, got {created_at:?}"
        );
        assert_eq!(
            elements(&body, "key"),
            tags.iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<String>>(),
            "case {name:?}: expected the tags the deleted table carried, got {body}"
        );

        // A read right after the delete corroborates what the delete answered: either the
        // table is still reported, and then in the state the delete put it into, or the
        // removal the clock owed it has already fallen due and the id resolves to nothing.
        // Which of the two it is depends on the clock, so the row asserts each alternative
        // rather than the timing.
        let (status, reported) = described(&router, &policy_table_id).await;
        if status == 200 {
            assert_eq!(
                element(&reported, "state").as_deref(),
                Some(DELETING),
                "case {name:?}: a table whose delete is under way must be reported in that \
                 state, got {reported}"
            );
        } else {
            assert_eq!(
                error_code(&reported),
                ec2::state::INVALID_TRANSIT_GATEWAY_POLICY_TABLE_ID_NOT_FOUND,
                "case {name:?}: a removed table must be refused with its own not-found code, \
                 got {reported}"
            );
        }

        // The removal the clock owes it lands without anybody asking again, and once it has,
        // the store holds exactly what it held before the table was created: the table, its
        // entries and their bytes are gone, and the other policy table is untouched.
        poll_until_gone(name, &router, &policy_table_id).await;
        let after = snapshot(&engine);
        assert_eq!(
            after, before,
            "case {name:?}: once the removal is due the store must hold what it held before \
             the table was created, expected {before:?}, got {after:?}"
        );

        // Every later reader of that id refuses it, so the removal is not just the answer of
        // one call — while the other policy table of the same gateway still reads back.
        for action in [
            "DeleteTransitGatewayPolicyTable",
            "GetTransitGatewayPolicyTableEntries",
            "GetTransitGatewayPolicyTableAssociations",
        ] {
            let (status, body) = invoke(&router, action, &table_param(&policy_table_id)).await;
            assert_eq!(
                status, 400,
                "case {name:?}: {action} must refuse the removed table, got {status} with \
                 body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_TRANSIT_GATEWAY_POLICY_TABLE_ID_NOT_FOUND,
                "case {name:?}: {action} must answer the policy table's own not-found code, \
                 got {body}"
            );
        }
        let body = invoke_ok(name, &router, "DescribeTransitGatewayPolicyTables", &[]).await;
        assert_eq!(
            elements(&body, "transitGatewayPolicyTableId"),
            vec![untouched_table.clone()],
            "case {name:?}: only the deleted table may be gone, got {body}"
        );
        let body = invoke_ok(
            name,
            &router,
            "GetTransitGatewayPolicyTableEntries",
            &table_param(&untouched_table),
        )
        .await;
        assert_eq!(
            elements(&body, "policyRuleNumber"),
            vec!["7".to_string()],
            "case {name:?}: the entry of the other policy table must survive the delete, got \
             {body}"
        );
    }
}

/// A policy table an attachment is still associated with, a policy table id that resolves to
/// nothing, a value that is no policy table id at all, a request that leaves the id out and a
/// dry run are each refused with their own documented code — never with a 501 or an empty
/// success — and nothing is removed.
#[tokio::test]
async fn a_delete_that_is_not_admissible_is_refused_and_removes_nothing() {
    let case = "the refusals";
    let (router, engine) = super::ec2_query::emulator();
    let transit_gateway_id = transit_gateway(case, &router).await;

    // A table an attachment is still associated with, and one nothing holds.
    let (associated_table, _) = policy_table(case, &router, &transit_gateway_id, &[]).await;
    let attachment_id = seed_attachment(&engine, &transit_gateway_id, "vpc-00000000000000002");
    invoke_ok(
        "the association that blocks the delete",
        &router,
        "AssociateTransitGatewayPolicyTable",
        &[
            param("TransitGatewayPolicyTableId", &associated_table),
            param("TransitGatewayAttachmentId", &attachment_id),
        ],
    )
    .await;
    let (free_table, _) = policy_table(
        case,
        &router,
        &transit_gateway_id,
        &[tag("Name", "free")],
    )
    .await;

    let before = snapshot(&engine);

    // (case, the request's fields, the HTTP status and the code expected)
    type RefusalCase = (String, Vec<(String, String)>, u16, &'static str);
    let mut cases: Vec<RefusalCase> = Vec::new();
    // One row per way a value can fail to be a policy table id of this kind: a hex suffix of
    // neither published length, a suffix that is not hex at all, the id of another resource of
    // the family, and an id whose prefix merely starts like this one's.
    for id in [
        "tgw-ptb-0123456789abcde",
        "tgw-ptb-not-an-id",
        "tgw-rtb-0123456789abcdef0",
        "tgw-ptbx-0123456789abcdef0",
    ] {
        cases.push((
            format!("a value that is not a policy table id: {id:?}"),
            table_param(id),
            400,
            ec2::state::INVALID_TRANSIT_GATEWAY_POLICY_TABLE_ID_MALFORMED,
        ));
    }
    // One row per published id length that names no policy table of the account.
    for id in ["tgw-ptb-0123456789abcdef0", "tgw-ptb-0000000f"] {
        cases.push((
            format!("a well-formed id no create ever minted: {id:?}"),
            table_param(id),
            400,
            ec2::state::INVALID_TRANSIT_GATEWAY_POLICY_TABLE_ID_NOT_FOUND,
        ));
    }
    cases.push((
        "a table an attachment is still associated with".to_string(),
        table_param(&associated_table),
        400,
        ec2::state::INCORRECT_STATE,
    ));
    cases.push((
        "a request that names no policy table at all".to_string(),
        Vec::new(),
        400,
        ec2::state::MISSING_PARAMETER,
    ));
    cases.push((
        "a dry run of a delete that would have succeeded".to_string(),
        {
            let mut params = table_param(&free_table);
            params.push(param("DryRun", "true"));
            params
        },
        DRY_RUN_STATUS,
        ec2::state::DRY_RUN_OPERATION,
    ));

    for (name, request, expected_status, expected_code) in &cases {
        let (status, body) = invoke(&router, "DeleteTransitGatewayPolicyTable", request).await;
        assert_eq!(
            status, *expected_status,
            "case {name:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, *expected_code,
            "case {name:?}: expected the code {expected_code:?}, got {code:?} in {body}"
        );
    }

    // The two refusals that quote the id are worded by the one place this crate words them,
    // so the sentence names the resource and the value the REQUEST gave.
    let malformed = "tgw-ptb-not-an-id";
    let (_, body) = invoke(
        &router,
        "DeleteTransitGatewayPolicyTable",
        &table_param(malformed),
    )
    .await;
    assert_eq!(
        element(&body, "Message").map(|message| unescaped(&message)),
        Some(format!("Invalid id: \"{malformed}\"")),
        "a malformed id must be named in the refusal, got {body}"
    );
    let unknown = "tgw-ptb-0123456789abcdef0";
    let (_, body) = invoke(
        &router,
        "DeleteTransitGatewayPolicyTable",
        &table_param(unknown),
    )
    .await;
    assert_eq!(
        element(&body, "Message").map(|message| unescaped(&message)),
        Some(format!(
            "Transit Gateway Policy Table {unknown} was deleted or does not exist."
        )),
        "an unknown id must be named in the refusal, got {body}"
    );

    let after = snapshot(&engine);
    assert_eq!(
        after, before,
        "a refused delete must remove nothing and owe nothing: expected {before:?}, got \
         {after:?}"
    );

    // The table the dry run named is still there to be deleted, which is what makes the dry
    // run's refusal a report about permission rather than about the table.
    let (status, body) = invoke(
        &router,
        "DeleteTransitGatewayPolicyTable",
        &table_param(&free_table),
    )
    .await;
    assert_eq!(
        status, 200,
        "the table a dry run named must still be deletable, got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(DELETING),
        "expected the state of a delete, got {body}"
    );
    poll_until_gone("the table the dry run named", &router, &free_table).await;
    assert!(
        !snapshot(&engine).policy_tables.contains(&free_table),
        "the delete that followed the dry run must take the table out of the store"
    );
    // The table the association blocks is still held, in the state it was in: a refused
    // delete changes nothing about it.
    let body = invoke_ok(
        case,
        &router,
        "DescribeTransitGatewayPolicyTables",
        &indexed("TransitGatewayPolicyTableIds", &[associated_table.as_str()]),
    )
    .await;
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(ec2::state::TRANSIT_GATEWAY_POLICY_TABLE_STATE_AVAILABLE),
        "a table whose delete was refused must stay as it was, got {body}"
    );
}
