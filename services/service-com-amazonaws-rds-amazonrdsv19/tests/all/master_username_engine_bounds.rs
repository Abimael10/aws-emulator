// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the LENGTH a `MasterUsername` may have, which Amazon RDS publishes
//! PER ENGINE.
//!
//! The rule under test: the bound a `MasterUsername` is held to is the one the member
//! publishes for the engine the request names — `MasterUsername` carries one constraint list
//! per engine the operation serves ("Amazon RDS for Db2 … Must be 1 to 16 letters or
//! numbers", "MariaDB … 1 to 16", "MySQL … 1 to 16", "Oracle … 1 to 30", "PostgreSQL … 1 to
//! 63", "Microsoft SQL Server … 1 to 128") — so a name inside that engine's bound is accepted
//! whatever a narrower engine's line says, a name one character past it is refused with
//! `InvalidParameterValue` naming the member and the bound, and nothing is created for the
//! refused request. An accepted name is stored and reported BYTE FOR BYTE, so its letter case
//! survives the round trip.
//!
//! The table spans what the rule generalises over: the six engine families whose published
//! bound differs (PostgreSQL, Microsoft SQL Server, Oracle, MySQL, MariaDB and Db2), the
//! cluster front door as well as the instance one for the Aurora engines of the two bounds
//! that differ most, and for each row BOTH its published bound and one character past it.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly as
//! the production router mounts it, each request is a real awsQuery form POST, and every
//! assertion reads the XML the emulator answered with.

use service_com_amazonaws_rds_amazonrdsv19::state::master_username_len;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts};

/// The error code Amazon RDS answers a parameter value its member does not accept with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// A master user password every row carries, inside every engine's documented password
/// policy, so the only refusal in play is the one under test.
const PASSWORD: &str = "Emulator-Probe-1";

/// Which front door a row creates its resource through.
#[derive(Debug, Clone, Copy)]
enum Subject {
    /// `CreateDBInstance`, read back through `DescribeDBInstances`.
    Instance {
        class: &'static str,
        allocated_storage: &'static str,
    },
    /// `CreateDBCluster`, read back through `DescribeDBClusters`.
    Cluster,
}

/// One engine, the bound its own line publishes, and the door a row reaches it through.
struct Row {
    case: &'static str,
    engine: &'static str,
    subject: Subject,
    /// The longest `MasterUsername` this engine's own constraint list publishes.
    ///
    /// Source: the per-engine constraint lists of `CreateDBInstanceMessage$MasterUsername`
    /// (`smithy_query(query="doc", shape="CreateDBInstanceMessage", member="MasterUsername")`
    /// carries the member's summary; the per-engine lists are the ones the API Reference
    /// publishes beside it). The service reads the same bound through
    /// [`master_username_len`], which every row cross-checks.
    longest: usize,
}

/// Each engine family whose published bound differs, plus the two Aurora engines through the
/// cluster door: a rule read from the engine has to hold at every door that takes the member.
const ROWS: &[Row] = &[
    Row {
        case: "PostgreSQL, whose line publishes the widest RDS bound",
        engine: "postgres",
        subject: Subject::Instance {
            class: "db.t3.micro",
            allocated_storage: "20",
        },
        longest: 63,
    },
    Row {
        case: "Microsoft SQL Server",
        engine: "sqlserver-ex",
        subject: Subject::Instance {
            class: "db.t3.small",
            allocated_storage: "30",
        },
        longest: 128,
    },
    Row {
        case: "Oracle",
        engine: "oracle-se2",
        subject: Subject::Instance {
            class: "db.m5.xlarge",
            allocated_storage: "40",
        },
        longest: 30,
    },
    Row {
        case: "MySQL",
        engine: "mysql",
        subject: Subject::Instance {
            class: "db.t3.micro",
            allocated_storage: "20",
        },
        longest: 16,
    },
    Row {
        case: "MariaDB",
        engine: "mariadb",
        subject: Subject::Instance {
            class: "db.t3.medium",
            allocated_storage: "25",
        },
        longest: 16,
    },
    Row {
        case: "Db2",
        engine: "db2-se",
        subject: Subject::Instance {
            class: "db.m6i.large",
            allocated_storage: "60",
        },
        longest: 16,
    },
    Row {
        case: "Aurora PostgreSQL through the cluster door",
        engine: "aurora-postgresql",
        subject: Subject::Cluster,
        longest: 63,
    },
    Row {
        case: "Aurora MySQL through the cluster door",
        engine: "aurora-mysql",
        subject: Subject::Cluster,
        longest: 16,
    },
];

/// A `MasterUsername` of exactly `length` characters: letters and digits, led by a letter, in
/// MIXED case, so an accepted one also proves the stored value keeps the case it was sent in.
fn username(length: usize) -> String {
    // Letters first so the leading character is always one, and both cases plus digits are
    // present in every name long enough to reach them.
    const ALPHABET: [char; 8] = ['M', 'a', 'S', 't', 'E', 'r', '0', '9'];
    (0..length)
        .map(|position| ALPHABET[position % ALPHABET.len()])
        .collect()
}

