// SPDX-License-Identifier: CC0-1.0

//! Amazon EC2 Auto Scaling reports the tags its groups carry through `DescribeTags`.
//!
//! The RULE under test: `DescribeTags` answers HTTP 200 with the tags the account's Auto
//! Scaling groups hold, each projected as a `TagDescription` (the group as `ResourceId`,
//! `auto-scaling-group` as `ResourceType`, the key, the value and whether it propagates at
//! launch), narrowed by the filter types the model documents for this operation —
//! `auto-scaling-group`, `key`, `value` and `propagate-at-launch` — whose values are joined
//! with OR and whose filters are joined with AND, and paged by `MaxRecords`/`NextToken`. A
//! request that matches nothing answers an EMPTY list ("If there's no match, no special
//! message is returned"), never a refusal, and an unusable criterion — an unknown filter
//! type, a filter with no values, a `MaxRecords` outside its documented range, a
//! `NextToken` this service never issued — is refused with HTTP 400.
//!
//! The rows span what the rule generalises over: which filter type narrows the read, how
//! many values one filter carries, how many filters are combined, how many groups and tags
//! are in play, and whether anything matches at all. The tags are written through
//! `CreateAutoScalingGroup` and through the two operations that write a group's tags —
//! `CreateOrUpdateTags` and `DeleteTags` — and read back here, so every assertion follows
//! state the service stored rather than a value this suite invented.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;

use crate::support::{borrowed, ec2_query, p, query, seed_subnets, tag_value, tag_values};

/// The `ResourceType` every tag of an Auto Scaling group is reported under, from the
/// model's own documentation for `TagDescription$ResourceType`: "The only supported value
/// is auto-scaling-group". Spelled out here rather than imported, so a wrong constant in
/// the implementation cannot make a test pass.
const GROUP_RESOURCE_TYPE: &str = "auto-scaling-group";

/// The two groups every row reads over, and the tags each was created with: three tags
/// across two groups, differing in key, in value and in whether they propagate at launch.
const ALPHA: &str = "tags-alpha";
const BETA: &str = "tags-beta";

/// One tag of a group: its key, its value and its `PropagateAtLaunch`.
struct SeededTag {
    group: &'static str,
    key: &'static str,
    value: &'static str,
    propagate: bool,
}

const SEEDED: &[SeededTag] = &[
    SeededTag {
        group: ALPHA,
        key: "env",
        value: "prod",
        propagate: true,
    },
    SeededTag {
        group: ALPHA,
        key: "team",
        value: "platform",
        propagate: false,
    },
    SeededTag {
        group: BETA,
        key: "env",
        value: "staging",
        propagate: true,
    },
];

/// One emulator: Auto Scaling and Amazon EC2 on one engine, because a group is created
/// from an EC2 launch template placed in EC2 subnets.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One tag as a row of this suite's expectations: group, key, value and propagate flag.
fn rendered(group: &str, key: &str, value: &str, propagate: bool) -> String {
    format!("{group}|{key}|{value}|{propagate}")
}

/// Every `TagDescription` in an answered body, in the order the service reported them, and
/// in the form [`rendered`] writes one.
///
/// Each member carries all five projected values, so the per-member lists line up: a
/// member missing one would shift the others and fail the assertion loudly rather than
/// quietly.
fn described_tags(case: &str, body: &str) -> Vec<String> {
    let resource_ids = tag_values(body, "ResourceId");
    let resource_types = tag_values(body, "ResourceType");
    let keys = tag_values(body, "Key");
    let values = tag_values(body, "Value");
    let propagate = tag_values(body, "PropagateAtLaunch");
    assert!(
        resource_ids.len() == keys.len()
            && keys.len() == values.len()
            && values.len() == propagate.len()
            && propagate.len() == resource_types.len(),
        "case {case:?}: every reported tag carries ResourceId, ResourceType, Key, Value \
         and PropagateAtLaunch, got {body}"
    );
    for resource_type in &resource_types {
        assert_eq!(
            resource_type, GROUP_RESOURCE_TYPE,
            "case {case:?}: expected ResourceType {GROUP_RESOURCE_TYPE}, got {body}"
        );
    }
    resource_ids
        .iter()
        .zip(keys.iter())
        .zip(values.iter())
        .zip(propagate.iter())
        .map(|(((resource_id, key), value), propagate)| {
            format!("{resource_id}|{key}|{value}|{propagate}")
        })
        .collect()
}

