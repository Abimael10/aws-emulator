// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `require_secure_transport` on the MySQL-compatible DB (instance)
//! parameter group families this front door serves — Amazon RDS for MariaDB and Amazon RDS
//! for MySQL, which run one server's variables.
//!
//! Every control-plane call goes over the wire through `db_instances::emulator` as a real
//! awsQuery form POST carrying the SigV4 credential scope the router keys on, and every
//! session is opened over a real TCP connection to the address and port the control plane
//! published, speaking the engine's own protocol. So nothing here is an in-process shortcut.
//!
//! The rules under test:
//!
//! * `ModifyDBParameterGroup` ACCEPTS `require_secure_transport` on a group of any family of
//!   those engines — the parameter is one of the engine's own server variables, and the
//!   catalog this front door publishes per family is a SAMPLE of the engine's list rather
//!   than the whole of it, so a name outside that sample is taken as sent rather than
//!   refused as a parameter the family lacks — and `DescribeDBParameters` then reports it
//!   back as the caller's own setting;
//! * a DB instance whose parameter group SETS it refuses a login that arrives in the clear
//!   with the engine's own `ER_SECURE_TRANSPORT_REQUIRED` and admits one that upgrades to
//!   TLS in band, and an instance whose group turns it off admits both.
//!
//! The families are read from the engine-version catalogue this same emulator publishes
//! (`DescribeDBEngineVersions`, whose `DBParameterGroupFamily` is the member a client reads
//! a family off), so every family of those engines is covered rather than one spelled out
//! here, and the rows span the value spellings the two engines' own documentation uses for
//! the switch (`ON`/`OFF` and `1`/`0`) and both transports.
//!
//! The sibling suite `db_instance_engine_endpoint` asserts the same enforcement for ONE
//! value of the switch on a MySQL instance, as one row of its table of login outcomes; this
//! suite's subject is the parameter itself — which families carry it, which spellings they
//! take, what a group reports back, and that the MariaDB engine behind the published
//! endpoint honours each of them.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, parameter_group, Refusal, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text, texts};

/// The parameter under test: the variable the MySQL-compatible engines carry the TLS
/// requirement as.
const PARAMETER: &str = "require_secure_transport";

/// The engines whose families publish that variable: the MySQL-compatible instance engines
/// this front door serves.
const ENGINES: [&str; 2] = ["mariadb", "mysql"];

/// The `ParameterValue` spellings that turn the switch ON, as the engines' own documentation
/// writes them: MySQL's own `ON` for a boolean system variable, and the `1` an Amazon RDS
/// parameter-group example sets it with.
const ON_VALUES: [&str; 2] = ["ON", "1"];

/// The same for OFF, which is the engine's documented default for the variable.
const OFF_VALUES: [&str; 2] = ["OFF", "0"];

/// `ER_SECURE_TRANSPORT_REQUIRED`: the error number MySQL and MariaDB refuse a login that
/// arrives in the clear with while the variable is on.
const SECURE_TRANSPORT_REQUIRED: &str = "3159";

/// The sentence the engines refuse such a login with, written out as the literal text the
/// server sends.
const SECURE_TRANSPORT_REQUIRED_SENTENCE: &str =
    "Connections using insecure transport are prohibited while --require_secure_transport=ON.";

/// Every `DBParameterGroupFamily` the engine-version catalogue publishes for `engine`, in
/// ascending order and without repeats — the families a client can create a DB parameter
/// group of for that engine.
async fn published_families(router: &MultiServiceRouter, engine: &str) -> Vec<String> {
    let body = expect_ok(
        engine,
        call(
            router,
            "DescribeDBEngineVersions",
            &[
                ("Engine", engine),
                // "Constraints: Minimum 20, maximum 100" — the largest page the model
                // admits, which every engine's published release list fits in.
                ("MaxRecords", "100"),
            ],
        )
        .await,
    );
    assert_eq!(
        optional_text(&body, "Marker"),
        None,
        "engine {engine:?}: the published release list fits one page, so no Marker is owed"
    );
    let families: BTreeSet<String> = texts(&body, "DBParameterGroupFamily").into_iter().collect();
    assert!(
        !families.is_empty(),
        "engine {engine:?}: the catalogue must publish a parameter group family"
    );
    families.into_iter().collect()
}

