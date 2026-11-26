// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how this front door RENDERS a `@pattern` requirement in the
//! constraint report it refuses a value with.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answered with.
//!
//! The rule under test: a value outside a member's `@pattern` is refused HTTP 400
//! `InvalidParameterValue` with the constraint report `1 validation error detected: Value
//! '<value>' at '<member path>' failed to satisfy constraint: Member must satisfy regular
//! expression pattern: <pattern>`, where `<pattern>` is the expression the service
//! ADVERTISES — the model's own text LESS the whole-value anchor pair, since the `^` and `$`
//! are how the model turns a find into a whole-string match rather than part of the
//! expression a client re-validates with. The CHECK still uses the model's pattern as
//! written, so a value the anchored expression admits is still served.
//!
//! The rows span what the rule generalises over: members of different operations, patterns
//! whose body is a character class, a repeated group, an escaped literal and a repetition
//! bound, a member whose model pattern carries NO anchor pair (quoted verbatim, so nothing
//! is stripped by position), and the control that the same members accept a value their
//! pattern admits.

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};

/// The Query API code a value a member's own constraint does not admit is refused under.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// One row: a request carrying a value outside one member's pattern, the member path the
/// report names it at, and the expression the report must quote.
struct PatternCase {
    case: &'static str,
    action: &'static str,
    /// The parameters the request carries, the offending one among them.
    params: &'static [(&'static str, &'static str)],
    /// The value the report quotes, which is the offending parameter's.
    value: &'static str,
    /// The member path the report names, the model's member name with its first character
    /// lowered — the spelling the framework's validating walk records a position in.
    member_path: &'static str,
    /// The expression the report must quote: the model's `@pattern` for that member, less
    /// the whole-value anchor pair where it carries one.
    pattern: &'static str,
}

/// The rows, each cited by the member whose `@pattern` the model declares
/// (`smithy_query(query="shape",shape="CreateDBCapacityReservationRequest",
/// model_path="services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json")`
/// and the same read of the other input shapes below).
const PATTERN_CASES: &[PatternCase] = &[
    PatternCase {
        // `DBCapacityReservationName`: `^[a-z](?:-?[a-z0-9]+){0,15}$` — a repeated
        // non-capturing group with a repetition bound.
        case: "a capacity reservation name carrying upper-case letters",
        action: "CreateDBCapacityReservation",
        params: &[
            ("DBCapacityReservationName", "Sim2Real-Bad-Name"),
            ("DBInstanceClass", "db.r7g.large"),
            ("Engine", "mysql"),
            ("CapacityPerAvailabilityZone", "1"),
            ("AvailabilityZones.AvailabilityZone.1", "us-west-2a"),
        ],
        value: "Sim2Real-Bad-Name",
        member_path: "dBCapacityReservationName",
        pattern: "[a-z](?:-?[a-z0-9]+){0,15}",
    },
    PatternCase {
        // `DBInstanceClass`: `^db\.[0-9a-z]{2,6}\.[0-9a-z\.]{4,20}$` — escaped literals
        // inside the expression, which the rendering leaves exactly as the model writes
        // them.
        case: "a capacity reservation naming a class of another form",
        action: "CreateDBCapacityReservation",
        params: &[
            ("DBCapacityReservationName", "emu-pattern-class"),
            ("DBInstanceClass", "r7g.large.not"),
            ("Engine", "mysql"),
            ("CapacityPerAvailabilityZone", "1"),
            ("AvailabilityZones.AvailabilityZone.1", "us-west-2a"),
        ],
        value: "r7g.large.not",
        member_path: "dBInstanceClass",
        pattern: r"db\.[0-9a-z]{2,6}\.[0-9a-z\.]{4,20}",
    },
    PatternCase {
        // `BlueGreenDeploymentIdentifier`: `^[A-Za-z][0-9A-Za-z-:._]*$` — a character class
        // with a `*` repetition.
        case: "a blue/green identifier that does not start with a letter",
        action: "SwitchoverBlueGreenDeployment",
        params: &[("BlueGreenDeploymentIdentifier", "9bgd-not-an-identifier")],
        value: "9bgd-not-an-identifier",
        member_path: "blueGreenDeploymentIdentifier",
        pattern: "[A-Za-z][0-9A-Za-z-:._]*",
    },
    PatternCase {
        // `CustomEngineVersion`: `^[a-z0-9_.-]{1,60}$`.
        case: "a custom engine version carrying a space",
        action: "DeleteCustomDBEngineVersion",
        params: &[
            ("Engine", "custom-oracle-ee"),
            ("EngineVersion", "19.cdb not a version"),
        ],
        value: "19.cdb not a version",
        member_path: "engineVersion",
        pattern: "[a-z0-9_.-]{1,60}",
    },
    PatternCase {
        // `CustomEngineName`: `^[A-Za-z0-9-]{1,35}$`, on the other member of the same input.
        case: "a custom engine name carrying a dot",
        action: "DeleteCustomDBEngineVersion",
        params: &[
            ("Engine", "custom.oracle.ee"),
            ("EngineVersion", "19.cdb_ru-2024-10.r1"),
        ],
        value: "custom.oracle.ee",
        member_path: "engine",
        pattern: "[A-Za-z0-9-]{1,35}",
    },
];