/// Create the two groups and their tags through `CreateAutoScalingGroup`, over the wire,
/// and answer the subnet they are placed in — for a case that needs one more group beside
/// them.
async fn given_tagged_groups(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
) -> String {
    let subnets = seed_subnets(engine, 1);
    let subnet = subnets
        .first()
        .expect("one seeded subnet")
        .id
        .clone();
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "tags-template"),
            p("LaunchTemplateData.ImageId", "ami-0abcdef1234567890"),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplate expected 200, got {status}: {body}"
    );

    for group in [ALPHA, BETA] {
        let mut params = vec![
            p("AutoScalingGroupName", group),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("LaunchTemplate.LaunchTemplateName", "tags-template"),
            p("VPCZoneIdentifier", subnet.clone()),
        ];
        for (position, tag) in SEEDED
            .iter()
            .filter(|tag| tag.group == group)
            .enumerate()
        {
            let member = position + 1;
            params.push(p(&format!("Tags.member.{member}.Key"), tag.key));
            params.push(p(&format!("Tags.member.{member}.Value"), tag.value));
            params.push(p(
                &format!("Tags.member.{member}.PropagateAtLaunch"),
                if tag.propagate { "true" } else { "false" },
            ));
        }
        let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "CreateAutoScalingGroup for {group} expected 200, got {status}: {body}"
        );
    }
    subnet
}

/// One row of the filter table: the request's filter parameters, and the tags it selects.
struct Row {
    case: &'static str,
    /// `Filters.member.N.*` parameters, as a client sends them.
    filters: &'static [(&'static str, &'static str)],
    /// The tags the row expects, in the order the service reports them: by the group the
    /// tag is on, then by key.
    expected: &'static [(&'static str, &'static str, &'static str, bool)],
}

const ROWS: &[Row] = &[
    Row {
        case: "no filters at all: every tag of every group",
        filters: &[],
        expected: &[
            (ALPHA, "env", "prod", true),
            (ALPHA, "team", "platform", false),
            (BETA, "env", "staging", true),
        ],
    },
    Row {
        case: "the auto-scaling-group filter, one value",
        filters: &[
            ("Filters.member.1.Name", "auto-scaling-group"),
            ("Filters.member.1.Values.member.1", ALPHA),
        ],
        expected: &[
            (ALPHA, "env", "prod", true),
            (ALPHA, "team", "platform", false),
        ],
    },
    Row {
        case: "the auto-scaling-group filter, both groups as values",
        filters: &[
            ("Filters.member.1.Name", "auto-scaling-group"),
            ("Filters.member.1.Values.member.1", ALPHA),
            ("Filters.member.1.Values.member.2", BETA),
        ],
        expected: &[
            (ALPHA, "env", "prod", true),
            (ALPHA, "team", "platform", false),
            (BETA, "env", "staging", true),
        ],
    },
    Row {
        case: "the key filter, selecting one key held by two groups",
        filters: &[
            ("Filters.member.1.Name", "key"),
            ("Filters.member.1.Values.member.1", "env"),
        ],
        expected: &[(ALPHA, "env", "prod", true), (BETA, "env", "staging", true)],
    },
    Row {
        case: "the key filter, two values joined with OR",
        filters: &[
            ("Filters.member.1.Name", "key"),
            ("Filters.member.1.Values.member.1", "env"),
            ("Filters.member.1.Values.member.2", "team"),
        ],
        expected: &[
            (ALPHA, "env", "prod", true),
            (ALPHA, "team", "platform", false),
            (BETA, "env", "staging", true),
        ],
    },
    Row {
        case: "the value filter",
        filters: &[
            ("Filters.member.1.Name", "value"),
            ("Filters.member.1.Values.member.1", "staging"),
        ],
        expected: &[(BETA, "env", "staging", true)],
    },
    Row {
        case: "the propagate-at-launch filter, true",
        filters: &[
            ("Filters.member.1.Name", "propagate-at-launch"),
            ("Filters.member.1.Values.member.1", "true"),
        ],
        expected: &[(ALPHA, "env", "prod", true), (BETA, "env", "staging", true)],
    },
    Row {
        case: "the propagate-at-launch filter, false",
        filters: &[
            ("Filters.member.1.Name", "propagate-at-launch"),
            ("Filters.member.1.Values.member.1", "false"),
        ],
        expected: &[(ALPHA, "team", "platform", false)],
    },
    Row {
        case: "two filters joined with AND",
        filters: &[
            ("Filters.member.1.Name", "key"),
            ("Filters.member.1.Values.member.1", "env"),
            ("Filters.member.2.Name", "value"),
            ("Filters.member.2.Values.member.1", "staging"),
        ],
        expected: &[(BETA, "env", "staging", true)],
    },
    Row {
        case: "two filters whose AND selects nothing",
        filters: &[
            ("Filters.member.1.Name", "auto-scaling-group"),
            ("Filters.member.1.Values.member.1", BETA),
            ("Filters.member.2.Name", "key"),
            ("Filters.member.2.Values.member.1", "team"),
        ],
        expected: &[],
    },
    Row {
        case: "a key no tag carries",
        filters: &[
            ("Filters.member.1.Name", "key"),
            ("Filters.member.1.Values.member.1", "unheld"),
        ],
        expected: &[],
    },
    Row {
        case: "a group the account does not hold",
        filters: &[
            ("Filters.member.1.Name", "auto-scaling-group"),
            ("Filters.member.1.Values.member.1", "absent-group"),
        ],
        expected: &[],
    },
];

