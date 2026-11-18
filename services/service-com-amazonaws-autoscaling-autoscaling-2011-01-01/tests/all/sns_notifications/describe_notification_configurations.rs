// SPDX-License-Identifier: CC0-1.0

//! `DescribeNotificationConfigurations` over the awsQuery wire: WHICH Amazon SNS
//! notification configurations one read reports, and how it pages them.
//!
//! The RULE under test: the read REPORTS the configurations the account's groups hold — one
//! entry per (group, topic, event type), in the collection's own order, so two reads answer
//! alike and nothing a later caller reads is changed by a read — narrowed to the groups
//! `AutoScalingGroupNames` names, where a read naming none reads every group of the account
//! and a name no group answers to selects nothing rather than refusing the read. One page
//! holds at most the `MaxRecords` the request names, inside the range the model documents
//! for it — "The maximum number of items to return with this call. The default value is 50
//! and the maximum value is 100"
//! (`smithy_query(query="doc", shape="DescribeNotificationConfigurationsType", member="MaxRecords")`)
//! — the entries past that page are reached through the `NextToken` the answer carries and
//! the last page carries none; a page size outside that range is refused HTTP 400
//! `ValidationError` naming the member and the bound the value missed, and a `NextToken`
//! this read never issued is refused HTTP 400 `InvalidNextToken` quoting the token the
//! caller submitted.
//!
//! The tables span what the rule generalises over: how many names the filter carries (none,
//! one, both, a name repeated, the names back to front, a name no group answers to, a name
//! differing from a stored one only in case), whether a named group holds any configuration
//! at all, and the page size a read walks with (none at all, each published bound, sizes
//! between them, one that holds the whole list, and — for the refusal — one step past each
//! bound and the extremes of the member's own type).
//!
//! The event types the seed configures are read from
//! `DescribeAutoScalingNotificationTypes` over the wire rather than written out here, so the
//! list these reads page is as long as the service's own published catalog makes it.

use smithy_server_runtime::MultiServiceRouter;

use crate::delete_notification_configuration::{put_configuration, topic_arn};
use crate::lifecycle_hooks::create_group;
use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, tag_values, xml_unescape};

/// The two groups this suite configures, in the order their names sort.
const GROUPS: [&str; 2] = ["asg-notified-a", "asg-notified-b"];

/// A third group of the account that is never configured with a topic: a group holding no
/// configuration contributes no entry, whether a read names it or reads every group.
const QUIET_GROUP: &str = "asg-notified-quiet";

/// How many topics each configured group is configured with. Four topics, each configured
/// with the whole published catalog, put the account's list PAST the default page size, so
/// the page a read that names no `MaxRecords` answers is observable.
const TOPICS_PER_GROUP: usize = 4;

/// The documented default page size: "The default value is 50 and the maximum value is 100"
/// (`DescribeNotificationConfigurationsType$MaxRecords`).
const DEFAULT_PAGE: usize = 50;

/// The largest page size the member admits, from the same documentation.
const LARGEST_PAGE: i32 = 100;

/// The smallest page size the member admits: a page holds items, so it holds at least one.
const SMALLEST_PAGE: i32 = 1;

/// One configuration as the read reports it: the group, the topic, the event type.
type Configuration = (String, String, String);

/// The event types this service publishes as configurable, as
/// `DescribeAutoScalingNotificationTypes` reports them.
async fn published_catalog(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeAutoScalingNotificationTypes", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingNotificationTypes expected HTTP 200, got {status} with {body:?}"
    );
    let catalog = tag_values(&body, "member");
    assert!(
        catalog.len() > 1,
        "the published catalog must name the types a group can notify about, got {catalog:?}"
    );
    catalog
}

/// POST one `DescribeNotificationConfigurations` carrying the filter, the page size and the
/// cursor a case asks for, and answer its status and body.
async fn describe(
    router: &MultiServiceRouter,
    names: &[&str],
    max_records: Option<i32>,
    next_token: Option<&str>,
) -> (u16, String) {
    let mut params: Vec<(String, String)> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        params.push(p(
            format!("AutoScalingGroupNames.member.{}", index + 1).as_str(),
            *name,
        ));
    }
    if let Some(max_records) = max_records {
        params.push(p("MaxRecords", max_records.to_string()));
    }
    if let Some(token) = next_token {
        params.push(p("NextToken", token));
    }
    query(
        router,
        "DescribeNotificationConfigurations",
        &borrowed(&params),
    )
    .await
}

