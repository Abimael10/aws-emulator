// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the parameter defaults Amazon RDS publishes as a FORMULA over the
//! instance's hardware: what the ENGINE of a DB instance runs with for them.
//!
//! A DB instance created with no parameter group of its own uses the default group of its
//! family, and the default that group publishes for such a parameter is an expression over
//! the instance class's memory (`LEAST({DBInstanceClassMemory/9531392},5000)` for the
//! PostgreSQL connection ceiling, `{DBInstanceClassMemory/32768}` for its shared buffer
//! pool, `{DBInstanceClassMemory/12582880}` for the MySQL connection ceiling). The rules
//! under test:
//!
//! * the value the engine runs with for such a parameter is that expression EVALUATED for
//!   the class the instance was created with — read through `SHOW`, through
//!   `current_setting()` and through `pg_settings`, which all report the one value;
//! * `pg_settings` tells a client where that value came from: it is NOT the engine's own
//!   compiled default (`source` is something other than `default`, while `boot_val` stays
//!   the value the engine itself would start with), and `reset_val` is the value in force;
//! * a formula with a ceiling is bounded by it, so a class whose memory is above the
//!   ceiling's reach reports the ceiling;
//! * each engine evaluates ITS OWN published formula, so a MySQL instance of the same class
//!   as a PostgreSQL one reports its own engine's number.
//!
//! The rows span what the rule generalises over: the instance class (four sizes across three
//! families, from the smallest burstable one to a class the ceiling binds for), the
//! parameter (two formulas of the PostgreSQL family), the engine (both wires) and the three
//! ways a client reads a parameter. Every expected number is the published formula written
//! out for the class's published memory, so no assertion recomputes it the way the code
//! does.