#[tokio::test]
async fn describe_tags_reports_the_stored_tags_the_filters_select() {
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    for row in ROWS {
        let case = row.case;
        let (status, body) = query(&router, "DescribeTags", row.filters).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTags expected 200, got {status}: {body}"
        );
        let expected: Vec<String> = row
            .expected
            .iter()
            .map(|(group, key, value, propagate)| rendered(group, key, value, *propagate))
            .collect();
        assert_eq!(
            described_tags(case, &body),
            expected,
            "case {case:?}: expected the tags {expected:?}, got {body}"
        );
        assert!(
            tag_values(&body, "NextToken").is_empty(),
            "case {case:?}: a page that holds every match issues no NextToken, got {body}"
        );
    }
}

#[tokio::test]
async fn the_propagate_at_launch_filter_accepts_only_a_boolean() {
    // The filter selects on a BOOLEAN member, so a value that is not one of the two
    // spellings of a Boolean states no criterion at all and the read is REFUSED; only
    // `true` and `false` select. A tag created without `PropagateAtLaunch` stored the
    // decision the service takes for it — the member's own default, which is that the tag
    // propagates — so it is selected by `true` alone.
    let case = "a tag created with no PropagateAtLaunch";
    let (router, engine) = emulator();
    let subnet = given_tagged_groups(&router, &engine).await;
    let flagless_group = "tags-flagless";
    let flagless_key = "unflagged";
    let params = vec![
        p("AutoScalingGroupName", flagless_group),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("LaunchTemplate.LaunchTemplateName", "tags-template"),
        p("VPCZoneIdentifier", subnet),
        p("Tags.member.1.Key", flagless_key),
        p("Tags.member.1.Value", "unset"),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );

    // The tag IS in the store: a filter on its key reports it, so the exclusions below are
    // the filter's doing and not an absent tag.
    let (status, body) = query(
        &router,
        "DescribeTags",
        &[
            ("Filters.member.1.Name", "key"),
            ("Filters.member.1.Values.member.1", flagless_key),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTags expected 200, got {status}: {body}"
    );
    assert!(
        body.contains(flagless_key),
        "case {case:?}: the stored tag must be reported by a filter on its key, got {body}"
    );

    // (the filter value, whether it selects the tag that was created without the flag)
    let rows: &[(&str, bool)] = &[
        // The decision stored for a tag whose create stated none: it propagates.
        ("true", true),
        ("false", false),
    ];
    for (value, selects) in rows {
        let (status, body) = query(
            &router,
            "DescribeTags",
            &[
                ("Filters.member.1.Name", "propagate-at-launch"),
                ("Filters.member.1.Values.member.1", value),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?} / {value:?}: DescribeTags expected 200, got {status}: {body}"
        );
        assert_eq!(
            body.contains(flagless_key),
            *selects,
            "case {case:?} / {value:?}: the filter selects the tag created without the \
             flag: {selects}, got {body}"
        );
    }

    // Everything else is not a Boolean and is refused, whatever it resembles: another
    // casing of one, a number, a word that means one, and the empty value.
    for value in ["yes", "1", "0", "True", "FALSE", "false ", ""] {
        let (status, body) = query(
            &router,
            "DescribeTags",
            &[
                ("Filters.member.1.Name", "propagate-at-launch"),
                ("Filters.member.1.Values.member.1", value),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?} / {value:?}: a value that is not a Boolean is refused, got \
             {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {case:?} / {value:?}: expected the code ValidationError, got {body}"
        );
        assert_eq!(
            crate::support::xml_unescape(&tag_value(&body, "Message")),
            NON_BOOLEAN_MESSAGE,
            "case {case:?} / {value:?}: expected the message {NON_BOOLEAN_MESSAGE:?}, got \
             {body}"
        );
    }
}

#[tokio::test]
async fn describe_tags_pages_over_the_tags_it_selects() {
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    // The whole selection, in the order the service reports it, so the pages below are
    // checked against the list they page over rather than against a written-out order.
    let (status, body) = query(&router, "DescribeTags", &[]).await;
    assert_eq!(status, 200, "DescribeTags expected 200, got {status}: {body}");
    let whole = described_tags("the whole selection", &body);
    assert_eq!(
        whole.len(),
        SEEDED.len(),
        "the seeded tags are the whole selection, got {whole:?}"
    );

    // One page per tag, each followed by the token that resumes the walk.
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for step in 0..SEEDED.len() {
        let case = format!("page {step} of one tag");
        let mut params = vec![p("MaxRecords", "1")];
        if let Some(token) = token.as_ref() {
            params.push(p("NextToken", token.clone()));
        }
        let (status, body) = query(&router, "DescribeTags", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTags expected 200, got {status}: {body}"
        );
        let page = described_tags(&case, &body);
        assert_eq!(
            page.len(),
            1,
            "case {case:?}: MaxRecords 1 answers one tag, got {page:?}"
        );
        collected.extend(page);
        let next = tag_value(&body, "NextToken");
        token = (!next.is_empty()).then_some(next);
        assert_eq!(
            token.is_some(),
            step + 1 < SEEDED.len(),
            "case {case:?}: a token follows every page but the last, got {body}"
        );
    }
    assert_eq!(
        collected, whole,
        "the pages walk the same selection the unpaged read answers, got {collected:?}"
    );
}

/// One refused request: the parameters, the error code and the WHOLE message the service
/// states for it — each of these refusals is one determined sentence, so a row states it in
/// full rather than a fragment of it.
struct Refusal {
    case: &'static str,
    params: &'static [(&'static str, &'static str)],
    code: &'static str,
    message: &'static str,
}

/// The sentence a filter type this read does not accept is refused with, spelled out here
/// rather than imported: the offending type is its only variable part and the accepted
/// types are fixed text, space separated.
fn unknown_filter_type(name: &str) -> String {
    format!(
        "Filter type {name} is not correct. Allowed Filter types are: auto-scaling-group key \
         value propagate-at-launch"
    )
}

/// The sentence a value that is not a Boolean is refused with where the parameter takes
/// one: the whole message, with no variable part.
const NON_BOOLEAN_MESSAGE: &str = "Boolean parameter can only be true or false";

/// The filter types that are NOT this read's: one of the OTHER read's tag filters, the
/// documented types in another casing, one carrying surrounding space, and a name no read
/// of this service accepts. None of them states a criterion this read can apply.
const UNKNOWN_FILTER_TYPES: &[&str] = &["tag-key", "tag:env", "Key", "AUTO-SCALING-GROUP", " key", "bogus"];

const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "a filter carrying no Name at all",
        params: &[("Filters.member.1.Values.member.1", "env")],
        code: "ValidationError",
        message: "The filter criteria isn't valid. The Name attribute is required to specify \
                  the type of filter being applied.",
    },
    Refusal {
        case: "a filter carrying no values",
        params: &[("Filters.member.1.Name", "key")],
        code: "ValidationError",
        message: "The filter criteria isn't valid. The Values attribute and one or more filter \
                  values are required.",
    },
    Refusal {
        case: "a MaxRecords below the documented minimum",
        params: &[("MaxRecords", "0")],
        code: "ValidationError",
        message: "1 validation error detected: Value '0' at 'maxRecords' failed to satisfy \
                  constraint: Member must have value greater than or equal to 1",
    },
    Refusal {
        case: "a MaxRecords above the documented maximum",
        params: &[("MaxRecords", "101")],
        code: "ValidationError",
        message: "1 validation error detected: Value '101' at 'maxRecords' failed to satisfy \
                  constraint: Member must have value less than or equal to 100",
    },
    Refusal {
        case: "a NextToken this service never issued",
        params: &[("NextToken", "not-a-token")],
        code: "ValidationError",
        message: "NextToken: not-a-token",
    },
    Refusal {
        case: "a NextToken that points past the end of the selection",
        params: &[("NextToken", "next-99")],
        code: "ValidationError",
        message: "NextToken: next-99",
    },
    Refusal {
        case: "a propagate-at-launch filter whose value is not a Boolean",
        params: &[
            ("Filters.member.1.Name", "propagate-at-launch"),
            ("Filters.member.1.Values.member.1", "maybe"),
        ],
        code: "ValidationError",
        message: NON_BOOLEAN_MESSAGE,
    },
    Refusal {
        case: "a propagate-at-launch filter whose SECOND value is not a Boolean",
        params: &[
            ("Filters.member.1.Name", "propagate-at-launch"),
            ("Filters.member.1.Values.member.1", "true"),
            ("Filters.member.1.Values.member.2", "maybe"),
        ],
        code: "ValidationError",
        message: NON_BOOLEAN_MESSAGE,
    },
];