/// The configurations one answer reports, in the order it reports them: every entry names
/// its group, its topic and its event type, so the three lists are read together.
fn reported(case: &str, body: &str) -> Vec<Configuration> {
    let groups = tag_values(body, "AutoScalingGroupName");
    let topics = tag_values(body, "TopicARN");
    let types = tag_values(body, "NotificationType");
    assert_eq!(
        (groups.len(), topics.len()),
        (types.len(), types.len()),
        "case {case:?}: every reported configuration names its group, topic and event type, \
         got {groups:?}, {topics:?} and {types:?}"
    );
    groups
        .into_iter()
        .zip(topics)
        .zip(types)
        .map(|((group, topic), notification_type)| (group, topic, notification_type))
        .collect()
}

/// An emulator holding [`GROUPS`], each configured with [`TOPICS_PER_GROUP`] topics for
/// every published event type, plus [`QUIET_GROUP`] holding none — and every configuration
/// the account then holds, in the order the collection holds them: group by group, topic by
/// topic, event type by event type.
async fn seeded() -> (MultiServiceRouter, Vec<Configuration>) {
    let router = router_with(new_engine());
    let catalog = published_catalog(&router).await;
    let events: Vec<&str> = catalog.iter().map(String::as_str).collect();

    let mut held: Vec<Configuration> = Vec::new();
    for group in GROUPS {
        create_group(&router, group).await;
        for index in 0..TOPICS_PER_GROUP {
            let topic = topic_arn(&format!("{group}-topic-{index}"));
            put_configuration(&router, "the seed", group, &topic, &events).await;
            for event in &catalog {
                held.push((group.to_string(), topic.clone(), event.clone()));
            }
        }
    }
    create_group(&router, QUIET_GROUP).await;

    // The collection is keyed by group, then topic, then event type, so the order a read
    // reports is that nesting — which is this list sorted.
    held.sort();
    assert!(
        held.len() > DEFAULT_PAGE,
        "the seed must hold more configurations than one default page, got {}",
        held.len()
    );
    (router, held)
}

/// The configurations of `held` that belong to one of `groups`.
fn held_by(held: &[Configuration], groups: &[&str]) -> Vec<Configuration> {
    held.iter()
        .filter(|(group, _, _)| groups.contains(&group.as_str()))
        .cloned()
        .collect()
}

/// The `Code` and the decoded `Message` of a refusal body.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// One selection row: the case, the names the read filters by, and the groups whose
/// configurations it must then report.
struct SelectionCase {
    case: &'static str,
    names: &'static [&'static str],
    selects: &'static [&'static str],
}

