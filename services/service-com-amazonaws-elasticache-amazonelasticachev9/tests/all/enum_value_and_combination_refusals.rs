// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two refusals this service answers BEFORE a handler reads the
//! request: a value outside a modelled enum, and a filter sent without the parameter it is
//! only served together with.
//!
//! The rules under test:
//!
//! * a value outside a modelled enum is refused as the bad parameter value ElastiCache calls
//!   it — HTTP 400, `InvalidParameterValue`, and the sentence the service words for THAT
//!   member — rather than with the generic constraint prose under a validation code no
//!   operation of this service models, so a client catching the modelled
//!   `InvalidParameterValueException` catches it;
//! * a filter an operation serves only together with another parameter is refused as the
//!   incompatible set it is — HTTP 400, `InvalidParameterCombination` — and that refusal comes
//!   FIRST, because which parameters a request carries does not depend on what they hold: the
//!   same request with a status outside the enum is still refused on the combination.
//!
//! The tests are tables: the rows span the member the bad value is at, the operation that
//! takes it, the position it arrives at inside a list, and the way the value misses the enum
//! — and, for the combination, how many statuses the request names, whether they are inside
//! the enum, and which other filters travel with them. Every assertion goes over the wire
//! through the same router the production emulator mounts.
//!
//! The expected sentences are written out here on purpose: a test that built them from the
//! same constant the service reads would pass whatever that constant said.

use crate::harness::{assert_refusal, params, Emulator};

/// AWS's refusal for a `DescribeEvents$SourceType` outside the modelled `SourceType` enum:
/// the whole sentence is fixed text, listing the event source types the service serves in its
/// own order and interpolating nothing from the request.
const INVALID_EVENT_SOURCE_TYPE: &str = "Invalid event source type. Valid types are \
     cache-cluster,cache-parameter-group,cache-security-group,cache-subnet-group,\
     replication-group,user,user-group,serverless-cache,serverless-cache-snapshot";

/// AWS's refusal for a `ServiceUpdateStatus` outside the modelled enum: fixed text around the
/// status the request sent.
fn invalid_service_update_status(value: &str) -> String {
    format!("The input service update status {value} is not valid.")
}

/// AWS's refusal for an `UpdateActionStatus` outside the modelled enum.
fn invalid_update_action_status(value: &str) -> String {
    format!("The input update action status {value} is not valid.")
}

/// AWS's refusal for an `UpdateActionStatus` filter sent without the service update it
/// narrows: wholly fixed text, naming the two parameters in prose.
const ACTION_STATUS_NEEDS_THE_UPDATE: &str =
    "Update action status needs to be provided together with service update name.";

/// The awsQuery error code ElastiCache names a bad parameter value with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The awsQuery error code ElastiCache names an incompatible set of parameters with.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// One enum-valued member of this API, and everything a row about it needs.
struct MemberUnderTest {
    /// The member as an awsQuery request spells it.
    member: &'static str,
    /// The operations that take it. The sentence is the MEMBER's, so every operation taking
    /// it answers the same one.
    actions: &'static [&'static str],
    /// Whether the member is a LIST of enum members, which a request carries as
    /// `<member>.member.<position>`.
    list: bool,
    /// One value INSIDE the enum, which fills the earlier positions of a list so a bad value
    /// can be sent behind a good one.
    inside: &'static str,
    /// Values outside the enum, with the way each misses it.
    outside: &'static [(&'static str, &'static str)],
    /// The pairs the request needs beside the member for the refusal to be about the member:
    /// the companion parameter a filter is only served with.
    companion: &'static [(&'static str, &'static str)],
    /// The whole sentence the refusal carries, for a given rejected value.
    sentence: fn(&str) -> String,
}