#[tokio::test]
async fn describe_tags_refuses_every_filter_type_that_is_not_its_own() {
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    for name in UNKNOWN_FILTER_TYPES {
        let (status, body) = query(
            &router,
            "DescribeTags",
            &[
                ("Filters.member.1.Name", name),
                ("Filters.member.1.Values.member.1", "env"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {name:?}: expected the code ValidationError, got {body}"
        );
        let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
        let expected = unknown_filter_type(name);
        assert_eq!(
            message, expected,
            "case {name:?}: expected the message {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn describe_tags_refuses_what_it_cannot_satisfy() {
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    for refusal in REFUSALS {
        let case = refusal.case;
        let (status, body) = query(&router, "DescribeTags", refusal.params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            refusal.code,
            "case {case:?}: expected the code {}, got {body}",
            refusal.code
        );
        let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message, refusal.message,
            "case {case:?}: expected the message {:?}, got {message:?}",
            refusal.message
        );
    }
}

// --- the tags a request WRITES, read back here -------------------------------------
//
// `CreateOrUpdateTags` and `DeleteTags` write the same state this read reports, so what
// they wrote is asserted where a client sees it: through `DescribeTags` and through the
// group's own read.

/// One tag as a tag-writing request carries it: the group it is on, its key, its value, and
/// the propagation decision when the request states one at all.
struct WrittenTag {
    group: &'static str,
    key: &'static str,
    value: &'static str,
    propagate: Option<bool>,
}

/// Those tags as the `Tags.member.N.*` parameters a client sends.
fn tag_params(tags: &[WrittenTag]) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = Vec::new();
    for (position, tag) in tags.iter().enumerate() {
        let member = position + 1;
        params.push(p(&format!("Tags.member.{member}.ResourceId"), tag.group));
        params.push(p(
            &format!("Tags.member.{member}.ResourceType"),
            GROUP_RESOURCE_TYPE,
        ));
        params.push(p(&format!("Tags.member.{member}.Key"), tag.key));
        params.push(p(&format!("Tags.member.{member}.Value"), tag.value));
        if let Some(propagate) = tag.propagate {
            params.push(p(
                &format!("Tags.member.{member}.PropagateAtLaunch"),
                if propagate { "true" } else { "false" },
            ));
        }
    }
    params
}

/// Every tag the account's groups carry, as this read reports them.
async fn described_tags_of_account(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeTags", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTags expected 200, got {status}: {body}"
    );
    described_tags(case, &body)
}

/// The `Tags` of one group as the group's OWN read reports them, in the form
/// [`rendered`] writes one: the same tag seen through the other reader.
async fn group_tags_of(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    described_tags(case, &body)
}

/// One row of the write table: what the call carries, and the whole tag set the account
/// holds afterwards.
struct WriteRow {
    case: &'static str,
    tags: &'static [WrittenTag],
    expected: &'static [(&'static str, &'static str, &'static str, bool)],
}