/// The create a row makes with `name` as its master user, and the read that answers whether
/// the resource exists afterwards.
fn create_params(row: &Row, identifier: &str, name: &str) -> (&'static str, Vec<(String, String)>) {
    let mut params: Vec<(String, String)> = vec![
        ("MasterUsername".to_string(), name.to_string()),
        ("MasterUserPassword".to_string(), PASSWORD.to_string()),
        ("Engine".to_string(), row.engine.to_string()),
    ];
    match row.subject {
        Subject::Instance {
            class,
            allocated_storage,
        } => {
            params.push(("DBInstanceIdentifier".to_string(), identifier.to_string()));
            params.push(("DBInstanceClass".to_string(), class.to_string()));
            params.push(("AllocatedStorage".to_string(), allocated_storage.to_string()));
            ("CreateDBInstance", params)
        }
        Subject::Cluster => {
            params.push(("DBClusterIdentifier".to_string(), identifier.to_string()));
            ("CreateDBCluster", params)
        }
    }
}

/// The read that answers whether a row's resource exists, with the error code and sentence its
/// not-found fault carries.
fn read_params(row: &Row, identifier: &str) -> (&'static str, (String, String), &'static str) {
    match row.subject {
        Subject::Instance { .. } => (
            "DescribeDBInstances",
            (
                "DBInstanceIdentifier".to_string(),
                identifier.to_string(),
            ),
            "DBInstanceNotFound",
        ),
        Subject::Cluster => (
            "DescribeDBClusters",
            (
                "DBClusterIdentifier".to_string(),
                identifier.to_string(),
            ),
            "DBClusterNotFoundFault",
        ),
    }
}

/// RULE: a `MasterUsername` at the bound the engine named in the request publishes is
/// accepted and round-trips byte for byte; one character past that bound is refused with
/// `InvalidParameterValue` naming the member and the bound, and creates nothing.
#[tokio::test]
async fn a_master_username_is_held_to_the_bound_its_engine_publishes() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        // The bound the service reads and the bound this suite states are the same number:
        // a drift in either is a failure here rather than a silently weaker assertion.
        assert_eq!(
            *master_username_len(row.engine).end(),
            row.longest,
            "case {:?}: the service must publish the engine's own bound",
            row.case
        );

        let at_bound = username(row.longest);
        let identifier = format!("emu-master-user-{index}");
        let case = format!("{} at its published bound", row.case);
        let (action, params) = create_params(row, &identifier, &at_bound);
        let created = expect_ok(&case, call(&router, action, &params).await);
        assert!(
            texts(&created, "MasterUsername").contains(&at_bound),
            "case {case:?}: expected the master user to be reported as sent ({at_bound:?}), \
             got {created}"
        );

        // ...and the stored resource reports the same bytes, so neither the create nor the
        // store folded the case of the name.
        let (read, (member, value), _) = read_params(row, &identifier);
        let described = expect_ok(&case, call(&router, read, &[(member, value)]).await);
        assert!(
            texts(&described, "MasterUsername").contains(&at_bound),
            "case {case:?}: expected the stored master user to keep its case ({at_bound:?}), \
             got {described}"
        );

        // One character past the engine's own bound is refused, and nothing is created.
        let past_bound = username(row.longest + 1);
        let refused_identifier = format!("emu-master-user-past-{index}");
        let case = format!("{} one character past its published bound", row.case);
        let (action, params) = create_params(row, &refused_identifier, &past_bound);
        let refused = expect_error(
            &case,
            call(&router, action, &params).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&refused, "Message");
        assert!(
            message.contains("MasterUsername") && message.contains(&format!("1 to {}", row.longest)),
            "case {case:?}: expected the refusal to name the member and the engine's bound \
             of {}, got {message:?}",
            row.longest
        );

        let (read, (member, value), not_found) = read_params(row, &refused_identifier);
        expect_error(
            &case,
            call(&router, read, &[(member, value)]).await,
            404,
            not_found,
        );
    }
}

/// RULE: the bound is the ENGINE's, so the same name is accepted on an engine whose line
/// admits it and refused on one whose line does not — the two answers differ in the engine
/// alone.
#[tokio::test]
async fn one_name_is_accepted_or_refused_by_the_engine_alone() {
    let router = emulator();

    // Seventeen characters: past MySQL's published bound of 16, inside PostgreSQL's 63.
    let name = username(17);
    let (admits, refuses) = (
        Row {
            case: "PostgreSQL admits a seventeen-character name",
            engine: "postgres",
            subject: Subject::Instance {
                class: "db.t3.micro",
                allocated_storage: "20",
            },
            longest: 63,
        },
        Row {
            case: "MySQL refuses the same name",
            engine: "mysql",
            subject: Subject::Instance {
                class: "db.t3.micro",
                allocated_storage: "20",
            },
            longest: 16,
        },
    );

    let (action, params) = create_params(&admits, "emu-master-user-shared-pg", &name);
    let created = expect_ok(admits.case, call(&router, action, &params).await);
    assert!(
        texts(&created, "MasterUsername").contains(&name),
        "case {:?}: expected the name to be accepted and reported, got {created}",
        admits.case
    );

    let (action, params) = create_params(&refuses, "emu-master-user-shared-my", &name);
    let refused = expect_error(
        refuses.case,
        call(&router, action, &params).await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&refused, "Message");
    assert!(
        message.contains("MasterUsername") && message.contains("1 to 16"),
        "case {:?}: expected the refusal to name the member and MySQL's bound, got \
         {message:?}",
        refuses.case
    );
}