/// The members whose enum refusal this service words itself, with the ways a value misses
/// each enum. The vocabularies are the model's
/// (`smithy_query(query="shape", shape="SourceType,ServiceUpdateStatus,UpdateActionStatus")`:
/// `SourceType` = cache-cluster … serverless-cache-snapshot, `ServiceUpdateStatus` =
/// available | cancelled | expired, `UpdateActionStatus` = not-applied … not-applicable), so
/// each "outside" value below is outside its own enum while being a real value of another, a
/// real value in the wrong case, a word no vocabulary holds, or nothing at all.
const MEMBERS: &[MemberUnderTest] = &[
    MemberUnderTest {
        member: "SourceType",
        actions: &["DescribeEvents"],
        list: false,
        inside: "cache-cluster",
        outside: &[
            ("a word no vocabulary holds", "notarealtype"),
            ("a value of another enum", "not-applied"),
            ("a real type in the wrong case", "CACHE-CLUSTER"),
            ("a type the service does not serve", "cache-node"),
            ("nothing at all", ""),
        ],
        companion: &[],
        sentence: |_value| INVALID_EVENT_SOURCE_TYPE.to_string(),
    },
    MemberUnderTest {
        member: "ServiceUpdateStatus",
        actions: &["DescribeServiceUpdates", "DescribeUpdateActions"],
        list: true,
        inside: "available",
        outside: &[
            ("a word no vocabulary holds", "notarealstatus"),
            ("a value of another enum", "not-applied"),
            ("a real status in the wrong case", "Available"),
            ("nothing at all", ""),
        ],
        companion: &[],
        sentence: invalid_service_update_status,
    },
    MemberUnderTest {
        member: "UpdateActionStatus",
        actions: &["DescribeUpdateActions"],
        list: true,
        inside: "not-applied",
        outside: &[
            ("a word no vocabulary holds", "notarealstatus"),
            ("a value of another enum", "available"),
            ("a real status in the wrong case", "Not-Applied"),
            ("nothing at all", ""),
        ],
        // An action status is only served together with the update it narrows, and that rule
        // is answered first — so a row about the VALUE names an update.
        companion: &[("ServiceUpdateName", "an-update-this-row-does-not-read")],
        sentence: invalid_update_action_status,
    },
];

/// A value outside a modelled enum is refused as the bad parameter value it is, with the
/// sentence the service words for that member, at every operation taking it and whatever
/// position it arrives at.
#[tokio::test]
async fn a_value_outside_a_modelled_enum_is_refused_as_a_bad_parameter_value() {
    let emulator = Emulator::new();

    for member in MEMBERS {
        for action in member.actions {
            for (misses, value) in member.outside {
                // The positions a bad value arrives at: a scalar has one, and a list is tried
                // at its first position and behind a value that IS inside the enum, so the
                // refusal is shown to be about the value rather than about the position.
                let positions: &[usize] = if member.list { &[1, 2] } else { &[0] };
                for position in positions {
                    let mut pairs = params(member.companion);
                    let case = match position {
                        0 => {
                            pairs.extend(params(&[(member.member, value)]));
                            format!("{action} {} with {misses}", member.member)
                        }
                        1 => {
                            pairs.extend(params(&[(
                                &format!("{}.member.1", member.member),
                                value,
                            )]));
                            format!("{action} {}.member.1 with {misses}", member.member)
                        }
                        _ => {
                            pairs.extend(params(&[
                                (&format!("{}.member.1", member.member), member.inside),
                                (&format!("{}.member.2", member.member), value),
                            ]));
                            format!(
                                "{action} {}.member.2 behind a good value with {misses}",
                                member.member
                            )
                        }
                    };
                    let refused = emulator.call(action, &pairs).await;
                    assert_refusal(
                        &case,
                        &refused,
                        400,
                        INVALID_PARAMETER_VALUE,
                        &(member.sentence)(value),
                    );
                }
            }

            // A value INSIDE the enum is served at the same member, so the rule above is
            // about the value rather than about the member being filtered on at all.
            let mut pairs = params(member.companion);
            let name = if member.list {
                format!("{}.member.1", member.member)
            } else {
                member.member.to_string()
            };
            pairs.extend(params(&[(name.as_str(), member.inside)]));
            let served = emulator.call(action, &pairs).await;
            assert_eq!(
                served.status,
                200,
                "case {action} {name} {}: a value inside the enum must be served, got {} {}",
                member.inside,
                served.status,
                served.body
            );
        }
    }
}