/// The rows are applied in order against ONE account, each over the state the previous one
/// left: a new key, a key written again with another value, the propagation decision
/// changed, a tag that states no decision at all (which takes the one the service applies,
/// the same default the create path applies), an empty value, and one call that writes to
/// both groups at once.
const WRITE_ROWS: &[WriteRow] = &[
    WriteRow {
        case: "a key neither group carries, propagating",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "owner",
            value: "payments",
            propagate: Some(true),
        }],
        expected: &[
            (ALPHA, "env", "prod", true),
            (ALPHA, "owner", "payments", true),
            (ALPHA, "team", "platform", false),
            (BETA, "env", "staging", true),
        ],
    },
    WriteRow {
        case: "the same key again, with another value and the decision reversed",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "owner",
            value: "billing",
            propagate: Some(false),
        }],
        expected: &[
            (ALPHA, "env", "prod", true),
            (ALPHA, "owner", "billing", false),
            (ALPHA, "team", "platform", false),
            (BETA, "env", "staging", true),
        ],
    },
    WriteRow {
        case: "a key the create of the group wrote, overwritten",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "env",
            value: "canary",
            propagate: Some(true),
        }],
        expected: &[
            (ALPHA, "env", "canary", true),
            (ALPHA, "owner", "billing", false),
            (ALPHA, "team", "platform", false),
            (BETA, "env", "staging", true),
        ],
    },
    WriteRow {
        case: "a tag stating no propagation decision, which takes the one the service applies",
        tags: &[WrittenTag {
            group: BETA,
            key: "unflagged",
            value: "unset",
            propagate: None,
        }],
        expected: &[
            (ALPHA, "env", "canary", true),
            (ALPHA, "owner", "billing", false),
            (ALPHA, "team", "platform", false),
            (BETA, "env", "staging", true),
            (BETA, "unflagged", "unset", true),
        ],
    },
    WriteRow {
        case: "two groups in ONE call, one of them with an empty value",
        tags: &[
            WrittenTag {
                group: ALPHA,
                key: "cost-centre",
                value: "",
                propagate: Some(false),
            },
            WrittenTag {
                group: BETA,
                key: "cost-centre",
                value: "1234",
                propagate: Some(true),
            },
        ],
        expected: &[
            (ALPHA, "cost-centre", "", false),
            (ALPHA, "env", "canary", true),
            (ALPHA, "owner", "billing", false),
            (ALPHA, "team", "platform", false),
            (BETA, "cost-centre", "1234", true),
            (BETA, "env", "staging", true),
            (BETA, "unflagged", "unset", true),
        ],
    },
];