#[tokio::test]
async fn a_pattern_requirement_is_quoted_as_the_service_advertises_it() {
    let router = emulator();

    for row in PATTERN_CASES {
        let body = expect_error(
            row.case,
            call(&router, row.action, row.params).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = format!(
            "1 validation error detected: Value '{}' at '{}' failed to satisfy constraint: \
             Member must satisfy regular expression pattern: {}",
            row.value, row.member_path, row.pattern
        );
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
        // The anchors are gone from the RENDERING and from nothing else: the expression the
        // report quotes is the one a client re-validates with.
        assert!(
            !message.contains("pattern: ^") && !message.ends_with('$'),
            "case {:?}: the quoted expression must carry no whole-value anchor pair, got \
             {message:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_member_whose_pattern_carries_no_anchor_pair_is_quoted_verbatim() {
    let router = emulator();

    // `DescribeDBRecommendations$Filters` holds its filter names to `[a-z-]+`, an expression
    // the model declares with no anchor at all: there is no pair to strip, so the report
    // quotes it exactly as the model writes it. (The same reading of the model as the rows
    // above, on `DescribeDBRecommendationsMessage`.)
    let body = expect_error(
        "an unsupported filter name outside the expression",
        call(
            &router,
            "DescribeDBRecommendations",
            &[
                ("Filters.Filter.1.Name", "Status"),
                ("Filters.Filter.1.Values.Value.1", "active"),
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&body, "Message");
    assert!(
        message.ends_with("Member must satisfy regular expression pattern: [a-z-]+"),
        "expected the expression quoted as the model declares it, got {message:?}"
    );
}

#[tokio::test]
async fn a_value_the_anchored_expression_admits_is_still_served() {
    let router = emulator();

    // The CHECK is the model's pattern as written, so the rendering above changes nothing
    // about which values are admitted: a name the anchored expression matches is served, and
    // one that would match only PART of the value is still refused.
    expect_ok(
        "a capacity reservation name the expression admits",
        call(
            &router,
            "CreateDBCapacityReservation",
            &[
                ("DBCapacityReservationName", "emu-pattern-admitted"),
                ("DBInstanceClass", "db.r7g.large"),
                ("Engine", "mysql"),
                ("CapacityPerAvailabilityZone", "1"),
                ("AvailabilityZones.AvailabilityZone.1", "us-west-2a"),
            ],
        )
        .await,
    );
    let body = expect_error(
        "a name whose tail alone is outside the expression",
        call(
            &router,
            "CreateDBCapacityReservation",
            &[
                ("DBCapacityReservationName", "emu-pattern-rejected!"),
                ("DBInstanceClass", "db.r7g.large"),
                ("Engine", "mysql"),
                ("CapacityPerAvailabilityZone", "1"),
                ("AvailabilityZones.AvailabilityZone.1", "us-west-2a"),
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    assert!(
        text(&body, "Message").contains("failed to satisfy constraint"),
        "expected the constraint report, got {body}"
    );
}
