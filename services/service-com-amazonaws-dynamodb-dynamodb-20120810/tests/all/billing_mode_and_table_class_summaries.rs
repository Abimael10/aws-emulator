// SPDX-License-Identifier: CC0-1.0

//! The two summaries a `TableDescription` carries about how a table is CHARGED, and when
//! each of them is there at all.
//!
//! RULE 1 — `BillingModeSummary` records the `PAY_PER_REQUEST` switch, so a table that has
//! never been on-demand carries NONE: "you may need to switch to on-demand mode at least
//! once in order to return a `BillingModeSummary` response"
//! (`smithy_query(query="shape",shape="BillingModeSummary")`). A table that HAS been
//! on-demand carries it from then on, reporting the mode in force beside
//! `LastUpdateToPayPerRequestDateTime`, "the time when `PAY_PER_REQUEST` was last set as
//! the read/write capacity mode" — the instant of the CALL that set it, which for a table
//! created on-demand is that table's own `CreationDateTime`. The mode of EVERY table is
//! still reported, by `TableThroughputModeSummary`, which carries no such note.
//!
//! RULE 2 — `TableClassSummary.LastUpdateDateTime` is "the date and time at which the table
//! class was last updated"
//! (`smithy_query(query="shape",shape="TableClassSummary")`): a table whose class no call
//! has updated carries the class alone, and an `UpdateTable` that changes it stamps the
//! instant of that call.
//!
//! The rows span what each rule generalises over: the mode a table is created in (named
//! either way, or left out) and the class it is created with and moved to, both classes the
//! `TableClass` enum names included.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::state::STATUS_TRANSITION;

use crate::emulator::{emulator, ok};

/// Wait out a transition the clock owes, so what follows observes the settled state.
async fn settle() {
    tokio::time::sleep(STATUS_TRANSITION + Duration::from_millis(200)).await;
}

/// One row of the capacity-mode table: how the table is created, and whether a description
/// of it must carry a `BillingModeSummary` at all.
struct ModeRow {
    case: &'static str,
    name: &'static str,
    /// The `BillingMode` the create names; `None` leaves the member out.
    created_mode: Option<&'static str>,
    /// The mode every description must report.
    expected_mode: &'static str,
    /// Whether a description carries `BillingModeSummary`: only a table that has been
    /// on-demand does.
    summary: bool,
}

const MODE_ROWS: &[ModeRow] = &[
    ModeRow {
        case: "created on-demand",
        name: "SummaryOnDemand",
        created_mode: Some("PAY_PER_REQUEST"),
        expected_mode: "PAY_PER_REQUEST",
        summary: true,
    },
    ModeRow {
        case: "created provisioned",
        name: "SummaryProvisioned",
        created_mode: Some("PROVISIONED"),
        expected_mode: "PROVISIONED",
        summary: false,
    },
    ModeRow {
        case: "created with no BillingMode member at all",
        name: "SummaryDefaulted",
        created_mode: None,
        expected_mode: "PROVISIONED",
        summary: false,
    },
];

/// The `CreateTable` body for one mode row.
fn create_body(row: &ModeRow) -> Value {
    let mut body = json!({
        "TableName": row.name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
    });
    if let Some(mode) = row.created_mode {
        body["BillingMode"] = json!(mode);
    }
    if row.expected_mode == "PROVISIONED" {
        body["ProvisionedThroughput"] = json!({"ReadCapacityUnits": 2, "WriteCapacityUnits": 2});
    }
    body
}

