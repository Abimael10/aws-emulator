// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyPrivateDnsNameOptions`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=ModifyPrivateDnsNameOptions`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test is that the call WRITES EXACTLY THE HOSTNAME OPTIONS THE REQUEST
//! NAMES onto the one instance it names, answers `true`, and leaves every option it does
//! not name as it stands — an option no call has ever set still following the on-launch
//! option of the subnet the instance runs in — read back through `DescribeInstances`, which
//! reports the same stored record as `Instance$PrivateDnsNameOptions`.
//!
//! So the tables below span the CLASS rather than one member: each of the three options of
//! the `PrivateDnsNameOptionsResponse` set is modified on an instance whose launch set all
//! three away from their defaults, both members of the `HostnameType` catalog are asked for,
//! both truth values of each boolean are asked for, all three are asked for at once, and a
//! request that names none is asked too.
//!
//! Five suites:
//!
//! 1. the option table — one modify per row, the `true` it answers, and the record
//!    `DescribeInstances` reports afterwards;
//! 2. the catalog — the words the rows ask for are members of the modelled `HostnameType`
//!    enum, so no row can pin a value the model does not publish;
//! 3. the subnet — an option the instance has never been given follows the subnet's own
//!    on-launch option, before and after a modify of a DIFFERENT option;
//! 4. the lifecycle table — which instance states admit a modify and which answer
//!    `IncorrectInstanceState`;
//! 5. the refusal table — the instance ids, the hostname types outside the catalog and the
//!    `DryRun` probe it refuses, each asserted to leave the instance's options exactly where
//!    they were. Every id this account holds no instance for is the ONE generic
//!    `UnknownResource`, whatever form it is in — a value outside the id form EC2 issues, a
//!    well-formed id of nothing, a long-form id of the shape EC2's examples carry, and the
//!    all-zero placeholder.

use ec2::ops::types::HOSTNAME_TYPE_VALUES;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, HOSTNAME_TYPE_IP_NAME, InstanceStateName};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;
use super::wire::{child_text, document};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// `HostnameType`'s `resource-name` member — "an instance DNS name … based on the instance
/// ID" (`smithy_query(query="shape",shape="HostnameType")`, whose values are `ip-name` and
/// `resource-name`), pinned against that catalog by
/// [`the_words_the_rows_use_are_members_of_the_modelled_catalog`].
const RESOURCE_NAME: &str = "resource-name";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// The three hostname options one instance carries, as every report of them spells them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    hostname_type: String,
    a_record: String,
    aaaa_record: String,
}

impl Options {
    /// The options the launches below ask for: each member set away from the value an
    /// instance whose launch named nothing would report, so an option a modify must leave
    /// alone cannot pass by being re-derived from the subnet's own defaults.
    fn launched() -> Self {
        Self {
            hostname_type: RESOURCE_NAME.to_string(),
            a_record: "true".to_string(),
            aaaa_record: "true".to_string(),
        }
    }

    /// The `PrivateDnsNameOptions.*` launch parameters that ask for these options.
    fn launch_params(&self) -> Vec<(String, String)> {
        vec![
            p("PrivateDnsNameOptions.HostnameType", self.hostname_type.clone()),
            p(
                "PrivateDnsNameOptions.EnableResourceNameDnsARecord",
                self.a_record.clone(),
            ),
            p(
                "PrivateDnsNameOptions.EnableResourceNameDnsAAAARecord",
                self.aaaa_record.clone(),
            ),
        ]
    }
}

/// The hostname options one `<privateDnsNameOptions>` of a response body carries.
fn options_of(case: &str, body: &str) -> Options {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "privateDnsNameOptions")
        .unwrap_or_else(|| {
            panic!("case {case:?}: the body carries no <privateDnsNameOptions>: {body}")
        });
    let text = |member: &str| {
        child_text(node, member).unwrap_or_else(|| {
            panic!("case {case:?}: <privateDnsNameOptions> carries no <{member}>: {body}")
        })
    };
    Options {
        hostname_type: text("hostnameType"),
        a_record: text("enableResourceNameDnsARecord"),
        aaaa_record: text("enableResourceNameDnsAAAARecord"),
    }
}