#[tokio::test]
async fn create_or_update_tags_writes_the_tags_every_read_reports() {
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    for row in WRITE_ROWS {
        let case = row.case;
        let params = tag_params(row.tags);
        let (status, body) =
            query(&router, "CreateOrUpdateTags", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateOrUpdateTags expected 200, got {status}: {body}"
        );

        let expected: Vec<String> = row
            .expected
            .iter()
            .map(|(group, key, value, propagate)| rendered(group, key, value, *propagate))
            .collect();
        assert_eq!(
            described_tags_of_account(case, &router).await,
            expected,
            "case {case:?}: expected the account to carry the tags {expected:?}"
        );

        // The group's OWN read reports the same tags: one tag, one stored record, two
        // readers of it. Sorted on both sides, because the group reports its tags in the
        // order it holds them while the account-wide read orders them by group and key.
        for tag in row.tags {
            let mut group_tags = group_tags_of(case, &router, tag.group).await;
            group_tags.sort();
            let mut written: Vec<String> = expected
                .iter()
                .filter(|line| line.starts_with(&format!("{}|", tag.group)))
                .cloned()
                .collect();
            written.sort();
            assert_eq!(
                group_tags, written,
                "case {case:?}: the group {:?} reports the tags {written:?}, got \
                 {group_tags:?}",
                tag.group
            );
        }
    }
}

#[tokio::test]
async fn delete_tags_removes_the_keys_it_names() {
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    // A key one group carries: removed from that group, and from nowhere else — the other
    // group's tag of the SAME key is untouched, because a tag is identified by the group it
    // is on as well as by its key.
    let case = "one key of one group";
    let params = tag_params(&[WrittenTag {
        group: ALPHA,
        key: "env",
        value: "prod",
        propagate: Some(true),
    }]);
    let (status, body) = query(&router, "DeleteTags", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteTags expected 200, got {status}: {body}"
    );
    assert_eq!(
        described_tags_of_account(case, &router).await,
        vec![
            rendered(ALPHA, "team", "platform", false),
            rendered(BETA, "env", "staging", true),
        ],
        "case {case:?}: only the named tag of the named group is gone"
    );

    // Removing it again removes it once: a key the group no longer carries leaves the
    // account exactly as it is, and the call still succeeds.
    let case = "the same key a second time";
    let (status, body) = query(&router, "DeleteTags", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteTags expected 200, got {status}: {body}"
    );
    assert_eq!(
        described_tags_of_account(case, &router).await,
        vec![
            rendered(ALPHA, "team", "platform", false),
            rendered(BETA, "env", "staging", true),
        ],
        "case {case:?}: removing a key the group does not carry changes nothing"
    );

    // A tag is removed BY ITS KEY: a request stating another value, and one stating no
    // value at all, remove the same tag.
    let case = "a key whose request states a value the tag does not carry";
    let params = vec![
        p("Tags.member.1.ResourceId", ALPHA),
        p("Tags.member.1.ResourceType", GROUP_RESOURCE_TYPE),
        p("Tags.member.1.Key", "team"),
        p("Tags.member.1.Value", "not-the-stored-value"),
    ];
    let (status, body) = query(&router, "DeleteTags", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteTags expected 200, got {status}: {body}"
    );
    assert_eq!(
        described_tags_of_account(case, &router).await,
        vec![rendered(BETA, "env", "staging", true)],
        "case {case:?}: the tag the key names is removed whatever value accompanies it"
    );

    let case = "a key stated with no value at all";
    let params = vec![
        p("Tags.member.1.ResourceId", BETA),
        p("Tags.member.1.Key", "env"),
    ];
    let (status, body) = query(&router, "DeleteTags", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteTags expected 200, got {status}: {body}"
    );
    assert!(
        described_tags_of_account(case, &router).await.is_empty(),
        "case {case:?}: the account carries no tag once every one of them is removed"
    );
}