/// The `UpdateActionStatus` filter is refused without the service update it narrows, whatever
/// the statuses it names are — and the refusal is the COMBINATION, not the value.
#[tokio::test]
async fn an_action_status_filter_without_the_service_update_is_refused_on_the_combination() {
    let emulator = Emulator::new();

    // The axis: how many statuses the filter names, whether they are inside the enum, and
    // which other filters travel with them — none of which changes the answer, because the
    // rule is about the parameters the request carries.
    let rows: &[(&str, &[(&str, &str)])] = &[
        (
            "one status inside the enum",
            &[("UpdateActionStatus.member.1", "not-applied")],
        ),
        (
            "two statuses inside the enum",
            &[
                ("UpdateActionStatus.member.1", "not-applied"),
                ("UpdateActionStatus.member.2", "complete"),
            ],
        ),
        (
            "a status outside the enum",
            &[("UpdateActionStatus.member.1", "notarealstatus")],
        ),
        (
            "a status outside the enum behind one inside it",
            &[
                ("UpdateActionStatus.member.1", "in-progress"),
                ("UpdateActionStatus.member.2", "notarealstatus"),
            ],
        ),
        (
            "a status beside the other filters of this operation",
            &[
                ("UpdateActionStatus.member.1", "not-applied"),
                ("ServiceUpdateStatus.member.1", "available"),
                ("Engine", "redis"),
                ("MaxRecords", "20"),
            ],
        ),
        (
            "a status beside a cache cluster id",
            &[
                ("UpdateActionStatus.member.1", "not-applied"),
                ("CacheClusterIds.member.1", "no-such-cluster"),
            ],
        ),
    ];

    for (case, pairs) in rows {
        let refused = emulator.call("DescribeUpdateActions", &params(pairs)).await;
        assert_refusal(
            case,
            &refused,
            400,
            INVALID_PARAMETER_COMBINATION,
            ACTION_STATUS_NEEDS_THE_UPDATE,
        );
    }

    // The one shape that supplies no status at all: a member sent EMPTY. An awsQuery client
    // filtering on nothing sends no member, so an empty one narrows nothing — there is no
    // combination to refuse, and the value is judged as the value it is.
    let empty = emulator
        .call(
            "DescribeUpdateActions",
            &params(&[("UpdateActionStatus.member.1", "")]),
        )
        .await;
    assert_refusal(
        "an empty status with no service update name",
        &empty,
        400,
        INVALID_PARAMETER_VALUE,
        &invalid_update_action_status(""),
    );

    // And the same filter travelling WITH a service update name is served: the rule is the
    // combination, not the filter.
    for (case, pairs) in [
        (
            "a status and a name the catalog does not hold",
            vec![
                ("ServiceUpdateName", "no-such-update"),
                ("UpdateActionStatus.member.1", "not-applied"),
            ],
        ),
        (
            "a name and no status at all",
            vec![("ServiceUpdateName", "no-such-update")],
        ),
        ("no filter at all", Vec::new()),
    ] {
        let served = emulator
            .call("DescribeUpdateActions", &params(&pairs))
            .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: must be served, got {} {}",
            served.status, served.body
        );
    }
}

/// Once the combination holds, the status VALUES are judged — and the actions of the named
/// update are reported, read back through the describe itself.
#[tokio::test]
async fn an_action_status_filter_with_the_service_update_reports_that_update_s_actions() {
    let emulator = Emulator::new();
    // One cache cluster, so the account has a resource for an update action to belong to.
    let created = emulator
        .call(
            "CreateCacheCluster",
            &params(&[
                ("CacheClusterId", "action-status-cluster"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheNodes", "1"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the cluster must be created, got {} {}",
        created.status, created.body
    );

    // What the unfiltered describe reports is where the update name and the status come from:
    // nothing here is written down, so the filtered read is checked against the service's own
    // answer.
    let every = emulator.call("DescribeUpdateActions", &[]).await;
    assert_eq!(
        every.status, 200,
        "the unfiltered describe must be served, got {} {}",
        every.status, every.body
    );
    let update = every
        .text("ServiceUpdateName")
        .expect("the account holding a cluster has an update action")
        .to_string();
    let status = every
        .text("UpdateActionStatus")
        .expect("an update action reports its status")
        .to_string();

    let filtered = emulator
        .call(
            "DescribeUpdateActions",
            &params(&[
                ("ServiceUpdateName", update.as_str()),
                ("UpdateActionStatus.member.1", status.as_str()),
            ]),
        )
        .await;
    assert_eq!(
        filtered.status, 200,
        "case the filter the service's own answer names: must be served, got {} {}",
        filtered.status, filtered.body
    );
    let names: Vec<&str> = filtered.all("ServiceUpdateName");
    assert!(
        !names.is_empty() && names.iter().all(|reported| *reported == update),
        "case the filter the service's own answer names: expected only {update:?}'s actions, \
         got {names:?}"
    );
    let statuses: Vec<&str> = filtered.all("UpdateAction/UpdateActionStatus");
    assert!(
        !statuses.is_empty() && statuses.iter().all(|reported| *reported == status),
        "case the filter the service's own answer names: expected every action in {status:?}, \
         got {statuses:?}"
    );

    // A status outside the enum, with the combination satisfied, is judged as the value it
    // is: the order is combination first, value second.
    let refused = emulator
        .call(
            "DescribeUpdateActions",
            &params(&[
                ("ServiceUpdateName", update.as_str()),
                ("UpdateActionStatus.member.1", "notarealstatus"),
            ]),
        )
        .await;
    assert_refusal(
        "a bad status with the combination satisfied",
        &refused,
        400,
        INVALID_PARAMETER_VALUE,
        &invalid_update_action_status("notarealstatus"),
    );
}