/// A read reports one configuration per (group, topic, event type), narrowed to the groups
/// it names — every group of the account when it names none, and nothing for a name no
/// group answers to.
#[tokio::test]
async fn a_read_reports_the_configurations_of_the_groups_it_names() {
    let (router, held) = seeded().await;

    let cases = [
        SelectionCase {
            case: "no name at all reads every group of the account",
            names: &[],
            selects: &GROUPS,
        },
        SelectionCase {
            case: "the first group alone",
            names: &[GROUPS[0]],
            selects: &[GROUPS[0]],
        },
        SelectionCase {
            case: "the second group alone",
            names: &[GROUPS[1]],
            selects: &[GROUPS[1]],
        },
        SelectionCase {
            case: "both groups",
            names: &[GROUPS[0], GROUPS[1]],
            selects: &GROUPS,
        },
        SelectionCase {
            case: "both groups named back to front",
            names: &[GROUPS[1], GROUPS[0]],
            selects: &GROUPS,
        },
        SelectionCase {
            case: "one group named twice",
            names: &[GROUPS[0], GROUPS[0]],
            selects: &[GROUPS[0]],
        },
        SelectionCase {
            case: "a group of the account holding no configuration",
            names: &[QUIET_GROUP],
            selects: &[],
        },
        SelectionCase {
            case: "a stored name beside a group holding no configuration",
            names: &[GROUPS[1], QUIET_GROUP],
            selects: &[GROUPS[1]],
        },
        SelectionCase {
            case: "a name no group answers to",
            names: &["asg-no-such-group"],
            selects: &[],
        },
        SelectionCase {
            case: "a stored name beside one no group answers to",
            names: &[GROUPS[0], "asg-no-such-group"],
            selects: &[GROUPS[0]],
        },
        SelectionCase {
            case: "a name differing from a stored one only in case",
            names: &["ASG-NOTIFIED-A"],
            selects: &[],
        },
        SelectionCase {
            case: "every group of the account, the unconfigured one included",
            names: &[GROUPS[0], GROUPS[1], QUIET_GROUP],
            selects: &GROUPS,
        },
    ];

    for SelectionCase {
        case,
        names,
        selects,
    } in cases
    {
        // A page large enough for the whole account, so this row is about the SELECTION
        // rather than about where the page ended.
        let (status, body) = describe(&router, names, Some(LARGEST_PAGE), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let expected = held_by(&held, selects);
        let answered = reported(case, &body);
        assert_eq!(
            answered, expected,
            "case {case:?}: expected the configurations {expected:?}, got {answered:?}"
        );
        let token = tag_value(&body, "NextToken");
        assert!(
            token.is_empty(),
            "case {case:?}: a page holding the whole selection carries no cursor, got {token:?}"
        );
    }

    // Reading is not a write: the account holds exactly what it held before every read
    // above.
    let (status, body) = describe(&router, &[], Some(LARGEST_PAGE), None).await;
    assert_eq!(
        status, 200,
        "the read after the selections must answer HTTP 200, got {status} with {body:?}"
    );
    let answered = reported("after the selections", &body);
    assert_eq!(
        answered, held,
        "after the selections: expected the stored configurations {held:?}, got {answered:?}"
    );
}

/// Whatever page size a caller walks with, the walk reaches every configuration of the
/// account exactly once and in one order — and a read naming no page size takes the
/// documented default.
#[tokio::test]
async fn the_page_walk_reaches_every_configuration_exactly_once() {
    let (router, held) = seeded().await;
    let total = held.len();

    // A read naming no page size answers the documented default and a cursor for the rest.
    let (status, body) = describe(&router, &[], None, None).await;
    assert_eq!(
        status, 200,
        "the default page must answer HTTP 200, got {status} with {body:?}"
    );
    let first = reported("no page size at all", &body);
    assert_eq!(
        first,
        held[..DEFAULT_PAGE],
        "no page size at all: expected the first {DEFAULT_PAGE} configurations, got {first:?}"
    );
    let token = tag_value(&body, "NextToken");
    assert!(
        !token.is_empty(),
        "no page size at all: a page that does not end the list carries a cursor, got {body:?}"
    );
    let (status, body) = describe(&router, &[], None, Some(&token)).await;
    assert_eq!(
        status, 200,
        "the resumed page must answer HTTP 200, got {status} with {body:?}"
    );
    let resumed = reported("the page after the default one", &body);
    assert_eq!(
        resumed,
        held[DEFAULT_PAGE..],
        "the page after the default one: expected the configurations past it, got {resumed:?}"
    );
    assert!(
        tag_value(&body, "NextToken").is_empty(),
        "the page after the default one: the last page carries no cursor, got {body:?}"
    );

    // The page sizes a walk can ask for: none at all, each published bound, sizes between
    // them, and one that holds the whole list exactly.
    let sizes = [
        None,
        Some(SMALLEST_PAGE),
        Some(2),
        Some(7),
        Some(DEFAULT_PAGE as i32),
        Some(total as i32),
        Some(LARGEST_PAGE),
    ];
    for max_records in sizes {
        let case = format!("a walk with the page size {max_records:?}");
        let limit = max_records.map_or(DEFAULT_PAGE, |value| value as usize);
        let mut walked: Vec<Configuration> = Vec::new();
        let mut cursor: Option<String> = None;
        // A walk of a list holding `total` configurations takes at most that many pages, so
        // a walk that has not ended by then is not converging.
        for _ in 0..=total {
            let (status, body) = describe(&router, &[], max_records, cursor.as_deref()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {body:?}"
            );
            let page = reported(&case, &body);
            assert!(
                page.len() <= limit,
                "case {case:?}: a page holds at most {limit} configurations, got {}",
                page.len()
            );
            let next = tag_value(&body, "NextToken");
            if !next.is_empty() {
                assert_eq!(
                    page.len(),
                    limit,
                    "case {case:?}: a page followed by a cursor is full, got {}",
                    page.len()
                );
            }
            walked.extend(page);
            if next.is_empty() {
                cursor = None;
                break;
            }
            cursor = Some(next);
        }
        assert!(
            cursor.is_none(),
            "case {case:?}: the walk must end, got a cursor after {} configurations",
            walked.len()
        );
        assert_eq!(
            walked, held,
            "case {case:?}: the walk must reach every configuration exactly once, in the \
             order an unpaged read reports"
        );
    }

    // A narrowed read pages its OWN selection: the walk of one group reaches that group's
    // configurations and no other group's.
    let mut walked: Vec<Configuration> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..=total {
        let (status, body) = describe(&router, &[GROUPS[1]], Some(3), cursor.as_deref()).await;
        assert_eq!(
            status, 200,
            "a narrowed walk must answer HTTP 200, got {status} with {body:?}"
        );
        walked.extend(reported("a narrowed walk", &body));
        let next = tag_value(&body, "NextToken");
        if next.is_empty() {
            cursor = None;
            break;
        }
        cursor = Some(next);
    }
    let expected = held_by(&held, &[GROUPS[1]]);
    assert!(
        cursor.is_none(),
        "a narrowed walk must end, got a cursor after {} configurations",
        walked.len()
    );
    assert_eq!(
        walked, expected,
        "a narrowed walk: expected the configurations {expected:?}, got {walked:?}"
    );
}

/// A page size outside the documented range is refused with the sentence the service states
/// for a member value outside its bounds, and the configurations a later caller reads are
/// exactly the ones that were there before.
#[tokio::test]
async fn a_page_size_outside_the_documented_range_is_refused_and_changes_nothing() {
    let (router, held) = seeded().await;

    let below = format!("Member must have value greater than or equal to {SMALLEST_PAGE}");
    let above = format!("Member must have value less than or equal to {LARGEST_PAGE}");

    // (case, the page size the read carries, the bound it missed) — each published bound's
    // far side, one step past it, and the extremes of the member's own type.
    let refused = [
        ("one below the smallest page", 0, &below),
        ("a negative page size", -1, &below),
        ("the smallest value the member can carry", i32::MIN, &below),
        ("one past the largest page", LARGEST_PAGE + 1, &above),
        ("far past the largest page", 100_000, &above),
        ("the largest value the member can carry", i32::MAX, &above),
    ];

    for (case, max_records, constraint) in refused {
        let (status, body) = describe(&router, &[], Some(max_records), None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} with {body:?}"
        );
        let expected = format!(
            "1 validation error detected: Value '{max_records}' at 'maxRecords' failed to \
             satisfy constraint: {constraint}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // The controls: each published bound itself, and the documented default, are inside the
    // range and still answer a page.
    for max_records in [SMALLEST_PAGE, DEFAULT_PAGE as i32, LARGEST_PAGE] {
        let case = format!("the page size {max_records}");
        let (status, body) = describe(&router, &[], Some(max_records), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let page = reported(&case, &body);
        let limit = (max_records as usize).min(held.len());
        assert_eq!(
            page,
            held[..limit],
            "case {case:?}: expected the first {limit} configurations, got {page:?}"
        );
    }

    // A refused read, and a read of any kind, leaves the collection as it was.
    let (status, body) = describe(&router, &[], Some(LARGEST_PAGE), None).await;
    assert_eq!(
        status, 200,
        "the read after the refusals must answer HTTP 200, got {status} with {body:?}"
    );
    let answered = reported("after the refusals", &body);
    assert_eq!(
        answered, held,
        "after the refusals: expected the stored configurations {held:?}, got {answered:?}"
    );
}

/// A cursor this read never issued is refused, quoting the token the caller submitted, and
/// the configurations stay exactly as they were.
#[tokio::test]
async fn a_cursor_this_read_never_issued_is_refused() {
    let (router, held) = seeded().await;
    let total = held.len();

    // Tokens a client can submit that this read never issued: free text, the marker alone,
    // the first page's own offset (which is reached with no cursor at all), an offset at and
    // past the end of the list, and text carrying characters the envelope escapes.
    let tokens = [
        "not-a-real-token".to_string(),
        "next-".to_string(),
        "next-0".to_string(),
        format!("next-{total}"),
        format!("next-{}", total + 1),
        "next-99999".to_string(),
        "next-abc".to_string(),
        "page 2 of 3".to_string(),
        "<token & 'quoted'>".to_string(),
    ];

    for token in &tokens {
        let case = format!("the token {token:?}");
        let (status, body) = describe(&router, &[], None, Some(token)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            code, "InvalidNextToken",
            "case {case:?}: expected the InvalidNextToken code, got {code:?} with {body:?}"
        );
        let expected = format!("The token '{token}' is invalid");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // The control: the cursor the read ITSELF issued resumes the list, and the refusals
    // above changed nothing.
    let (status, body) = describe(&router, &[], Some(SMALLEST_PAGE), None).await;
    assert_eq!(
        status, 200,
        "the first page must answer HTTP 200, got {status} with {body:?}"
    );
    let token = tag_value(&body, "NextToken");
    assert!(
        !token.is_empty(),
        "the first page of a longer list carries a cursor, got {body:?}"
    );
    let (status, body) = describe(&router, &[], Some(LARGEST_PAGE), Some(&token)).await;
    assert_eq!(
        status, 200,
        "the resumed read must answer HTTP 200, got {status} with {body:?}"
    );
    let answered = reported("the resumed read", &body);
    assert_eq!(
        answered,
        held[1..],
        "the resumed read: expected the configurations after the first, got {answered:?}"
    );
}