/// One refused tag write: which operation, the parameters, and the whole sentence the
/// refusal states.
#[tokio::test]
async fn a_group_carries_at_most_the_account_quota_of_tags() {
    // The quota Amazon Web Services publishes for the tags of one resource, spelled out
    // here rather than imported: a write that would take a group past it is refused with
    // the modelled limit error, and a write that only overwrites keys the group already
    // carries never crosses it however often it is repeated.
    const QUOTA: usize = 50;
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    // Fill ALPHA, which already carries two tags from its create, up to the quota.
    let mut params: Vec<(String, String)> = Vec::new();
    for member in 1..=(QUOTA - 2) {
        params.push(p(&format!("Tags.member.{member}.ResourceId"), ALPHA));
        params.push(p(
            &format!("Tags.member.{member}.ResourceType"),
            GROUP_RESOURCE_TYPE,
        ));
        params.push(p(&format!("Tags.member.{member}.Key"), format!("fill-{member}")));
        params.push(p(&format!("Tags.member.{member}.Value"), "x"));
    }
    let (status, body) = query(&router, "CreateOrUpdateTags", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "filling the group to the quota expected 200, got {status}: {body}"
    );
    let at_quota = described_tags_of_account("the group at its quota", &router).await;
    assert_eq!(
        at_quota.iter().filter(|tag| tag.starts_with(&format!("{ALPHA}|"))).count(),
        QUOTA,
        "the group carries the whole quota, got {at_quota:?}"
    );

    // Writing a key it already carries only overwrites it, so it stays at the quota.
    let overwrite = tag_params(&[WrittenTag {
        group: ALPHA,
        key: "fill-1",
        value: "y",
        propagate: Some(true),
    }]);
    let (status, body) = query(&router, "CreateOrUpdateTags", &borrowed(&overwrite)).await;
    assert_eq!(
        status, 200,
        "overwriting a key the group already carries expected 200, got {status}: {body}"
    );

    // One key beyond it is refused, and nothing of that call is written.
    let beyond = tag_params(&[WrittenTag {
        group: ALPHA,
        key: "one-too-many",
        value: "z",
        propagate: Some(true),
    }]);
    let (status, body) = query(&router, "CreateOrUpdateTags", &borrowed(&beyond)).await;
    assert_eq!(
        status, 400,
        "a key beyond the quota expected 400, got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "Code"),
        "LimitExceeded",
        "expected the LimitExceeded code past the quota, got {body}"
    );
    let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
    assert_eq!(
        message,
        format!("You have already reached the limit of {QUOTA} tags for Auto Scaling group {ALPHA}"),
        "expected the quota sentence, got {message:?}"
    );
    let after = described_tags_of_account("after the refusal", &router).await;
    assert!(
        !after.iter().any(|tag| tag.contains("one-too-many")),
        "a refused write stores no tag, got {after:?}"
    );
    assert_eq!(
        after.iter().filter(|tag| tag.starts_with(&format!("{ALPHA}|"))).count(),
        QUOTA,
        "the group still carries exactly the quota, got {after:?}"
    );
}

/// One refused tag write: which operation, the parameters, and the whole sentence the
/// refusal states.
#[tokio::test]
async fn one_call_states_at_most_the_per_call_bound_of_tags() {
    // How many tags ONE call may state — the per-call bound this service's batch writes
    // carry, spelled out here rather than imported. A call past it is refused before any of
    // it is applied, whichever of the two writers it is sent to.
    const LIMIT: usize = 50;
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;
    let untouched = described_tags_of_account("before the refusal", &router).await;

    let mut params: Vec<(String, String)> = Vec::new();
    for member in 1..=(LIMIT + 1) {
        params.push(p(&format!("Tags.member.{member}.ResourceId"), ALPHA));
        params.push(p(
            &format!("Tags.member.{member}.ResourceType"),
            GROUP_RESOURCE_TYPE,
        ));
        params.push(p(
            &format!("Tags.member.{member}.Key"),
            format!("batch-{member}"),
        ));
        params.push(p(&format!("Tags.member.{member}.Value"), "x"));
    }

    for operation in ["CreateOrUpdateTags", "DeleteTags"] {
        let (status, body) = query(&router, operation, &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {operation:?}: a call stating {} tags is refused, got {status}: {body}",
            LIMIT + 1
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {operation:?}: expected the code ValidationError, got {body}"
        );
        let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message,
            format!("Trying to update too many tags at once. The limit is {LIMIT}"),
            "case {operation:?}: expected the per-call sentence, got {message:?}"
        );
        assert_eq!(
            described_tags_of_account(operation, &router).await,
            untouched,
            "case {operation:?}: a refused call changes no tag of the account"
        );
    }
}