/// The default family of `engine` in this region, as a client reads it:
/// `DescribeDBEngineVersions(Engine=…, DefaultOnly=true).DBParameterGroupFamily`.
async fn default_family(router: &MultiServiceRouter, engine: &str) -> String {
    let body = expect_ok(
        engine,
        call(
            router,
            "DescribeDBEngineVersions",
            &[("Engine", engine), ("DefaultOnly", "true")],
        )
        .await,
    );
    text(&body, "DBParameterGroupFamily")
}

/// `ModifyDBParameterGroup` setting `PARAMETER` to `value` on `group`, applied immediately.
async fn set_secure_transport(
    router: &MultiServiceRouter,
    group: &str,
    value: &str,
) -> (u16, String) {
    call(
        router,
        "ModifyDBParameterGroup",
        &[
            ("DBParameterGroupName", group),
            ("Parameters.Parameter.1.ParameterName", PARAMETER),
            ("Parameters.Parameter.1.ParameterValue", value),
            ("Parameters.Parameter.1.ApplyMethod", "immediate"),
        ],
    )
    .await
}

/// The `<Parameter>` subtree `group` reports for `PARAMETER` as the CALLER's own setting, or
/// `None` where the group carries none of its own for it.
async fn reported_setting(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Option<String> {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBParameters",
            &[("DBParameterGroupName", group), ("Source", "user")],
        )
        .await,
    );
    subtrees(&body, "Parameter")
        .into_iter()
        .find(|parameter| text(parameter, "ParameterName") == PARAMETER)
}

#[tokio::test]
async fn every_mysql_family_group_takes_require_secure_transport() {
    let router = emulator();

    // The default family each engine publishes is the one a client reads before it creates a
    // group, so it must be among the families the catalogue publishes for that engine.
    for engine in ENGINES {
        let families = published_families(&router, engine).await;
        let default = default_family(&router, engine).await;
        assert!(
            families.contains(&default),
            "engine {engine:?}: the default family {default:?} must be one of the published \
             families {families:?}"
        );

        for (index, family) in families.iter().enumerate() {
            let group = format!("emu-rst-{engine}-{index}");
            expect_ok(
                family,
                call(
                    &router,
                    "CreateDBParameterGroup",
                    &[
                        ("DBParameterGroupName", group.as_str()),
                        ("DBParameterGroupFamily", family.as_str()),
                        ("Description", "a TLS-enforcement group under test"),
                    ],
                )
                .await,
            );

            // Each spelling of the switch, set in turn on the one group: the modify is
            // accepted, answers with the group it changed, and the group then reports the
            // value as the caller's own.
            for value in ON_VALUES.iter().chain(OFF_VALUES.iter()) {
                let case = format!("{PARAMETER} = {value:?} on a group of {family}");
                let body = expect_ok(&case, set_secure_transport(&router, &group, value).await);
                assert_eq!(
                    text(&body, "DBParameterGroupName"),
                    group,
                    "case {case:?}: the modify answers with the group it changed: {body}"
                );
                let reported = reported_setting(&router, &case, &group).await;
                let reported = reported.unwrap_or_else(|| {
                    panic!("case {case:?}: the group must report the parameter it was set")
                });
                assert_eq!(
                    text(&reported, "ParameterValue"),
                    *value,
                    "case {case:?}: the group reports the value the caller set: {reported}"
                );
                assert_eq!(
                    text(&reported, "ApplyMethod"),
                    "immediate",
                    "case {case:?}: the group reports the apply method the caller asked \
                     for: {reported}"
                );
            }
        }
    }
}

