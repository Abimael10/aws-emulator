// SPDX-License-Identifier: CC0-1.0

//! The per-parameter METADATA `DescribeParameters` publishes, end to end over the wire.
//!
//! Every case drives the SAME path a client does: an `awsJson1_1` `POST /` with
//! `X-Amz-Target: AmazonMemoryDB.<Operation>` through the multi-service router (the
//! shared harness in `cluster_lifecycle`), so what is asserted is the emulator's HTTP
//! behaviour and not a handler called directly.
//!
//! The rules under test, each over a SET of parameters rather than one recorded request:
//!
//! * a reported parameter's `Description` and `AllowedValues` are the service's ONE
//!   fixed string per parameter name, word for word, the same for every family that
//!   carries the name and the same for a default group and a group a caller created;
//! * `AllowedValues` renders the value domain with an end the service publishes NO bound
//!   for left empty on its side of the hyphen, so a domain open at the top keeps the
//!   trailing hyphen, one open at both ends is the bare hyphen, and several disjoint
//!   ranges are comma-separated;
//! * what a parameter publishes is exactly what an update enforces for it, in both
//!   directions: every published bound is accepted, a value past a bound the service
//!   publishes is refused with that same expression quoted, and a domain open at the
//!   bottom accepts a negative value.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::cluster_lifecycle::{error_code, error_message, router};
use crate::parameters::{
    carries, create_group, default_group_name, field, parameters, update, Lines,
};

/// One row of the published per-parameter metadata: the name, the description the service
/// publishes for it and the `AllowedValues` expression it publishes, where `None` is a
/// setting the engine parses itself and the service publishes no value set for.
///
/// Source: the engine default parameters the recorded MemoryDB endpoint publishes for its
/// parameter group families — the service's own catalog
/// (`crate::state::ENGINE_PARAMETERS`), whose text and bounds are the same for every
/// account. Rows the issue cited (the slow-log, client-side-caching and vector-search
/// settings) and rows it did not (`acllog-max-len`, `timeout`, `maxmemory-policy`,
/// `proto-max-bulk-len`, `lfu-decay-time`, `notify-keyspace-events`,
/// `latency-tracking`), so the rule is checked beyond the cited names and across every
/// shape of domain: a closed range, a range open at the top, a range open at BOTH ends,
/// two disjoint ranges, a token set and free text.
struct Published {
    name: &'static str,
    description: &'static str,
    allowed_values: Option<&'static str>,
    lines: Lines,
}

const PUBLISHED: &[Published] = &[
    Published {
        name: "slowlog-log-slower-than",
        description: "The execution time, in microseconds, to exceed in order for the \
                      command to get logged. Note that a negative number disables the \
                      slow log, while a value of zero forces the logging of every \
                      command",
        allowed_values: Some("-"),
        lines: Lines::Every,
    },
    Published {
        name: "slowlog-max-len",
        description: "The length of the slow log. There is no limit to this length. Just \
                      be aware that it will consume memory. You can reclaim memory used \
                      by the slow log with SLOWLOG RESET.",
        allowed_values: Some("0-"),
        lines: Lines::Every,
    },
    Published {
        name: "tracking-table-max-keys",
        description: "The maximum number of keys allowed for the tracking table for \
                      client side caching",
        allowed_values: Some("1-100000000"),
        lines: Lines::Every,
    },
    Published {
        name: "acllog-max-len",
        description: "The maximum length of the ACL Log",
        allowed_values: Some("1-10000"),
        lines: Lines::Every,
    },
    Published {
        name: "timeout",
        description: "Close connection if client is idle for a given number of seconds, \
                      or never if 0",
        allowed_values: Some("0,20-"),
        lines: Lines::Every,
    },
    Published {
        name: "lfu-decay-time",
        description: "The amount of time in minutes to decrement the key counter for LFU \
                      eviction policyd",
        allowed_values: Some("0-"),
        lines: Lines::Every,
    },
    Published {
        name: "proto-max-bulk-len",
        description: "The largest single request or bulk reply, in bytes",
        allowed_values: Some("1048576-536870912"),
        lines: Lines::Every,
    },
    Published {
        name: "maxmemory-policy",
        description: "Max memory policy",
        allowed_values: Some(
            "volatile-lru,allkeys-lru,volatile-lfu,allkeys-lfu,volatile-random,\
             allkeys-random,volatile-ttl,noeviction",
        ),
        lines: Lines::Every,
    },
    Published {
        name: "notify-keyspace-events",
        description: "The keyspace events for Redis to notify Pub/Sub clients about. By \
                      default all notifications are disabled",
        allowed_values: None,
        lines: Lines::Every,
    },
    Published {
        name: "latency-tracking",
        description: "Track the per command latencies and enable exporting the percentile \
                      distribution via the INFO latencystats command and cumulative \
                      latency distributions (histograms) via the LATENCY command.",
        allowed_values: Some("yes,no"),
        lines: Lines::Redis7Onwards,
    },
    Published {
        name: "search-enabled",
        description: "When set to yes, it enables search capabilities.",
        allowed_values: Some("yes,no"),
        lines: Lines::ValkeyOnly,
    },
    Published {
        name: "search-query-timeout-ms",
        description: "The maximum amount of time in milliseconds that a search query is \
                      allowed to run.",
        allowed_values: Some("1-60000"),
        lines: Lines::ValkeyOnly,
    },
];