#[tokio::test]
async fn only_a_table_that_has_been_on_demand_carries_a_billing_mode_summary() {
    let router = emulator();

    for row in MODE_ROWS {
        let case = row.case;
        let created = ok(&router, "CreateTable", create_body(row)).await;
        let described = &created["TableDescription"];
        assert_eq!(
            described["TableThroughputModeSummary"]["TableThroughputMode"],
            json!(row.expected_mode),
            "case {case:?}: every description reports the mode in force, got {described}"
        );
        assert_eq!(
            described.get("BillingModeSummary").is_some(),
            row.summary,
            "case {case:?}: expected BillingModeSummary present={}, got {described}",
            row.summary
        );
        if row.summary {
            assert_eq!(
                described["BillingModeSummary"]["LastUpdateToPayPerRequestDateTime"],
                described["CreationDateTime"],
                "case {case:?}: a table created on-demand had PAY_PER_REQUEST set when it \
                 was created, got {described}"
            );
        }

        settle().await;
        let read = ok(&router, "DescribeTable", json!({"TableName": row.name})).await;
        let read = &read["Table"];
        assert_eq!(
            (
                read.get("BillingModeSummary").is_some(),
                read["BillingModeSummary"]["LastUpdateToPayPerRequestDateTime"].clone(),
            ),
            (
                row.summary,
                described["BillingModeSummary"]["LastUpdateToPayPerRequestDateTime"].clone(),
            ),
            "case {case:?}: a later description reports the same summary, got {read}"
        );
    }

    // A PROVISIONED table that is SWITCHED to on-demand carries the summary from the
    // switch on, and the instant it reports is the one the switching call was made at —
    // not the one the capacity move settles at, which is later.
    let switched = ok(
        &router,
        "UpdateTable",
        json!({"TableName": "SummaryProvisioned", "BillingMode": "PAY_PER_REQUEST"}),
    )
    .await;
    let switched = &switched["TableDescription"];
    let set_at = switched["BillingModeSummary"]["LastUpdateToPayPerRequestDateTime"].as_f64();
    assert!(
        set_at.is_some_and(|instant| instant > 0.0),
        "case switched to on-demand: the switch's own echo carries the instant it was made, \
         got {switched}"
    );
    assert_eq!(
        switched["TableStatus"],
        json!("UPDATING"),
        "case switched to on-demand: the capacity move is still in flight, got {switched}"
    );

    settle().await;
    // Switching BACK to provisioned does not erase it: the member records when
    // PAY_PER_REQUEST was LAST SET, not which mode is current.
    let back = ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": "SummaryProvisioned",
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 3, "WriteCapacityUnits": 3},
        }),
    )
    .await;
    let back = &back["TableDescription"];
    assert_eq!(
        (
            back["BillingModeSummary"]["BillingMode"].clone(),
            back["BillingModeSummary"]["LastUpdateToPayPerRequestDateTime"].as_f64(),
        ),
        (json!("PROVISIONED"), set_at),
        "case switched back to provisioned: the summary reports the mode in force and the \
         instant on-demand was last set, got {back}"
    );
}

#[tokio::test]
async fn a_table_class_change_stamps_the_instant_it_was_made() {
    let router = emulator();

    // The rows differ along the class a table is created with and the class it is moved
    // to: both values `TableClass` names
    // (`smithy_query(query="shape",shape="TableClassSummary")`: "valid values are STANDARD
    // and STANDARD_INFREQUENT_ACCESS"), including a create that names none at all.
    let rows: [(&str, &str, Option<&str>, &str); 3] = [
        (
            "created with no class, moved to infrequent access",
            "ClassDefaulted",
            None,
            "STANDARD_INFREQUENT_ACCESS",
        ),
        (
            "created STANDARD, moved to infrequent access",
            "ClassStandard",
            Some("STANDARD"),
            "STANDARD_INFREQUENT_ACCESS",
        ),
        (
            "created infrequent access, moved to STANDARD",
            "ClassInfrequent",
            Some("STANDARD_INFREQUENT_ACCESS"),
            "STANDARD",
        ),
    ];

    for (case, name, created_class, moved_to) in rows {
        let mut create = json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": 1},
        });
        if let Some(class) = created_class {
            create["TableClass"] = json!(class);
        }
        let created = ok(&router, "CreateTable", create).await;
        let described = &created["TableDescription"];
        // The class a create NAMED is reported, and no update instant with it: nothing has
        // updated the class.
        match created_class {
            Some(class) => assert_eq!(
                (
                    described["TableClassSummary"]["TableClass"].clone(),
                    described["TableClassSummary"].get("LastUpdateDateTime"),
                ),
                (json!(class), None),
                "case {case:?}: a class nothing has updated carries no update instant, got \
                 {described}"
            ),
            None => assert_eq!(
                described.get("TableClassSummary"),
                None,
                "case {case:?}: a table created with no class carries no summary, got \
                 {described}"
            ),
        }

        settle().await;
        let moved = ok(
            &router,
            "UpdateTable",
            json!({"TableName": name, "TableClass": moved_to}),
        )
        .await;
        let moved = &moved["TableDescription"];
        let stamped = moved["TableClassSummary"]["LastUpdateDateTime"].as_f64();
        assert_eq!(
            moved["TableClassSummary"]["TableClass"],
            json!(moved_to),
            "case {case:?}: the update reports the class it moved to, got {moved}"
        );
        assert!(
            stamped.is_some_and(|instant| instant > 0.0),
            "case {case:?}: a class change stamps the instant it was made, got {moved}"
        );

        settle().await;
        let read = ok(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (
                read["Table"]["TableClassSummary"]["TableClass"].clone(),
                read["Table"]["TableClassSummary"]["LastUpdateDateTime"].as_f64(),
            ),
            (json!(moved_to), stamped),
            "case {case:?}: a later description reports the class and the instant the \
             change stamped, got {read}"
        );

        // A request that RE-STATES the class the table is already in updates nothing, so
        // the instant the real change stamped stands.
        let restated = ok(
            &router,
            "UpdateTable",
            json!({"TableName": name, "TableClass": moved_to}),
        )
        .await;
        assert_eq!(
            restated["TableDescription"]["TableClassSummary"]["LastUpdateDateTime"].as_f64(),
            stamped,
            "case {case:?}: re-stating the class updates nothing, got {restated}"
        );
        settle().await;
    }
}