/// One row of the enforcement table: the engine an instance runs, the value its parameter
/// group sets the switch to, how the session is opened, and what the server answers.
struct Enforcement {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The value the instance's own parameter group sets, or `None` for an instance left on
    /// the family's default group, which sets nothing.
    set_to: Option<&'static str>,
    transport: Transport,
    /// `true` when the login must be refused `ER_SECURE_TRANSPORT_REQUIRED`.
    refused: bool,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mysql_family_endpoint_enforces_the_parameter_its_group_sets() {
    let router = emulator();
    let family = default_family(&router, "mariadb").await;

    // One group per value spelling, each of the region's default MariaDB family, so the
    // instances below differ only in what their own group says.
    for value in ON_VALUES.iter().chain(OFF_VALUES.iter()) {
        parameter_group(
            &router,
            &format!("emu-rst-group-{}", value.to_ascii_lowercase()),
            &family,
            PARAMETER,
            value,
        )
        .await;
    }

    // The rows span what the rule generalises over: each spelling that turns the switch on
    // and each that turns it off, both transports a client can open a session with, and an
    // instance left on the family's own default group, which does not set the parameter at
    // all.
    let table: &[Enforcement] = &[
        Enforcement {
            case: "mariadb with the switch ON refuses a plaintext login",
            identifier: "emu-rst-on",
            engine: "mariadb",
            set_to: Some("ON"),
            transport: Transport::Plaintext,
            refused: true,
        },
        Enforcement {
            case: "mariadb with the switch set to 1 refuses a plaintext login too",
            identifier: "emu-rst-one",
            engine: "mariadb",
            set_to: Some("1"),
            transport: Transport::Plaintext,
            refused: true,
        },
        Enforcement {
            case: "mariadb with the switch ON admits a session upgraded to TLS",
            identifier: "emu-rst-on-tls",
            engine: "mariadb",
            set_to: Some("ON"),
            transport: Transport::Tls,
            refused: false,
        },
        Enforcement {
            case: "mariadb with the switch OFF admits a plaintext login",
            identifier: "emu-rst-off",
            engine: "mariadb",
            set_to: Some("OFF"),
            transport: Transport::Plaintext,
            refused: false,
        },
        Enforcement {
            case: "mariadb with the switch set to 0 admits one as well",
            identifier: "emu-rst-zero",
            engine: "mariadb",
            set_to: Some("0"),
            transport: Transport::Plaintext,
            refused: false,
        },
        Enforcement {
            case: "mariadb on the family's default group, which sets nothing, admits one",
            identifier: "emu-rst-default",
            engine: "mariadb",
            set_to: None,
            transport: Transport::Plaintext,
            refused: false,
        },
    ];

    for row in table {
        let case = row.case;
        let extra: Vec<(&str, String)> = match row.set_to {
            Some(value) => vec![(
                "DBParameterGroupName",
                format!("emu-rst-group-{}", value.to_ascii_lowercase()),
            )],
            None => Vec::new(),
        };
        let (address, port) =
            available_instance(&router, row.identifier, row.engine, &extra).await;
        let outcome: Result<(), Refusal> = mysql_login(
            &address,
            port,
            row.transport,
            MASTER_USER,
            MASTER_PASSWORD,
            Some(DATABASE),
        )
        .await
        .map(|_| ());
        match (row.refused, &outcome) {
            (false, Ok(())) => {}
            (true, Err(refusal)) => {
                assert_eq!(
                    refusal.code, SECURE_TRANSPORT_REQUIRED,
                    "case {case:?}: expected {SECURE_TRANSPORT_REQUIRED}, got {refusal:?}"
                );
                assert_eq!(
                    refusal.message, SECURE_TRANSPORT_REQUIRED_SENTENCE,
                    "case {case:?}: expected the engine's own sentence, got {refusal:?}"
                );
            }
            (refused, actual) => panic!(
                "case {case:?}: expected refused={refused}, got {actual:?}"
            ),
        }
    }
}