/// One refused tag write: which operation, the parameters, and the whole sentence the
/// refusal states.
#[tokio::test]
async fn a_group_carries_one_tag_per_key_however_often_a_request_states_it() {
    // A tag of a group is identified by its key, so a create that states one key several
    // times leaves the group carrying that tag ONCE, with the value the last statement of
    // the key gave it — the same thing a later write of the same key does.
    let case = "a create stating one key three times";
    let (router, engine) = emulator();
    let subnet = given_tagged_groups(&router, &engine).await;
    let group = "tags-duplicated";
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("LaunchTemplate.LaunchTemplateName", "tags-template"),
        p("VPCZoneIdentifier", subnet),
    ];
    for (member, value) in [(1, "first"), (2, "second"), (3, "last")] {
        params.push(p(&format!("Tags.member.{member}.Key"), "repeated"));
        params.push(p(&format!("Tags.member.{member}.Value"), value));
        params.push(p(
            &format!("Tags.member.{member}.PropagateAtLaunch"),
            "false",
        ));
    }
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );

    let (status, body) = query(
        &router,
        "DescribeTags",
        &[
            ("Filters.member.1.Name", "auto-scaling-group"),
            ("Filters.member.1.Values.member.1", group),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTags expected 200, got {status}: {body}"
    );
    assert_eq!(
        described_tags(case, &body),
        vec![rendered(group, "repeated", "last", false)],
        "case {case:?}: the group carries the key once, at the value stated last"
    );
}

/// One refused tag write: which operation, the parameters, and the whole sentence the
/// refusal states.
struct WriteRefusal {
    case: &'static str,
    params: &'static [(&'static str, &'static str)],
    message: &'static str,
}

const WRITE_REFUSALS: &[WriteRefusal] = &[
    WriteRefusal {
        case: "a tag naming a group the account does not hold",
        params: &[
            ("Tags.member.1.ResourceId", "absent-group"),
            ("Tags.member.1.ResourceType", GROUP_RESOURCE_TYPE),
            ("Tags.member.1.Key", "env"),
            ("Tags.member.1.Value", "prod"),
        ],
        message: "AutoScalingGroup not found - AutoScaling Group absent-group not found",
    },
    WriteRefusal {
        case: "a tag naming no resource at all",
        params: &[
            ("Tags.member.1.Key", "env"),
            ("Tags.member.1.Value", "prod"),
        ],
        message: "AutoScalingGroup name not found - null",
    },
    WriteRefusal {
        case: "a tag naming a resource type this service does not tag",
        params: &[
            ("Tags.member.1.ResourceId", ALPHA),
            ("Tags.member.1.ResourceType", "auto-scaling-instance"),
            ("Tags.member.1.Key", "env"),
            ("Tags.member.1.Value", "prod"),
        ],
        message: "Resource type auto-scaling-instance is not correct. Allowed Resource types \
                  are: auto-scaling-group",
    },
    WriteRefusal {
        case: "a SECOND tag of the call naming a group the account does not hold",
        params: &[
            ("Tags.member.1.ResourceId", ALPHA),
            ("Tags.member.1.ResourceType", GROUP_RESOURCE_TYPE),
            ("Tags.member.1.Key", "written"),
            ("Tags.member.1.Value", "no"),
            ("Tags.member.2.ResourceId", "absent-group"),
            ("Tags.member.2.ResourceType", GROUP_RESOURCE_TYPE),
            ("Tags.member.2.Key", "env"),
            ("Tags.member.2.Value", "prod"),
        ],
        message: "AutoScalingGroup not found - AutoScaling Group absent-group not found",
    },
];

#[tokio::test]
async fn a_tag_write_is_applied_as_a_whole_or_refused() {
    let (router, engine) = emulator();
    given_tagged_groups(&router, &engine).await;

    // The tag set the account holds before any refusal, and must still hold after every one
    // of them: a refused call writes nothing.
    let untouched = described_tags_of_account("before the refusals", &router).await;

    for operation in ["CreateOrUpdateTags", "DeleteTags"] {
        for refusal in WRITE_REFUSALS {
            let case = format!("{operation}: {}", refusal.case);
            let expected = refusal.message;
            let (status, body) = query(&router, operation, refusal.params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status}: {body}"
            );
            assert_eq!(
                tag_value(&body, "Code"),
                "ValidationError",
                "case {case:?}: expected the code ValidationError, got {body}"
            );
            let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
            assert_eq!(
                described_tags_of_account(&case, &router).await,
                untouched,
                "case {case:?}: a refused call leaves every tag of the account as it was"
            );
        }
    }
}
