// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CHARACTER CLASS a `MasterUsername` is held to, and for the
//! sentence Amazon RDS refuses a name outside its identifier rules with.
//!
//! The rule under test: a master user name is an identifier of the engine it is created on —
//! letters, numbers and UNDERSCORES, led by a letter, no longer than the bound that engine
//! publishes (`MasterUsername`: "Must be 1 to 16 letters, numbers, or underscores. First
//! character must be a letter", with the per-engine bounds the member's constraint lists
//! carry) — and a name that is not such an identifier is refused with HTTP 400,
//! `InvalidParameterValue` and the service's own fixed sentence, which carries neither the
//! name nor the bound. A name the rules admit is stored and reported BYTE FOR BYTE: no
//! lowercasing, no truncation, on the create and on the report alike.
//!
//! The table spans what the rule generalises over: names carrying underscores at both ends of
//! the engine range (a 16-character one on MySQL, a 25-character one on PostgreSQL), a
//! mixed-case name longer than the narrowest engine's bound, a digit-led name, names carrying
//! characters the class excludes, and a name one past the engine's own bound.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly as
//! the production router mounts it, each request is a real awsQuery form POST, and every
//! assertion reads the XML the emulator answered with.

use service_com_amazonaws_rds_amazonrdsv19::state::INVALID_MASTER_USERNAME_MESSAGE;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts};

/// The error code Amazon RDS answers a parameter value its member does not accept with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The fault a report answers for a DB instance that does not exist.
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

/// A master user password inside every engine's published policy, so the only refusal in play
/// is the one under test.
const PASSWORD: &str = "Emulator-Probe-1";

/// One master user name a create is made with, and whether the engine's identifier rules
/// admit it.
struct Row {
    case: &'static str,
    engine: &'static str,
    class: &'static str,
    allocated_storage: &'static str,
    /// The `MasterUsername` the request carries.
    name: &'static str,
    /// Whether the name is an identifier the engine admits.
    accepted: bool,
}

/// Each class the rule covers: the character the member's own prose admits beside letters and
/// numbers, the case and length an engine's own bound admits, and every form the rules
/// exclude.
///
/// Source: `CreateDBInstanceMessage$MasterUsername`
/// (`smithy_query(query="doc", shape="CreateDBInstanceMessage", member="MasterUsername")`),
/// whose constraints are the character class, the leading letter and the per-engine length
/// bound.
const ROWS: &[Row] = &[
    Row {
        case: "underscores inside the PostgreSQL bound",
        engine: "postgres",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "application_db_admin_user",
        accepted: true,
    },
    Row {
        case: "an underscore inside the MySQL bound",
        engine: "mysql",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "app_db_admin_usr",
        accepted: true,
    },
    Row {
        case: "a mixed-case name past the narrowest engine's bound, inside its own",
        engine: "postgres",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "Sim2RealAdminUser",
        accepted: true,
    },
    Row {
        case: "a trailing underscore, which the class admits",
        engine: "postgres",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "Admin_",
        accepted: true,
    },
    Row {
        case: "a name led by a digit",
        engine: "postgres",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "1stadmin",
        accepted: false,
    },
    Row {
        case: "a name led by an underscore",
        engine: "postgres",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "_admin",
        accepted: false,
    },
    Row {
        case: "a hyphen, which the class excludes",
        engine: "postgres",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "app-db-admin",
        accepted: false,
    },
    Row {
        case: "a dot, which the class excludes",
        engine: "mysql",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "mysql.sys",
        accepted: false,
    },
    Row {
        case: "a name one past the MySQL bound",
        engine: "mysql",
        class: "db.t3.micro",
        allocated_storage: "20",
        name: "app_db_admin_user",
        accepted: false,
    },
    Row {
        case: "a 64-character name, one past the PostgreSQL bound",
        engine: "postgres",
        class: "db.t3.micro",
        allocated_storage: "20",
        // 64 characters: one more than the 63 the PostgreSQL line publishes.
        name: "abbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        accepted: false,
    },
];

/// The create one row makes.
fn create_params(row: &Row, identifier: &str) -> Vec<(&'static str, String)> {
    vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("Engine", row.engine.to_string()),
        ("DBInstanceClass", row.class.to_string()),
        ("AllocatedStorage", row.allocated_storage.to_string()),
        ("MasterUsername", row.name.to_string()),
        ("MasterUserPassword", PASSWORD.to_string()),
    ]
}

/// RULE: a `MasterUsername` the engine's identifier rules admit — letters, numbers and
/// underscores, led by a letter, inside the engine's own length bound — is created and
/// round-trips byte for byte; one the rules exclude is refused with `InvalidParameterValue`
/// and the service's fixed sentence, and creates nothing.
#[tokio::test]
async fn a_master_user_name_is_an_identifier_of_its_engine() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let identifier = format!("emu-master-name-{index}");
        let response = call(&router, "CreateDBInstance", &create_params(row, &identifier)).await;

        if row.accepted {
            let created = expect_ok(row.case, response);
            assert!(
                texts(&created, "MasterUsername").contains(&row.name.to_string()),
                "case {:?}: expected the master user to be reported as sent ({:?}), got \
                 {created}",
                row.case,
                row.name
            );

            // ...and the STORE kept the same bytes: neither the create nor the store folded
            // the case or cut the name.
            let described = expect_ok(
                row.case,
                call(
                    &router,
                    "DescribeDBInstances",
                    &[("DBInstanceIdentifier", identifier.clone())],
                )
                .await,
            );
            assert!(
                texts(&described, "MasterUsername").contains(&row.name.to_string()),
                "case {:?}: expected the stored master user to be reported as sent ({:?}), \
                 got {described}",
                row.case,
                row.name
            );
            continue;
        }

        let refused = expect_error(row.case, response, 400, INVALID_PARAMETER_VALUE);
        let message = text(&refused, "Message");
        assert_eq!(
            message, INVALID_MASTER_USERNAME_MESSAGE,
            "case {:?}: expected the refusal {INVALID_MASTER_USERNAME_MESSAGE:?}, got \
             {message:?}",
            row.case
        );

        // Nothing was created: the report answers the identifier's not-found fault.
        let not_found = expect_error(
            row.case,
            call(
                &router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", identifier.clone())],
            )
            .await,
            404,
            DB_INSTANCE_NOT_FOUND,
        );
        let message = text(&not_found, "Message");
        let expected = format!("DBInstance {identifier} not found.");
        assert_eq!(
            message, expected,
            "case {:?}: expected the report to answer {expected:?}, got {message:?}",
            row.case
        );
    }
}