/// How many times one table's class may be CHANGED in the trailing window: "You can update
/// the table class of a table twice in a 30-day trailing period" ("Service, account, and
/// table quotas in Amazon DynamoDB", the table-class entry).
const MAX_TABLE_CLASS_CHANGES: usize = 2;

/// The two values `TableClass` names, which a run of changes alternates between.
const TABLE_CLASSES: [&str; 2] = ["STANDARD_INFREQUENT_ACCESS", "STANDARD"];

/// The class that is NOT the one in force, which is what makes the next request a CHANGE.
fn other_class(in_force: Option<&str>) -> &'static str {
    if in_force == Some(TABLE_CLASSES[0]) {
        TABLE_CLASSES[1]
    } else {
        TABLE_CLASSES[0]
    }
}

/// One row of the quota table: the case, the table, the class it is created with, and how
/// many class changes its run asks for past the quota.
type QuotaRow = (&'static str, &'static str, Option<&'static str>, usize);

#[tokio::test]
async fn a_table_class_change_past_the_quota_is_refused_and_changes_nothing() {
    // RULE: a table-class CHANGE is held to the documented quota on how many of them one
    // table may be given in the trailing window ([`MAX_TABLE_CLASS_CHANGES`]); past it the
    // answer is the control plane's modelled limit error — the code a client waits the
    // window out on — and the table keeps the class it has. A request that re-states the
    // class the table is already in changes nothing and spends none of the quota.
    let router = emulator();

    /// The sentence AWS refuses a change past the quota with, which states the quota and
    /// nothing of the table.
    const EXPECTED: &str =
        "Subscriber limit exceeded: Updates to TableClass are limited to 2 times in 30 day(s).";

    // The rows differ in the class the table starts in — both values `TableClass` names and
    // a create that names none — and in how many changes past the quota the run asks for,
    // so a refusal is answered on the first one past it and on every later one too.
    let rows: [QuotaRow; 3] = [
        ("created with no class", "ClassQuotaDefaulted", None, 1),
        (
            "created STANDARD",
            "ClassQuotaStandard",
            Some("STANDARD"),
            2,
        ),
        (
            "created infrequent access",
            "ClassQuotaInfrequent",
            Some("STANDARD_INFREQUENT_ACCESS"),
            1,
        ),
    ];

    for (case, name, created_class, past_quota) in rows {
        let mut create = json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        });
        if let Some(class) = created_class {
            create["TableClass"] = json!(class);
        }
        ok(&router, "CreateTable", create).await;
        settle().await;

        // The changes the quota ADMITS are served, each alternating the class so every one
        // of them is a real change.
        let mut in_force = created_class.map(ToString::to_string);
        for change in 0..MAX_TABLE_CLASS_CHANGES {
            let asked = other_class(in_force.as_deref());
            let served = ok(
                &router,
                "UpdateTable",
                json!({"TableName": name, "TableClass": asked}),
            )
            .await;
            assert_eq!(
                served["TableDescription"]["TableClassSummary"]["TableClass"],
                json!(asked),
                "case {case:?}: change {change} is inside the quota and is served, got {served}"
            );
            in_force = Some(asked.to_string());
            settle().await;
        }

        // Re-stating the class in force changes nothing, so it is not held to the quota at
        // all: it is served even though the quota's changes are spent.
        let restated = ok(
            &router,
            "UpdateTable",
            json!({"TableName": name, "TableClass": in_force.clone()}),
        )
        .await;
        assert_eq!(
            restated["TableDescription"]["TableClassSummary"]["TableClass"],
            json!(in_force),
            "case {case:?}: re-stating the class in force is served, got {restated}"
        );
        settle().await;

        // Every further CHANGE is refused, and the table keeps the class the last admitted
        // one left it in. Each attempt asks for the OTHER class, so every one of them would
        // be a real change.
        for attempt in 0..past_quota {
            let asked = other_class(in_force.as_deref());
            let (status, body) = crate::emulator::call(
                &router,
                "UpdateTable",
                json!({"TableName": name, "TableClass": asked}),
            )
            .await;
            assert_eq!(
                (
                    status,
                    crate::emulator::error_code(&body),
                    crate::emulator::message(&body)
                ),
                (
                    400,
                    "LimitExceededException".to_string(),
                    EXPECTED.to_string()
                ),
                "case {case:?}: attempt {attempt} past the quota is refused, got {status} with \
                 {body}"
            );
            assert_eq!(
                body.get("TableDescription"),
                None,
                "case {case:?}: a refused change answers no TableDescription, got {body}"
            );
            let read = ok(&router, "DescribeTable", json!({"TableName": name})).await;
            assert_eq!(
                read["Table"]["TableClassSummary"]["TableClass"],
                json!(in_force),
                "case {case:?}: the refused change left the class in force, got {read}"
            );
        }
    }
}