/// Launch one instance with `params` beside the image and the counts, and answer its id.
async fn launch_with(router: &MultiServiceRouter, case: &str, params: &[(String, String)]) -> String {
    let mut all = vec![p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")];
    all.extend_from_slice(params);
    let (status, body) = ec2_query::call(router, REGION, "RunInstances", &all).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from RunInstances, got {status} with body {body}"
    );
    ec2_query::element(&body, "instanceId").unwrap_or_else(|| {
        panic!("case {case:?}: RunInstances must report the instance it minted: {body}")
    })
}

/// Launch one instance whose hostname options are [`Options::launched`].
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    launch_with(router, case, &Options::launched().launch_params()).await
}

/// One `ModifyPrivateDnsNameOptions` call for `instance_id` with `params` beside it.
async fn modify(
    router: &MultiServiceRouter,
    instance_id: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut all = vec![p("InstanceId", instance_id)];
    all.extend_from_slice(params);
    ec2_query::call(router, REGION, "ModifyPrivateDnsNameOptions", &all).await
}

/// The body `DescribeInstances` answers for one instance.
async fn describe(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = ec2_query::call(
        router,
        REGION,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeInstances, got {status} with body {body}"
    );
    body
}

/// The hostname options `DescribeInstances` reports for one instance.
async fn described(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Options {
    let body = describe(router, case, instance_id).await;
    options_of(case, &body)
}

/// One row of the option table: what the request names, and what the instance's options must
/// be afterwards.
struct Row {
    /// What this row is a case of, named in every assertion of it.
    case: &'static str,
    /// The `ModifyPrivateDnsNameOptions` parameters beside `InstanceId`.
    names: &'static [(&'static str, &'static str)],
    /// The options the instance must carry once the modify is served, as a mutation of
    /// [`Options::launched`].
    expected: fn(Options) -> Options,
}

/// Every option of the set, one at a time, both members of the hostname-type catalog, both
/// truth values of each boolean, all three at once, and a request that names none.
const ROWS: &[Row] = &[
    Row {
        case: "the hostname type back to the address-based name",
        names: &[("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME)],
        expected: |launched| Options {
            hostname_type: HOSTNAME_TYPE_IP_NAME.to_string(),
            ..launched
        },
    },
    Row {
        case: "the hostname type set to the id-based name it already carries",
        names: &[("PrivateDnsHostnameType", RESOURCE_NAME)],
        expected: |launched| launched,
    },
    Row {
        case: "the A record turned off",
        names: &[("EnableResourceNameDnsARecord", "false")],
        expected: |launched| Options {
            a_record: "false".to_string(),
            ..launched
        },
    },
    Row {
        case: "the A record left on",
        names: &[("EnableResourceNameDnsARecord", "true")],
        expected: |launched| launched,
    },
    Row {
        case: "the AAAA record turned off",
        names: &[("EnableResourceNameDnsAAAARecord", "false")],
        expected: |launched| Options {
            aaaa_record: "false".to_string(),
            ..launched
        },
    },
    Row {
        case: "both records turned off together",
        names: &[
            ("EnableResourceNameDnsARecord", "false"),
            ("EnableResourceNameDnsAAAARecord", "false"),
        ],
        expected: |launched| Options {
            a_record: "false".to_string(),
            aaaa_record: "false".to_string(),
            ..launched
        },
    },
    Row {
        case: "every option at once",
        names: &[
            ("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME),
            ("EnableResourceNameDnsARecord", "false"),
            ("EnableResourceNameDnsAAAARecord", "false"),
        ],
        expected: |_launched| Options {
            hostname_type: HOSTNAME_TYPE_IP_NAME.to_string(),
            a_record: "false".to_string(),
            aaaa_record: "false".to_string(),
        },
    },
    Row {
        case: "no option at all",
        names: &[],
        expected: |launched| launched,
    },
];

/// The parameters one row names.
fn named(names: &[(&str, &str)]) -> Vec<(String, String)> {
    names.iter().map(|(key, value)| p(key, *value)).collect()
}

/// The `<return>` of a response body, as the result member this operation answers with
/// ("Returns `true` if the request succeeds").
fn returned(case: &str, body: &str) -> String {
    ec2_query::member(body, "return")
        .unwrap_or_else(|| panic!("case {case:?}: the body carries no <return>: {body}"))
}

/// Rule 1: the call writes exactly the options the request names, leaves every other one as
/// it stands, answers `true`, and `DescribeInstances` reports the same record.
#[tokio::test]
async fn writes_the_options_the_request_names_and_maintains_the_rest() {
    let (router, _engine) = ec2_query::emulator();

    // One instance per row, all launched with the same non-default options.
    let mut instances = Vec::new();
    for row in ROWS {
        instances.push(launch(&router, row.case).await);
    }

    for (row, instance_id) in ROWS.iter().zip(&instances) {
        let case = row.case;
        let expected = (row.expected)(Options::launched());

        let (status, body) = modify(&router, instance_id, &named(row.names)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the modify, got {status} with body {body}"
        );
        let answered = returned(case, &body);
        assert_eq!(
            answered, "true",
            "case {case:?}: expected the modify to answer true, got {answered:?} in {body}"
        );

        let reported = described(&router, case, instance_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected DescribeInstances to report {expected:?}, got {reported:?}"
        );
    }
}

/// Rule 2: the words the rows above ask for are members of the catalog of the member they
/// are asked for, so no row can pin a value the model does not publish.
#[test]
fn the_words_the_rows_use_are_members_of_the_modelled_catalog() {
    for word in [HOSTNAME_TYPE_IP_NAME, RESOURCE_NAME] {
        assert!(
            HOSTNAME_TYPE_VALUES.contains(&word),
            "case {word:?}: expected the catalog {HOSTNAME_TYPE_VALUES:?} to publish it, it \
             does not"
        );
    }
    assert_eq!(
        HOSTNAME_TYPE_VALUES.len(),
        2,
        "case \"the HostnameType catalog\": expected the two members the model publishes, got \
         {HOSTNAME_TYPE_VALUES:?}"
    );
}

/// Rule 3: an option the instance has never been given is not a copy of anything — it
/// follows the on-launch option of the SUBNET the instance runs in, before and after a
/// modify of a DIFFERENT option, while an instance that WAS given that option keeps its own.
#[tokio::test]
async fn an_option_no_call_has_set_follows_the_subnet() {
    let case = "an option the instance was never given";
    let (router, _engine) = ec2_query::emulator();

    // One instance whose launch named no hostname options at all, and one whose launch named
    // every one of them.
    let follower = launch_with(&router, case, &[]).await;
    let own = launch(&router, case).await;

    let body = describe(&router, case, &follower).await;
    let subnet_id = ec2_query::element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: the instance must report its subnet: {body}"));
    let before = options_of(case, &body);
    assert_eq!(
        before.hostname_type, HOSTNAME_TYPE_IP_NAME,
        "case {case:?}: expected the subnet's own on-launch hostname type, got {before:?}"
    );

    // The one option this instance is ever given.
    let (status, body) = modify(
        &router,
        &follower,
        &[p("EnableResourceNameDnsARecord", "true")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the modify, got {status} with body {body}"
    );

    // The subnet's on-launch hostname type now moves.
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "ModifySubnetAttribute",
        &[
            p("SubnetId", subnet_id.clone()),
            p("PrivateDnsHostnameTypeOnLaunch", RESOURCE_NAME),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ModifySubnetAttribute, got {status} with body \
         {body}"
    );

    let followed = described(&router, case, &follower).await;
    assert_eq!(
        followed,
        Options {
            hostname_type: RESOURCE_NAME.to_string(),
            a_record: "true".to_string(),
            aaaa_record: "false".to_string(),
        },
        "case {case:?}: the option the modify wrote is the instance's own and the two it never \
         named follow the subnet, got {followed:?}"
    );

    let kept = described(&router, case, &own).await;
    assert_eq!(
        kept,
        Options::launched(),
        "case {case:?}: the instance that was given every option keeps its own, got {kept:?}"
    );
}

/// Rule 4: an instance on its way to `terminated`, or already there, has no hostname left to
/// answer DNS queries for, so it answers `IncorrectInstanceState` and keeps its options;
/// every other state admits the modify.
#[tokio::test]
async fn only_an_instance_that_is_not_terminating_admits_a_modify() {
    let states = [
        (InstanceStateName::Pending, true),
        (InstanceStateName::Running, true),
        (InstanceStateName::Stopping, true),
        (InstanceStateName::Stopped, true),
        (InstanceStateName::ShuttingDown, false),
        (InstanceStateName::Terminated, false),
    ];
    for (state, admitted) in states {
        let case = format!("a modify of an instance in {}", state.as_str());
        let (router, engine) = ec2_query::emulator();
        let instance_id = launch(&router, &case).await;
        {
            let store = engine.state::<Ec2State>();
            let mut guard = store.lock();
            let instance = guard.instances.get_mut(&instance_id).unwrap_or_else(|| {
                panic!("case {case:?}: the store must hold the launched instance {instance_id}")
            });
            instance.state = state;
            instance.pending_transition = None;
        }

        let (status, body) = modify(
            &router,
            &instance_id,
            &[p("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME)],
        )
        .await;
        if admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            let reported = described(&router, &case, &instance_id).await;
            assert_eq!(
                reported.hostname_type, HOSTNAME_TYPE_IP_NAME,
                "case {case:?}: expected the hostname type {HOSTNAME_TYPE_IP_NAME:?}, got \
                 {reported:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            ec2_query::error_code(&body),
            "IncorrectInstanceState",
            "case {case:?}: expected the state refusal, got {body}"
        );
        assert_eq!(
            ec2_query::error_message(&body),
            format!(
                "The instance '{instance_id}' is not in a state from which it can be modified."
            ),
            "case {case:?}: expected the refusal to name the instance and the action: {body}"
        );
        let reported = described(&router, &case, &instance_id).await;
        assert_eq!(
            reported,
            Options::launched(),
            "case {case:?}: a refused modify must leave the options as they were, got \
             {reported:?}"
        );
    }
}

/// One row of the refusal table.
struct Refused {
    case: &'static str,
    /// The `InstanceId` the request names, or `None` for the instance this suite launched.
    instance_id: Option<&'static str>,
    /// The parameters beside it.
    names: &'static [(&'static str, &'static str)],
    status: u16,
    code: &'static str,
    /// The whole sentence the refusal must carry, as a template of the id the request named,
    /// or `None` where only the class of the refusal is this operation's to state.
    message: Option<fn(&str) -> String>,
}

/// The whole sentence an `InstanceId` this account holds no instance for is refused with, as
/// a template of the value the request sent: the caller's own account and that value.
fn unknown_resource_refusal(named: &str) -> String {
    format!(
        "User ({account}) does not own a resource {named} .",
        account = engine::emulator_account_id()
    )
}

/// The one code this operation refuses an `InstanceId` it cannot resolve with, whatever form
/// that value is in — the generic not-found class, rather than the
/// `InvalidInstanceID.Malformed` / `.NotFound` pair most readers of one named instance split
/// an unresolvable id into.
const UNKNOWN_RESOURCE: &str = "UnknownResource";

/// The ids, hostname types and probes a modify refuses.
const REFUSALS: &[Refused] = &[
    Refused {
        case: "an instance id outside the form EC2 issues",
        instance_id: Some("not-an-instance"),
        names: &[("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME)],
        status: 400,
        code: UNKNOWN_RESOURCE,
        message: Some(unknown_resource_refusal),
    },
    Refused {
        case: "a well-formed instance id this account holds no instance for",
        instance_id: Some("i-0fedcba9876543210"),
        names: &[("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME)],
        status: 400,
        code: UNKNOWN_RESOURCE,
        message: Some(unknown_resource_refusal),
    },
    Refused {
        case: "a long-form instance id of the shape EC2's own examples carry",
        instance_id: Some("i-1234567890abcdef0"),
        names: &[("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME)],
        status: 400,
        code: UNKNOWN_RESOURCE,
        message: Some(unknown_resource_refusal),
    },
    Refused {
        case: "the all-zero instance id no mint ever hands out",
        instance_id: Some("i-00000000000000000"),
        names: &[("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME)],
        status: 400,
        code: UNKNOWN_RESOURCE,
        message: Some(unknown_resource_refusal),
    },
    Refused {
        // The modelled `HostnameType` enum constraint refuses a non-member BEFORE the
        // handler runs, with the boundary's own code for a value outside a declared
        // constraint.
        case: "a hostname type the model does not define",
        instance_id: None,
        names: &[("PrivateDnsHostnameType", "instance-name")],
        status: 400,
        code: "ValidationError",
        message: None,
    },
    Refused {
        case: "a hostname type that is a member in the wrong casing",
        instance_id: None,
        names: &[("PrivateDnsHostnameType", "IP-Name")],
        status: 400,
        code: "ValidationError",
        message: None,
    },
    Refused {
        case: "a dry run of a request that would have succeeded",
        instance_id: None,
        names: &[
            ("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME),
            ("EnableResourceNameDnsARecord", "false"),
            ("DryRun", "true"),
        ],
        status: 412,
        code: "DryRunOperation",
        message: Some(|_named| "Request would have succeeded, but DryRun flag is set.".to_string()),
    },
];

/// Rule 5: every refusal answers its own status and code — and, where the sentence is this
/// operation's own, that sentence — and leaves the instance's options exactly where they
/// were.
#[tokio::test]
async fn a_refused_modify_writes_nothing() {
    let (router, _engine) = ec2_query::emulator();
    let instance_id = launch(&router, "the instance every refusal is asked about").await;

    for row in REFUSALS {
        let case = row.case;
        let named_id = row.instance_id.unwrap_or(instance_id.as_str());
        let (status, body) = modify(&router, named_id, &named(row.names)).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {}, got {status} with body {body}",
            row.status
        );
        assert_eq!(
            ec2_query::error_code(&body),
            row.code,
            "case {case:?}: expected the code {:?}, got {body}",
            row.code
        );
        if let Some(message) = row.message {
            assert_eq!(
                ec2_query::error_message(&body),
                message(named_id),
                "case {case:?}: expected the sentence {:?}, got {body}",
                message(named_id)
            );
        }

        let reported = described(&router, case, &instance_id).await;
        assert_eq!(
            reported,
            Options::launched(),
            "case {case:?}: a refused modify must leave the options as they were, got \
             {reported:?}"
        );
    }
}

/// Rule 6: nothing but the one instance the request names is touched — a second instance of
/// the same account keeps the options its own launch chose.
#[tokio::test]
async fn a_modify_touches_only_the_instance_it_names() {
    let case = "two instances, one modified";
    let (router, _engine) = ec2_query::emulator();
    let modified = launch(&router, case).await;
    let untouched = launch(&router, case).await;

    let (status, body) = modify(
        &router,
        &modified,
        &[
            p("PrivateDnsHostnameType", HOSTNAME_TYPE_IP_NAME),
            p("EnableResourceNameDnsAAAARecord", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the modify, got {status} with body {body}"
    );

    let reported = described(&router, case, &untouched).await;
    assert_eq!(
        reported,
        Options::launched(),
        "case {case:?}: the instance the request did not name keeps its own options, got \
         {reported:?}"
    );
    let changed = described(&router, case, &modified).await;
    assert_eq!(
        changed,
        Options {
            hostname_type: HOSTNAME_TYPE_IP_NAME.to_string(),
            aaaa_record: "false".to_string(),
            ..Options::launched()
        },
        "case {case:?}: the instance the request named carries the modified options, got \
         {changed:?}"
    );
}