/// The parameter group families the engine catalog publishes, one per engine line.
const FAMILIES: &[&str] = &["memorydb_redis6", "memorydb_redis7", "memorydb_valkey7"];

/// A family carrying every row above, so one group can exercise the whole table.
const VALKEY_FAMILY: &str = "memorydb_valkey7";

/// The `AllowedValues` a group reports for one parameter, absent for free text.
fn published_domain(reported: &BTreeMap<String, Value>, name: &str, case: &str) -> Option<String> {
    reported
        .get(name)
        .unwrap_or_else(|| panic!("case {case:?}: expected the parameter {name:?} to be reported"))
        .get("AllowedValues")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Every parameter a group reports carries the service's FIXED description and value-set
/// expression for its name: the same for a default group and a created one, and the same
/// for every family that carries the name.
#[tokio::test]
async fn a_reported_parameter_carries_the_services_fixed_description_and_value_set() {
    let router = router();

    for (index, family) in FAMILIES.iter().enumerate() {
        let created = format!("published-text-{index}");
        create_group(&router, &created, family).await;
        let default_group = default_group_name(family);

        for source in [default_group.as_str(), created.as_str()] {
            let reported = parameters(&router, source).await;
            for row in PUBLISHED {
                let case = format!("{source}/{}", row.name);
                let Some(parameter) = reported.get(row.name) else {
                    assert!(
                        !carries(family, row.lines),
                        "case {case:?}: the family must carry this setting, got the \
                         listing {:?}",
                        reported.keys().collect::<Vec<_>>()
                    );
                    continue;
                };
                assert!(
                    carries(family, row.lines),
                    "case {case:?}: a family of another engine line must not report this \
                     setting, got {parameter}"
                );
                assert_eq!(
                    field(parameter, "Description", &case),
                    row.description,
                    "case {case:?}: expected the published description, got {parameter}"
                );
                assert_eq!(
                    parameter.get("AllowedValues").and_then(Value::as_str),
                    row.allowed_values,
                    "case {case:?}: expected the published value set, got {parameter}"
                );
            }
        }
    }
}

/// One update case: a value for a parameter, and whether the published domain holds it.
struct DomainCase {
    name: &'static str,
    value: &'static str,
    accepted: bool,
}

/// Values at, inside and past each published bound, including the negative value a
/// domain open at the bottom accepts and the cap a closed one refuses.
///
/// Source: the `AllowedValues` expression the service publishes for each name (see
/// [`PUBLISHED`]); the slow-log threshold's negative value is the documented way to
/// disable the slow log, which is why its domain is open at the bottom.
const DOMAIN_CASES: &[DomainCase] = &[
    // Open at both ends: a negative value, zero and a value far above any cap.
    DomainCase { name: "slowlog-log-slower-than", value: "-1", accepted: true },
    DomainCase { name: "slowlog-log-slower-than", value: "-1000000", accepted: true },
    DomainCase { name: "slowlog-log-slower-than", value: "0", accepted: true },
    DomainCase { name: "slowlog-log-slower-than", value: "5000000", accepted: true },
    // Open at the top only: the published bottom bound holds, one below it does not.
    DomainCase { name: "slowlog-max-len", value: "0", accepted: true },
    DomainCase { name: "slowlog-max-len", value: "1000000", accepted: true },
    DomainCase { name: "slowlog-max-len", value: "-1", accepted: false },
    // Closed at both ends: both bounds hold, one past either does not.
    DomainCase { name: "search-query-timeout-ms", value: "1", accepted: true },
    DomainCase { name: "search-query-timeout-ms", value: "60000", accepted: true },
    DomainCase { name: "search-query-timeout-ms", value: "0", accepted: false },
    DomainCase { name: "search-query-timeout-ms", value: "60001", accepted: false },
    DomainCase { name: "tracking-table-max-keys", value: "1", accepted: true },
    DomainCase { name: "tracking-table-max-keys", value: "100000000", accepted: true },
    DomainCase { name: "tracking-table-max-keys", value: "100000001", accepted: false },
    // Two disjoint ranges: each range's bound holds, the gap between them does not.
    DomainCase { name: "timeout", value: "0", accepted: true },
    DomainCase { name: "timeout", value: "20", accepted: true },
    DomainCase { name: "timeout", value: "19", accepted: false },
];

/// What a parameter PUBLISHES in `AllowedValues` is what an update enforces for it: a
/// value the expression holds is accepted and read back, and one it does not is refused
/// with the modelled error and that same expression quoted — so a domain open at the
/// bottom accepts the negative value that disables the setting it belongs to.
#[tokio::test]
async fn an_update_enforces_exactly_the_value_set_the_parameter_publishes() {
    let router = router();
    const GROUP: &str = "published-domains";
    create_group(&router, GROUP, VALKEY_FAMILY).await;
    let reported = parameters(&router, GROUP).await;

    for case_row in DOMAIN_CASES {
        let case = format!("{}={}", case_row.name, case_row.value);
        let allowed = published_domain(&reported, case_row.name, &case)
            .unwrap_or_else(|| panic!("case {case:?}: a numeric setting publishes its range"));
        let before = field(&parameters(&router, GROUP).await[case_row.name], "Value", &case);

        let (status, answered) =
            update(&router, GROUP, &[(case_row.name, case_row.value)]).await;
        let read_back = field(&parameters(&router, GROUP).await[case_row.name], "Value", &case);

        if case_row.accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: {:?} lies in the published {allowed:?} and must be \
                 accepted, got {status} with {answered}",
                case_row.value
            );
            assert_eq!(
                read_back, case_row.value,
                "case {case:?}: expected the accepted value to persist, got {read_back:?}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "InvalidParameterValueException"),
                "case {case:?}: {:?} is outside the published {allowed:?} and must be \
                 refused, got {status} with {answered}",
                case_row.value
            );
            assert_eq!(
                error_message(&answered, &case),
                format!("value:{} is outside of range:{allowed}", case_row.value),
                "case {case:?}: the refusal must quote the published value set"
            );
            assert_eq!(
                read_back, before,
                "case {case:?}: a refused value must not be persisted, got {read_back:?}"
            );
        }
    }
}