use crate::db_instance_engine_endpoint::{
    available_instance_of_class, mysql_login, postgres_login, PostgresSession, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::emulator;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The value PostgreSQL's `pg_settings.source` carries for a parameter still on the engine's
/// OWN compiled default — the one value a formula-valued parameter must not report, because
/// the engine cannot compute a formula.
const SOURCE_ENGINE_DEFAULT: &str = "default";

/// The value `pg_settings.source` carries for a setting the server read out of its
/// configuration file, which is where Amazon RDS writes the value it computed for the
/// instance's class.
const SOURCE_CONFIGURATION_FILE: &str = "configuration file";

/// The value the PostgreSQL engine itself starts `max_connections` with
/// (`postgresql.conf` ships `max_connections = 100`), which is what `boot_val` reports
/// whatever the instance's class gives the running value.
const POSTGRES_BOOT_MAX_CONNECTIONS: &str = "100";

/// One DB instance class, and what the published formulas come to for the memory the
/// instance-class table publishes for it.
struct ClassCase {
    case: &'static str,
    /// `DBInstanceClass`.
    class: &'static str,
    /// The memory the published hardware-specification table gives the class, in GiB — the
    /// number the formulas below are written out for.
    memory_gib: u64,
    /// `LEAST({DBInstanceClassMemory/9531392},5000)` for that memory.
    max_connections: &'static str,
    /// `{DBInstanceClassMemory/32768}` for that memory: the shared buffer pool in 8 kB
    /// pages.
    shared_buffers: &'static str,
}

/// The classes, smallest first: the two smallest burstable sizes, a general-purpose class
/// and a memory-optimised one whose memory puts the connection formula past its ceiling.
const CLASS_CASES: &[ClassCase] = &[
    ClassCase {
        case: "the smallest burstable class",
        class: "db.t3.micro",
        memory_gib: 1,
        // 1 GiB / 9531392 = 112 (truncated), below the formula's ceiling of 5000.
        max_connections: "112",
        // 1 GiB / 32768 = 32768 pages of 8 kB.
        shared_buffers: "32768",
    },
    ClassCase {
        case: "a 2 GiB burstable class",
        class: "db.t3.small",
        memory_gib: 2,
        max_connections: "225",
        shared_buffers: "65536",
    },
    ClassCase {
        case: "a general-purpose class",
        class: "db.m5.large",
        memory_gib: 8,
        max_connections: "901",
        shared_buffers: "262144",
    },
    ClassCase {
        case: "a memory-optimised class the connection formula's ceiling binds for",
        class: "db.r5.4xlarge",
        memory_gib: 128,
        // 128 GiB / 9531392 = 14419, above the ceiling, so LEAST reports the ceiling.
        max_connections: "5000",
        shared_buffers: "4194304",
    },
];

/// Open a session as the master user on a PostgreSQL instance's published endpoint.
async fn session(address: &str, port: u16) -> PostgresSession {
    postgres_login(
        address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted")
}

/// The one value a statement answered with.
async fn value(case: &str, session: &mut PostgresSession, sql: &str) -> String {
    match session.query(sql).await {
        Ok(rows) => rows.only().unwrap_or_default().to_string(),
        Err(refusal) => panic!("case {case:?}: {sql:?} must run, got {refusal:?}"),
    }
}

#[tokio::test]
async fn a_formula_default_is_evaluated_for_the_instances_own_class() {
    let router = emulator();
    for row in CLASS_CASES {
        let identifier = format!("emu-formula-{}", row.class.replace('.', "-"));
        // No `DBParameterGroupName`: the instance uses the default group of its family,
        // whose published defaults are the formulas under test.
        let (address, port) = available_instance_of_class(
            &router,
            &identifier,
            "postgres",
            row.class,
            &params!["EngineVersion" => "16.4"],
        )
        .await;
        let mut session = session(&address, port).await;

        for (parameter, expected) in [
            ("max_connections", row.max_connections),
            ("shared_buffers", row.shared_buffers),
        ] {
            let case = format!("{} ({} GiB): {parameter}", row.case, row.memory_gib);
            // The three ways a client reads a parameter must all report the evaluated
            // formula.
            let shown = value(&case, &mut session, &format!("SHOW {parameter}")).await;
            assert_eq!(
                shown, expected,
                "case {case:?}: SHOW {parameter} must report the formula evaluated for \
                 {} GiB, got {shown:?}",
                row.memory_gib
            );
            let through_function = value(
                &case,
                &mut session,
                &format!("SELECT current_setting('{parameter}')"),
            )
            .await;
            assert_eq!(
                through_function, expected,
                "case {case:?}: current_setting must agree with SHOW, got {through_function:?}"
            );
            let catalogued = value(
                &case,
                &mut session,
                &format!("SELECT setting FROM pg_settings WHERE name = '{parameter}'"),
            )
            .await;
            assert_eq!(
                catalogued, expected,
                "case {case:?}: pg_settings must agree with SHOW, got {catalogued:?}"
            );
            // And the catalog says where the value came from: not the engine's own
            // compiled default, which is what `boot_val` still reports.
            let source = value(
                &case,
                &mut session,
                &format!("SELECT source FROM pg_settings WHERE name = '{parameter}'"),
            )
            .await;
            assert_ne!(
                source, SOURCE_ENGINE_DEFAULT,
                "case {case:?}: a value the service computed for the class is not the \
                 engine's own default, got {source:?}"
            );
            assert_eq!(
                source, SOURCE_CONFIGURATION_FILE,
                "case {case:?}: the value must come from the configuration the service wrote \
                 for this class, got {source:?}"
            );
            let reset = value(
                &case,
                &mut session,
                &format!("SELECT reset_val FROM pg_settings WHERE name = '{parameter}'"),
            )
            .await;
            assert_eq!(
                reset, expected,
                "case {case:?}: reset_val must be the value in force, got {reset:?}"
            );
        }

        let boot = value(
            row.case,
            &mut session,
            "SELECT boot_val FROM pg_settings WHERE name = 'max_connections'",
        )
        .await;
        assert_eq!(
            boot, POSTGRES_BOOT_MAX_CONNECTIONS,
            "case {:?}: boot_val must stay the value the engine itself starts with, got \
             {boot:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn each_engine_evaluates_its_own_published_formula() {
    let router = emulator();
    // The same class as the second row above, on the MySQL wire: its family publishes
    // `{DBInstanceClassMemory/12582880}` for `max_connections`, so the number is its own.
    let (address, port) = available_instance_of_class(
        &router,
        "emu-formula-mysql",
        "mysql",
        "db.t3.small",
        &params!["EngineVersion" => "8.0.39"],
    )
    .await;
    let mut session = mysql_login(
        &address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted");
    // 2 GiB / 12582880 = 170 (truncated).
    let expected = "170";
    // `@@<variable>` is how a MySQL client reads a server variable's value.
    let sql = "SELECT @@max_connections";
    let rows = session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("{sql:?} must run, got {refusal:?}"));
    let reported = rows.only().unwrap_or_default().to_string();
    assert_eq!(
        reported, expected,
        "{sql:?} must report the MySQL formula evaluated for a 2 GiB class, got {reported:?}"
    );
}