/// A describe is a REPORT: reading a group's parameters leaves the published metadata and
/// every reported value byte-identical for a later caller, whichever group is read.
#[tokio::test]
async fn reading_a_groups_parameters_changes_nothing_a_later_read_reports() {
    let router = router();
    const GROUP: &str = "published-reread";
    create_group(&router, GROUP, VALKEY_FAMILY).await;

    for source in [default_group_name(VALKEY_FAMILY), GROUP.to_string()] {
        let first = parameters(&router, &source).await;
        let second = parameters(&router, &source).await;
        assert_eq!(
            first, second,
            "case {source:?}: a describe must report the same parameters to a later caller"
        );
        assert!(
            !first.is_empty(),
            "case {source:?}: the group must report its family's engine defaults"
        );
    }

    // The update of one setting leaves every other setting's published record alone.
    let before = parameters(&router, GROUP).await;
    let (status, answered) = update(&router, GROUP, &[("slowlog-log-slower-than", "-1")]).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected the update to be served, got {status} with {answered}",
        "disable the slow log"
    );
    let after = parameters(&router, GROUP).await;
    for (name, parameter) in &before {
        if name == "slowlog-log-slower-than" {
            continue;
        }
        assert_eq!(
            after.get(name),
            Some(parameter),
            "case {name:?}: an update must leave another setting's record alone"
        );
    }
}
