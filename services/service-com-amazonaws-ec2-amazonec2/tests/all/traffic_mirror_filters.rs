// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the Traffic Mirror filter slice:
//! `CreateTrafficMirrorFilter`, `CreateTrafficMirrorFilterRule` and
//! `DescribeTrafficMirrorFilters`, driven over the wire the service actually speaks —
//! `ec2Query`, through the crate's shared [`super::ec2_query`] harness. A rule lives on the
//! filter that holds it and is reported as one of that filter's own members, so the create of
//! a rule is covered here too, in the section that follows the filter's own rules.
//!
//! The rules under test are:
//!
//! 1. `CreateTrafficMirrorFilter` MINTS A FILTER FROM THE REQUEST and
//!    `DescribeTrafficMirrorFilters` REPORTS THE STORE: every member the request sets
//!    round-trips through the describe, the id is the service's own, and a fresh filter
//!    carries no rule in either direction ("By default, no traffic is mirrored");
//! 2. `DescribeTrafficMirrorFilters` NARROWS: by `TrafficMirrorFilterIds`, and by every
//!    filter name it answers — values ORed within one entry, entries ANDed with each
//!    other — and a filter name it does not answer is refused;
//! 3. an id that names no Traffic Mirror filter of the account is refused with
//!    `InvalidTrafficMirrorFilterId.NotFound`, whatever the id looks like, while an account
//!    that simply holds no filter is reported as holding none;
//! 4. the report PAGES: `MaxResults` bounds a page, `NextToken` resumes it, and walking the
//!    pages visits every filter exactly once;
//! 5. one `ClientToken` names one filter, and `DryRun` answers `DryRunOperation` without
//!    storing or reporting anything;
//! 6. the create holds the request to every bound the crate publishes — a value AT the
//!    bound is stored, one PAST it is refused — and it decides that BEFORE it writes, so a
//!    `DryRun` that would not have worked is refused with the reason it would have failed
//!    for and a refused create stores nothing;
//! 7. a create carrying no `ClientToken` is never a replay: two identical requests mint two
//!    distinct filters and neither response echoes a token.
//!
//! Each table row is a distinct input along what the rule generalises over — how long a
//! description is and which characters it holds, how many tags a filter carries, which
//! filter name selects, how many values one entry carries, which page size, which shape of
//! unknown id — so no row is the specification. The recorded unit this slice was filed from
//! is ONE row of the table in [`an_unknown_filter_id_is_refused_whatever_it_looks_like`].

use service_com_amazonaws_ec2_amazonec2 as ec2;
use ec2::state::{
    MAX_CLIENT_TOKEN_CHARS, MAX_TAGS_PER_RESOURCE, MAX_TAG_KEY_CHARS, MAX_TAG_VALUE_CHARS,
    MAX_TRAFFIC_MIRROR_FILTERS_PAGE_SIZE, RESERVED_TAG_PREFIX,
    TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, filters, indexed, unescaped};

/// The Region every call here is signed for. A Traffic Mirror filter is a Region-scoped
/// resource whose modelled shape names no Region, so the Region only selects the envelope.
const REGION: &str = "us-west-2";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// One row of the create table: `(case, the create parameters, the description it must
/// report, the tags it must report)`.
type CreateRow = (&'static str, Params, Option<&'static str>, Vec<(&'static str, &'static str)>);

/// One row of the filter table: `(case, the describe parameters, the ids it must select)`.
type FilterRow = (String, Params, Vec<String>);

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

async fn create(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "CreateTrafficMirrorFilter", params).await
}

async fn describe(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "DescribeTrafficMirrorFilters", params).await
}

/// A `TrafficMirrorFilterId` is `tmf-` followed by 8 or 17 lowercase hex digits, the short
/// and the long EC2 id form.
fn is_traffic_mirror_filter_id(candidate: &str) -> bool {
    let Some(suffix) = candidate.strip_prefix("tmf-") else {
        return false;
    };
    matches!(suffix.len(), 8 | 17)
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Create one filter and answer the id it was minted with, failing the case when the create
/// did not succeed.
async fn created_id(router: &MultiServiceRouter, case: &str, params: &[(String, String)]) -> String {
    let (status, body) = create(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateTrafficMirrorFilter, got {status} \
         with body {body}"
    );
    let id = element(&body, "trafficMirrorFilterId").unwrap_or_default();
    assert!(
        is_traffic_mirror_filter_id(&id),
        "case {case:?}: expected a minted Traffic Mirror filter id, got {id:?}"
    );
    id
}

/// The ids `DescribeTrafficMirrorFilters` reports for `params`, in the order it reported
/// them.
async fn described_ids(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> Vec<String> {
    let (status, body) = describe(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeTrafficMirrorFilters, got {status} \
         with body {body}"
    );
    elements(&body, "trafficMirrorFilterId")
}

/// The form parameters for one tag on the one resource type this create tags.
fn tag_params(index: usize, key: &str, value: &str) -> Params {
    vec![
        p(
            &format!("TagSpecification.{index}.ResourceType"),
            TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE,
        ),
        p(&format!("TagSpecification.{index}.Tag.1.Key"), key),
        p(&format!("TagSpecification.{index}.Tag.1.Value"), value),
    ]
}

/// The form parameters for `count` tags in ONE specification of the one taggable resource
/// type, so a row can carry a tag list of any size.
fn many_tag_params(count: usize) -> Params {
    let mut params = vec![p(
        "TagSpecification.1.ResourceType",
        TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE,
    )];
    for index in 1..=count {
        params.push(p(
            &format!("TagSpecification.1.Tag.{index}.Key"),
            format!("Key{index}"),
        ));
        params.push(p(
            &format!("TagSpecification.1.Tag.{index}.Value"),
            format!("value-{index}"),
        ));
    }
    params
}

/// RULE 1: every create in the table is stored as the request describes it and reads back
/// out of the store through the describe, with the id, and only the id, derived by the
/// service — and a fresh filter carries no rule in either direction and no network service.
#[tokio::test]
async fn every_created_filter_round_trips_through_the_describe() {
    // The rows differ along what the rule generalises over: whether a description is given
    // at all, how long it is, which characters it holds, and how many tags the filter
    // carries.
    let rows: Vec<CreateRow> = vec![
        ("no description and no tags", Vec::new(), None, Vec::new()),
        (
            "a short description",
            vec![p("Description", "web tier")],
            Some("web tier"),
            Vec::new(),
        ),
        (
            "a description holding punctuation and non-ASCII text",
            vec![p("Description", "mirror: ingress/egress — grün")],
            Some("mirror: ingress/egress — grün"),
            Vec::new(),
        ),
        (
            "a 255-character description",
            vec![p("Description", "d".repeat(255))],
            None,
            Vec::new(),
        ),
        (
            "one tag and a description",
            [vec![p("Description", "tagged")], tag_params(1, "Team", "platform")].concat(),
            Some("tagged"),
            vec![("Team", "platform")],
        ),
        (
            "two tag specifications of the one taggable resource type",
            [tag_params(1, "Team", "network"), tag_params(2, "Env", "staging")].concat(),
            None,
            vec![("Team", "network"), ("Env", "staging")],
        ),
    ];

    for (case, params, description, tags) in rows {
        let router = emulator();
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );

        let id = element(&body, "trafficMirrorFilterId").unwrap_or_default();
        assert!(
            is_traffic_mirror_filter_id(&id),
            "case {case:?}: expected a minted Traffic Mirror filter id, got {id:?}"
        );
        // A fresh filter mirrors nothing: no rule in either direction, and no network
        // service, so no rule id can be reported for it.
        assert!(
            elements(&body, "trafficMirrorFilterRuleId").is_empty(),
            "case {case:?}: a fresh filter carries no rules, got {body}"
        );

        // ROUND TRIP: the filter is in the store, not just echoed.
        let (status, described) =
            describe(&router, &indexed("TrafficMirrorFilterId", &[&id])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the describe, got {status} / {described}"
        );
        assert_eq!(
            elements(&described, "trafficMirrorFilterId"),
            vec![id.clone()],
            "case {case:?}: expected the describe to report the created filter, got {described}"
        );
        for (source, reported) in [("the create", &body), ("the describe", &described)] {
            let described_description = element(reported, "description").map(|text| unescaped(&text));
            match description {
                Some(expected) => assert_eq!(
                    described_description.as_deref(),
                    Some(expected),
                    "case {case:?}: expected {source} to report description {expected:?}, \
                     got {described_description:?}"
                ),
                // The 255-character row's text is built rather than written out, and a row
                // that named no description reports none at all.
                None => {
                    let named = params
                        .iter()
                        .find(|(key, _)| key == "Description")
                        .map(|(_, value)| value.clone());
                    assert_eq!(
                        described_description, named,
                        "case {case:?}: expected {source} to report the description the \
                         request gave ({named:?}), got {described_description:?}"
                    );
                }
            }
            let keys = elements(reported, "key");
            let values = elements(reported, "value");
            assert_eq!(
                keys.len(),
                tags.len(),
                "case {case:?}: expected {source} to report {} tags, got {keys:?}",
                tags.len()
            );
            for (key, value) in &tags {
                let at = keys.iter().position(|reported_key| reported_key == key);
                assert!(
                    at.is_some(),
                    "case {case:?}: expected {source} to report the tag key {key:?}, got {keys:?}"
                );
                assert_eq!(
                    values.get(at.unwrap_or_default()).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected {source} to report {key:?}={value:?}, got {values:?}"
                );
            }
        }
    }
}

/// RULE 2: the describe narrows by ids and by every filter name it answers, and refuses a
/// name it does not answer.
#[tokio::test]
async fn the_describe_narrows_by_ids_and_by_every_filter_it_answers() {
    let router = emulator();
    let web = created_id(
        &router,
        "web",
        &[vec![p("Description", "web tier")], tag_params(1, "Team", "platform")].concat(),
    )
    .await;
    let data = created_id(
        &router,
        "data",
        &[vec![p("Description", "data tier")], tag_params(1, "Team", "data")].concat(),
    )
    .await;
    let bare = created_id(&router, "bare", &[]).await;
    let all = vec![web.clone(), data.clone(), bare.clone()];

    // The rows differ along which filter name selects, how many values one entry carries,
    // whether a wildcard is used, and how many entries are ANDed together.
    let rows: Vec<FilterRow> = vec![
        (
            "no narrowing at all".to_string(),
            Vec::new(),
            all.clone(),
        ),
        (
            "one id".to_string(),
            indexed("TrafficMirrorFilterId", &[&data]),
            vec![data.clone()],
        ),
        (
            "two ids".to_string(),
            indexed("TrafficMirrorFilterId", &[&bare, &web]),
            vec![web.clone(), bare.clone()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        ),
        (
            "the traffic-mirror-filter-id filter".to_string(),
            filters(&[("traffic-mirror-filter-id", &[web.as_str()])]),
            vec![web.clone()],
        ),
        (
            "the description filter, matched exactly".to_string(),
            filters(&[("description", &["data tier"])]),
            vec![data.clone()],
        ),
        (
            "the description filter, two values ORed".to_string(),
            filters(&[("description", &["data tier", "web tier"])]),
            vec![web.clone(), data.clone()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        ),
        (
            "the description filter with EC2's * wildcard".to_string(),
            filters(&[("description", &["*tier"])]),
            vec![web.clone(), data.clone()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        ),
        (
            "the description filter on a value nothing holds".to_string(),
            filters(&[("description", &["mirror everything"])]),
            Vec::new(),
        ),
        (
            "the tag-key filter".to_string(),
            filters(&[("tag-key", &["Team"])]),
            vec![web.clone(), data.clone()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        ),
        (
            "the tag:<key> filter".to_string(),
            filters(&[("tag:Team", &["data"])]),
            vec![data.clone()],
        ),
        (
            "two filters, ANDed".to_string(),
            filters(&[("description", &["*tier"]), ("tag:Team", &["platform"])]),
            vec![web.clone()],
        ),
        (
            "two filters, ANDed, that no one filter satisfies together".to_string(),
            filters(&[("description", &["data tier"]), ("tag:Team", &["platform"])]),
            Vec::new(),
        ),
        (
            "a filter carrying no value at all".to_string(),
            filters(&[("description", &[])]),
            Vec::new(),
        ),
        (
            "an id and a filter that disagree".to_string(),
            [
                indexed("TrafficMirrorFilterId", &[&bare]),
                filters(&[("description", &["web tier"])]),
            ]
            .concat(),
            Vec::new(),
        ),
    ];

    for (case, params, expected) in rows {
        let mut reported = described_ids(&router, &case, &params).await;
        reported.sort();
        let mut want = expected;
        want.sort();
        assert_eq!(
            reported, want,
            "case {case:?}: expected the describe to select {want:?}, got {reported:?}"
        );
    }

    // A filter name this operation does not answer is refused, whether or not any filter
    // would have matched it — including a `tag:` prefix with no key after it.
    for name in ["owner-id", "Description", "traffic-mirror-filter-ids", "tag:"] {
        let (status, body) = describe(&router, &filters(&[(name, &["web tier"])])).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400 for an unanswered filter name, got {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {name:?}: expected InvalidParameterValue, got {body}"
        );
    }
}

/// RULE 3: an id that names no Traffic Mirror filter of the account is refused, whatever
/// shape it has — while an account that holds no filter at all is reported as holding none.
#[tokio::test]
async fn an_unknown_filter_id_is_refused_whatever_it_looks_like() {
    let router = emulator();

    // An empty account: the report is empty, not an error.
    let (status, body) = describe(&router, &[]).await;
    assert_eq!(
        status, 200,
        "case \"an account with no filters\": expected HTTP 200, got {status} / {body}"
    );
    assert!(
        elements(&body, "trafficMirrorFilterId").is_empty(),
        "case \"an account with no filters\": expected an empty report, got {body}"
    );

    let known = created_id(&router, "the one stored filter", &[]).await;

    // The rows differ along the SHAPE of the unknown id: the long 17-hex form, the short
    // 8-hex form, the 16-hex form the recorded unit this issue was filed from used, a
    // non-hex suffix and a foreign prefix.
    let unknown = [
        "tmf-0123456789abcdef0",
        "tmf-00000000",
        "tmf-ffffffffffffffff",
        "tmf-not-hex",
        "vpc-0123456789abcdef0",
    ];
    for id in unknown {
        for (case, params) in [
            (format!("{id} alone"), indexed("TrafficMirrorFilterId", &[id])),
            (
                format!("{id} beside a known id"),
                indexed("TrafficMirrorFilterId", &[known.as_str(), id]),
            ),
        ] {
            let (status, body) = describe(&router, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for an id naming no filter, got {status} / {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidTrafficMirrorFilterId.NotFound",
                "case {case:?}: expected InvalidTrafficMirrorFilterId.NotFound, got {body}"
            );
            assert!(
                element(&body, "Message")
                    .map(|message| unescaped(&message))
                    .is_some_and(|message| message.contains(id)),
                "case {case:?}: expected the message to name the id the request gave, got {body}"
            );
        }
    }
}

/// RULE 4: the report pages — a page holds at most `MaxResults` filters, `NextToken`
/// resumes where the page ended, and walking the pages visits every filter exactly once.
#[tokio::test]
async fn the_report_pages_over_every_filter_exactly_once() {
    let router = emulator();
    let mut created: Vec<String> = Vec::new();
    for index in 0..7 {
        created.push(
            created_id(
                &router,
                &format!("filter {index}"),
                &[p("Description", format!("tier {index}"))],
            )
            .await,
        );
    }
    created.sort();

    // The modelled `MaxResults` range is 5..1000, so these are the smallest page the
    // operation can be asked for, one above it, and a page larger than the whole store.
    for page_size in [5usize, 6, 7, 1000] {
        let case = format!("MaxResults={page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut params = vec![p("MaxResults", page_size.to_string())];
            if let Some(token) = &next_token {
                params.push(p("NextToken", token.clone()));
            }
            let (status, body) = describe(&router, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            let page = elements(&body, "trafficMirrorFilterId");
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page holds at most MaxResults filters, got {}",
                page.len()
            );
            walked.extend(page);
            next_token = element(&body, "nextToken");
            if next_token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, created,
            "case {case:?}: paging must walk every filter exactly once, got {walked:?}"
        );
    }

    // No page size named: one page, up to the largest the operation can be asked for.
    assert!(
        created.len() <= MAX_TRAFFIC_MIRROR_FILTERS_PAGE_SIZE,
        "this suite seeds fewer filters than one page holds"
    );
    let (status, body) = describe(&router, &[]).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} with body {body}");
    assert!(
        element(&body, "nextToken").is_none(),
        "case \"no MaxResults\": a page that holds everything carries no token, got {body}"
    );

    // A token this service did not issue is refused rather than read as a cursor.
    for token in ["not-a-token", "00", ""] {
        let (status, body) = describe(&router, &[p("NextToken", token)]).await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected HTTP 400 for a foreign NextToken, got {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidPaginationToken",
            "case {token:?}: expected InvalidPaginationToken, got {body}"
        );
    }
}

/// RULE 5: one `ClientToken` names one filter, `DryRun` stores nothing, and a tag
/// specification naming another resource type is refused.
#[tokio::test]
async fn idempotency_dry_run_and_tagging_are_refused_or_replayed() {
    let router = emulator();

    // The rows differ along the token's own text, and each row is replayed, so idempotency
    // is asserted for a token of each shape rather than for one recorded value.
    for token in ["a", "idem-7", "0123456789abcdef0123456789abcdef"] {
        let params = vec![p("ClientToken", token), p("Description", format!("for {token}"))];
        let first = created_id(&router, token, &params).await;
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {token:?}: expected a replay to succeed, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "trafficMirrorFilterId").as_deref(),
            Some(first.as_str()),
            "case {token:?}: one ClientToken names one filter, got {body}"
        );
        assert_eq!(
            element(&body, "clientToken").as_deref(),
            Some(token),
            "case {token:?}: the response echoes the token the request carried, got {body}"
        );
    }
    let after_replays = described_ids(&router, "after the replays", &[]).await;
    assert_eq!(
        after_replays.len(),
        3,
        "a replayed create stores no second filter, got {after_replays:?}"
    );

    // DryRun: refused with DryRunOperation ("If you have the required permissions, the
    // error response is DryRunOperation" — `DryRun`), which EC2 answers with HTTP 412, and
    // nothing stored or reported.
    for (case, params) in [
        ("a dry-run create", vec![p("DryRun", "true")]),
        (
            "a dry-run create with tags",
            [vec![p("DryRun", "true")], tag_params(1, "Team", "platform")].concat(),
        ),
    ] {
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 412,
            "case {case:?}: expected HTTP 412, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "case {case:?}: expected DryRunOperation, got {body}"
        );
    }
    let (status, body) = describe(&router, &[p("DryRun", "true")]).await;
    assert_eq!(
        status, 412,
        "case \"a dry-run describe\": expected HTTP 412, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case \"a dry-run describe\": expected DryRunOperation, got {body}"
    );
    assert_eq!(
        described_ids(&router, "after the dry runs", &[]).await,
        after_replays,
        "a dry run stores nothing"
    );

    // A tag specification may name only the resource this call creates.
    for resource_type in ["vpc", "traffic-mirror-session", "instance"] {
        let (status, body) = create(
            &router,
            &[
                p("TagSpecification.1.ResourceType", resource_type),
                p("TagSpecification.1.Tag.1.Key", "Team"),
                p("TagSpecification.1.Tag.1.Value", "platform"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {resource_type:?}: expected HTTP 400 for a foreign resource type, \
             got {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {resource_type:?}: expected InvalidParameterValue, got {body}"
        );
    }
    assert_eq!(
        described_ids(&router, "after the refused tag specifications", &[]).await,
        after_replays,
        "a refused create stores nothing"
    );
}

/// RULE 6: the create holds the request to every bound this crate publishes — a value AT
/// the bound is stored, one PAST it is refused — and it decides that before the first
/// write, so the same request under `DryRun` is refused with the reason it would have
/// failed for rather than with `DryRunOperation`, and no refused create leaves a filter
/// behind.
#[tokio::test]
async fn the_create_holds_every_published_bound_before_it_writes() {
    // The bounds come from the crate's published constants — `MAX_CLIENT_TOKEN_CHARS`
    // (`CreateLaunchTemplate$ClientToken`: "Constraint: Maximum 128 ASCII characters"),
    // `MAX_TAG_KEY_CHARS`, `MAX_TAG_VALUE_CHARS` and `MAX_TAGS_PER_RESOURCE` — so each is
    // asserted at the published bound and one past it, beside the tag rules that hold
    // whatever the lengths are. `None` is a row that must be STORED.
    type BoundRow = (String, Params, Option<&'static str>);
    let rows: Vec<BoundRow> = vec![
        (
            format!("a ClientToken of {MAX_CLIENT_TOKEN_CHARS} characters"),
            vec![p("ClientToken", "t".repeat(MAX_CLIENT_TOKEN_CHARS))],
            None,
        ),
        (
            format!("a ClientToken of {} characters", MAX_CLIENT_TOKEN_CHARS + 1),
            vec![p("ClientToken", "t".repeat(MAX_CLIENT_TOKEN_CHARS + 1))],
            Some("InvalidParameterValue"),
        ),
        (
            format!("a tag key of {MAX_TAG_KEY_CHARS} characters"),
            tag_params(1, &"k".repeat(MAX_TAG_KEY_CHARS), "at the key bound"),
            None,
        ),
        (
            format!("a tag key of {} characters", MAX_TAG_KEY_CHARS + 1),
            tag_params(1, &"k".repeat(MAX_TAG_KEY_CHARS + 1), "past the key bound"),
            Some("InvalidParameterValue"),
        ),
        (
            format!("a tag value of {MAX_TAG_VALUE_CHARS} characters"),
            tag_params(1, "AtValueBound", &"v".repeat(MAX_TAG_VALUE_CHARS)),
            None,
        ),
        (
            format!("a tag value of {} characters", MAX_TAG_VALUE_CHARS + 1),
            tag_params(1, "PastValueBound", &"v".repeat(MAX_TAG_VALUE_CHARS + 1)),
            Some("InvalidParameterValue"),
        ),
        (
            format!("{MAX_TAGS_PER_RESOURCE} tags"),
            many_tag_params(MAX_TAGS_PER_RESOURCE),
            None,
        ),
        (
            format!("{} tags", MAX_TAGS_PER_RESOURCE + 1),
            many_tag_params(MAX_TAGS_PER_RESOURCE + 1),
            Some("TagLimitExceeded"),
        ),
        (
            "an empty tag key".to_string(),
            tag_params(1, "", "platform"),
            Some("InvalidParameterValue"),
        ),
        (
            format!("a tag key in the reserved {RESERVED_TAG_PREFIX} namespace"),
            tag_params(1, &format!("{RESERVED_TAG_PREFIX}cloudformation:stack-name"), "mirrors"),
            Some("InvalidParameterValue"),
        ),
        (
            "one tag key given twice".to_string(),
            [tag_params(1, "Team", "platform"), tag_params(2, "Team", "network")].concat(),
            Some("InvalidParameterValue"),
        ),
        (
            "a tag specification naming no resource type".to_string(),
            vec![
                p("TagSpecification.1.Tag.1.Key", "Team"),
                p("TagSpecification.1.Tag.1.Value", "platform"),
            ],
            Some("MissingParameter"),
        ),
    ];

    let router = emulator();
    let mut stored: Vec<String> = Vec::new();
    for (case, params, refusal) in &rows {
        let (status, body) = create(&router, params).await;
        match refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a value at the published bound is stored, got {status} \
                     with body {body}"
                );
                let id = element(&body, "trafficMirrorFilterId").unwrap_or_default();
                assert!(
                    is_traffic_mirror_filter_id(&id),
                    "case {case:?}: expected a minted Traffic Mirror filter id, got {id:?}"
                );
                stored.push(id);
            }
            Some(code) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400 for a request past the bound, \
                     got {status} with body {body}"
                );
                assert_eq!(
                    error_code(&body).as_str(),
                    *code,
                    "case {case:?}: expected {code}, got {body}"
                );
            }
        }
    }

    // The same table under `DryRun`: the flag asks whether the request WOULD have worked,
    // so a row that would have been refused keeps its own refusal and a row that would have
    // succeeded answers `DryRunOperation` (HTTP 412) — and neither stores a thing, the
    // replayed `ClientToken` row included.
    for (case, params, refusal) in &rows {
        let case = format!("{case}, under DryRun");
        let params = [params.clone(), vec![p("DryRun", "true")]].concat();
        let (status, body) = create(&router, &params).await;
        let (expected_status, expected_code) = match refusal {
            None => (412, "DryRunOperation"),
            Some(code) => (400, *code),
        };
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
    }

    let mut described = described_ids(&router, "after the whole bound table", &[]).await;
    described.sort();
    stored.sort();
    assert_eq!(
        described, stored,
        "only the creates at the published bounds are stored, got {described:?}"
    );
}

/// RULE 7: a create that carries no `ClientToken` is never a replay — "Unique,
/// case-sensitive identifier that YOU provide" is what makes one, so two requests with
/// identical parameters mint two distinct filters and neither response echoes a token.
#[tokio::test]
async fn a_create_without_a_client_token_is_never_a_replay() {
    let router = emulator();
    // The rows differ along how much of the record the two requests share: nothing, a
    // description, and a description plus a tag — identical in every member either way.
    let rows: Vec<(&str, Params)> = vec![
        ("no parameters at all", Vec::new()),
        ("the same description twice", vec![p("Description", "mirror everything")]),
        (
            "the same description and tag twice",
            [vec![p("Description", "tagged")], tag_params(1, "Team", "platform")].concat(),
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, params) in rows {
        let mut ids: Vec<String> = Vec::new();
        for attempt in 1..=2 {
            let case = format!("{case}, request {attempt}");
            let (status, body) = create(&router, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            assert!(
                element(&body, "clientToken").is_none(),
                "case {case:?}: a request carrying no token is answered with none, got {body}"
            );
            let id = element(&body, "trafficMirrorFilterId").unwrap_or_default();
            assert!(
                is_traffic_mirror_filter_id(&id),
                "case {case:?}: expected a minted Traffic Mirror filter id, got {id:?}"
            );
            ids.push(id);
        }
        assert_ne!(
            ids[0], ids[1],
            "case {case:?}: two tokenless creates mint two filters, got {ids:?}"
        );
        minted.extend(ids);
    }

    let mut described = described_ids(&router, "after the tokenless creates", &[]).await;
    described.sort();
    minted.sort();
    assert_eq!(
        described, minted,
        "every tokenless create is stored on its own, got {described:?}"
    );
}

// ---------------------------------------------------------------------------
// The RULES of a filter: `CreateTrafficMirrorFilterRule`
//
// A Traffic Mirror rule has nowhere to live but the filter it belongs to, and a filter
// reports its rules as its own members ("Information about the ingress rules that are
// associated with the Traffic Mirror filter", `TrafficMirrorFilter$IngressFilterRules`), so
// the create of a rule is covered here, beside the filter it writes onto and the describe
// that reads it back. The rules under test are:
//
// 8. `CreateTrafficMirrorFilterRule` MINTS A RULE FROM THE REQUEST and the filter REPORTS IT:
//    every member the request sets round-trips through `DescribeTrafficMirrorFilters`, in the
//    direction's own rule set, with the id — and only the id — derived by the service;
// 9. a filter's rules are reported in ASCENDING RULE NUMBER order within each direction,
//    whatever order they were created in, and the two directions are separate sets, so one
//    number may be used once in each;
// 10. one rule number in one direction of one filter names ONE rule, so a second create at
//     that number is refused, and a rule for a filter the account does not hold is refused;
// 11. the create holds the request to every bound and form this crate publishes — a value AT
//     the bound is stored, one PAST it is refused — and it decides that BEFORE it writes, so a
//     `DryRun` that would not have worked is refused with the reason it would have failed for
//     and a refused create stores nothing;
// 12. one `ClientToken` names one rule, and a create carrying none is never a replay.
// ---------------------------------------------------------------------------

/// One `CreateTrafficMirrorFilterRule` call.
async fn create_rule(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "CreateTrafficMirrorFilterRule", params).await
}

/// A `TrafficMirrorFilterRuleId` is `tmfr-` followed by 8 or 17 lowercase hex digits, the
/// short and the long EC2 id form.
fn is_traffic_mirror_filter_rule_id(candidate: &str) -> bool {
    let Some(suffix) = candidate.strip_prefix("tmfr-") else {
        return false;
    };
    matches!(suffix.len(), 8 | 17)
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// One Traffic Mirror rule as a reader reports it: every member of the modelled
/// `TrafficMirrorFilterRule`, read off the XML so the create's answer and the filter's report
/// can be compared member by member rather than one member at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedRule {
    rule_id: Option<String>,
    filter_id: Option<String>,
    direction: Option<String>,
    rule_number: Option<String>,
    rule_action: Option<String>,
    protocol: Option<String>,
    destination_port_range: Option<(Option<String>, Option<String>)>,
    source_port_range: Option<(Option<String>, Option<String>)>,
    destination_cidr_block: Option<String>,
    source_cidr_block: Option<String>,
    description: Option<String>,
    tags: Vec<(String, String)>,
}

/// The port range `name` of `node`, as the two ports it reports.
fn reported_port_range(
    node: roxmltree::Node<'_, '_>,
    name: &str,
) -> Option<(Option<String>, Option<String>)> {
    let range = super::wire::child(node, name)?;
    Some((
        super::wire::child_text(range, "fromPort"),
        super::wire::child_text(range, "toPort"),
    ))
}

/// `node` — an element carrying a `trafficMirrorFilterRuleId` — as the rule it reports.
fn rule_of(node: roxmltree::Node<'_, '_>) -> ReportedRule {
    ReportedRule {
        rule_id: super::wire::child_text(node, "trafficMirrorFilterRuleId"),
        filter_id: super::wire::child_text(node, "trafficMirrorFilterId"),
        direction: super::wire::child_text(node, "trafficDirection"),
        rule_number: super::wire::child_text(node, "ruleNumber"),
        rule_action: super::wire::child_text(node, "ruleAction"),
        protocol: super::wire::child_text(node, "protocol"),
        destination_port_range: reported_port_range(node, "destinationPortRange"),
        source_port_range: reported_port_range(node, "sourcePortRange"),
        destination_cidr_block: super::wire::child_text(node, "destinationCidrBlock"),
        source_cidr_block: super::wire::child_text(node, "sourceCidrBlock"),
        description: super::wire::child_text(node, "description").map(|text| unescaped(&text)),
        tags: super::wire::set_items(node, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    super::wire::child_text(item, "key").unwrap_or_default(),
                    super::wire::child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect(),
    }
}

/// Every Traffic Mirror rule `body` reports, in document order: the elements that carry a
/// rule id, wherever they are nested.
fn reported_rules(case: &str, body: &str) -> Vec<ReportedRule> {
    let document = super::wire::document(case, body);
    document
        .descendants()
        .filter(|node| {
            node.is_element() && super::wire::child(*node, "trafficMirrorFilterRuleId").is_some()
        })
        .map(rule_of)
        .collect()
}

/// The rules a `DescribeTrafficMirrorFilters` body reports in the `set` rule set
/// (`ingressFilterRuleSet` or `egressFilterRuleSet`), in the order it reported them.
fn rules_in_set(case: &str, body: &str, set: &str) -> Vec<ReportedRule> {
    let document = super::wire::document(case, body);
    document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == set)
        .flat_map(|rule_set| {
            rule_set
                .children()
                .filter(roxmltree::Node::is_element)
                .map(rule_of)
                .collect::<Vec<ReportedRule>>()
        })
        .collect()
}

/// What one row of the rule-create table must be reported as, beside the id the service
/// mints and the filter id the request named.
struct ExpectedRule {
    direction: &'static str,
    rule_number: &'static str,
    rule_action: &'static str,
    protocol: Option<&'static str>,
    destination_port_range: Option<(&'static str, &'static str)>,
    source_port_range: Option<(&'static str, &'static str)>,
    destination_cidr_block: &'static str,
    source_cidr_block: &'static str,
    description: Option<&'static str>,
    tags: Vec<(&'static str, &'static str)>,
}

/// The form parameters for one tag on the one resource type a rule's create tags.
fn rule_tag_params(index: usize, key: &str, value: &str) -> Params {
    vec![
        p(
            &format!("TagSpecification.{index}.ResourceType"),
            ec2::state::TRAFFIC_MIRROR_FILTER_RULE_TAGGABLE_RESOURCE,
        ),
        p(&format!("TagSpecification.{index}.Tag.1.Key"), key),
        p(&format!("TagSpecification.{index}.Tag.1.Value"), value),
    ]
}

/// The parameters every rule create needs beside the filter id: a direction, an action, a
/// number and the two ranges the rule matches, so a row only names what it varies.
fn rule_params(
    filter_id: &str,
    direction: &str,
    rule_number: i32,
    rule_action: &str,
    destination_cidr_block: &str,
    source_cidr_block: &str,
) -> Params {
    vec![
        p("TrafficMirrorFilterId", filter_id),
        p("TrafficDirection", direction),
        p("RuleNumber", rule_number.to_string()),
        p("RuleAction", rule_action),
        p("DestinationCidrBlock", destination_cidr_block),
        p("SourceCidrBlock", source_cidr_block),
    ]
}

/// Assert `reported` carries the members `expected` names, as `source` reported them.
fn assert_rule(
    case: &str,
    source: &str,
    reported: &ReportedRule,
    expected: &ExpectedRule,
    filter_id: &str,
) {
    let rule_id = reported.rule_id.clone().unwrap_or_default();
    assert!(
        is_traffic_mirror_filter_rule_id(&rule_id),
        "case {case:?}: expected {source} to report a minted rule id, got {rule_id:?}"
    );
    assert_eq!(
        reported.filter_id.as_deref(),
        Some(filter_id),
        "case {case:?}: expected {source} to report the filter the rule belongs to, got {:?}",
        reported.filter_id
    );
    for (member, actual, want) in [
        ("trafficDirection", &reported.direction, Some(expected.direction)),
        ("ruleNumber", &reported.rule_number, Some(expected.rule_number)),
        ("ruleAction", &reported.rule_action, Some(expected.rule_action)),
        ("protocol", &reported.protocol, expected.protocol),
        (
            "destinationCidrBlock",
            &reported.destination_cidr_block,
            Some(expected.destination_cidr_block),
        ),
        (
            "sourceCidrBlock",
            &reported.source_cidr_block,
            Some(expected.source_cidr_block),
        ),
        ("description", &reported.description, expected.description),
    ] {
        assert_eq!(
            actual.as_deref(),
            want,
            "case {case:?}: expected {source} to report {member} as {want:?}, got {actual:?}"
        );
    }
    for (member, actual, want) in [
        (
            "destinationPortRange",
            &reported.destination_port_range,
            expected.destination_port_range,
        ),
        (
            "sourcePortRange",
            &reported.source_port_range,
            expected.source_port_range,
        ),
    ] {
        let want = want.map(|(from, to)| (Some(from.to_string()), Some(to.to_string())));
        assert_eq!(
            *actual, want,
            "case {case:?}: expected {source} to report {member} as {want:?}, got {actual:?}"
        );
    }
    let tags: Vec<(String, String)> = expected
        .tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    assert_eq!(
        reported.tags, tags,
        "case {case:?}: expected {source} to report the tags {tags:?}, got {:?}",
        reported.tags
    );
}

/// RULE 8: every rule in the table is stored as the request describes it and reads back out
/// of the store through the filter's own report, in that direction's rule set, with the id —
/// and only the id — derived by the service.
#[tokio::test]
async fn every_created_rule_round_trips_through_the_filter() {
    // The rows differ along what the rule generalises over: which direction and which action
    // the rule takes, which address family its ranges are of, whether a protocol is named,
    // whether a port range is named on one side, both or neither, how large the rule number
    // is, and whether a description and tags are carried.
    let rows: Vec<(&str, Params, ExpectedRule)> = vec![
        (
            "an ingress accept rule over every port and protocol",
            Vec::new(),
            ExpectedRule {
                direction: "ingress",
                rule_number: "100",
                rule_action: "accept",
                protocol: None,
                destination_port_range: None,
                source_port_range: None,
                destination_cidr_block: "10.0.0.0/16",
                source_cidr_block: "0.0.0.0/0",
                description: None,
                tags: Vec::new(),
            },
        ),
        (
            "an egress reject rule with a TCP port range on both sides, a description \
             and a tag",
            [
                vec![
                    p("Protocol", "6"),
                    p("DestinationPortRange.FromPort", "443"),
                    p("DestinationPortRange.ToPort", "443"),
                    p("SourcePortRange.FromPort", "1024"),
                    p("SourcePortRange.ToPort", "65535"),
                ],
                rule_tag_params(1, "Team", "platform"),
            ]
            .concat(),
            ExpectedRule {
                direction: "egress",
                rule_number: "1",
                rule_action: "reject",
                protocol: Some("6"),
                destination_port_range: Some(("443", "443")),
                source_port_range: Some(("1024", "65535")),
                destination_cidr_block: "10.0.0.0/16",
                source_cidr_block: "0.0.0.0/0",
                description: Some("to the collector — grün"),
                tags: vec![("Team", "platform")],
            },
        ),
        (
            "an ingress UDP rule over IPv6 ranges with a destination port range only",
            vec![
                p("Protocol", "17"),
                p("DestinationPortRange.FromPort", "53"),
                p("DestinationPortRange.ToPort", "53"),
            ],
            ExpectedRule {
                direction: "ingress",
                rule_number: "32766",
                rule_action: "accept",
                protocol: Some("17"),
                destination_port_range: Some(("53", "53")),
                source_port_range: None,
                destination_cidr_block: "2001:db8::/32",
                source_cidr_block: "::/0",
                description: Some("dns"),
                tags: Vec::new(),
            },
        ),
        (
            "an egress rule over the whole port space on the lowest protocol number",
            vec![
                p("Protocol", "0"),
                p("SourcePortRange.FromPort", "0"),
                p("SourcePortRange.ToPort", "65535"),
            ],
            ExpectedRule {
                direction: "egress",
                rule_number: "5",
                rule_action: "accept",
                protocol: Some("0"),
                destination_port_range: None,
                source_port_range: Some(("0", "65535")),
                destination_cidr_block: "192.168.0.0/24",
                source_cidr_block: "172.16.0.0/12",
                description: None,
                tags: Vec::new(),
            },
        ),
    ];

    for (case, extra, expected) in rows {
        let router = emulator();
        let filter_id = created_id(&router, case, &[p("Description", "under test")]).await;
        let mut params = rule_params(
            &filter_id,
            expected.direction,
            expected.rule_number.parse().unwrap_or_default(),
            expected.rule_action,
            expected.destination_cidr_block,
            expected.source_cidr_block,
        );
        if let Some(description) = expected.description {
            params.push(p("Description", description));
        }
        params.extend(extra.iter().cloned());

        let (status, body) = create_rule(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateTrafficMirrorFilterRule, got \
             {status} with body {body}"
        );
        let created = reported_rules(case, &body);
        assert_eq!(
            created.len(),
            1,
            "case {case:?}: expected the create to report exactly one rule, got {body}"
        );
        assert_rule(case, "the create", &created[0], &expected, &filter_id);

        // ROUND TRIP: the rule is on the filter in the store, not just echoed — and it is in
        // THAT direction's set, with the other direction left empty.
        let (status, described) =
            describe(&router, &indexed("TrafficMirrorFilterId", &[&filter_id])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the describe, got {status} / {described}"
        );
        let (named, other) = if expected.direction == "ingress" {
            ("ingressFilterRuleSet", "egressFilterRuleSet")
        } else {
            ("egressFilterRuleSet", "ingressFilterRuleSet")
        };
        assert_eq!(
            rules_in_set(case, &described, named),
            created,
            "case {case:?}: expected the filter to report the created rule in {named}, \
             got {described}"
        );
        assert!(
            rules_in_set(case, &described, other).is_empty(),
            "case {case:?}: expected no rule in {other}, got {described}"
        );
    }
}

/// RULE 9: a filter reports each direction's rules in ascending rule number order, whatever
/// order they were created in, and the two directions are separate sets — so one number may
/// be used once in each.
#[tokio::test]
async fn a_filters_rules_are_reported_in_ascending_rule_number_per_direction() {
    let router = emulator();
    let filter_id = created_id(&router, "ordering", &[]).await;

    // Created out of order, and the same numbers in both directions, so neither the create
    // order nor the other direction can be what the report follows.
    let created_order = [300, 10, 32766, 1, 120];
    for direction in ["ingress", "egress"] {
        for rule_number in created_order {
            let case = format!("{direction} rule {rule_number}");
            let (status, body) = create_rule(
                &router,
                &rule_params(
                    &filter_id,
                    direction,
                    rule_number,
                    "accept",
                    "10.0.0.0/16",
                    "0.0.0.0/0",
                ),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
        }
    }

    let (status, described) =
        describe(&router, &indexed("TrafficMirrorFilterId", &[&filter_id])).await;
    assert_eq!(
        status, 200,
        "the describe must report the filter, got {status} / {described}"
    );
    let mut ascending: Vec<String> = created_order.iter().map(i32::to_string).collect();
    ascending.sort_by_key(|number| number.parse::<i32>().unwrap_or_default());
    for set in ["ingressFilterRuleSet", "egressFilterRuleSet"] {
        let reported: Vec<String> = rules_in_set("ordering", &described, set)
            .into_iter()
            .map(|rule| rule.rule_number.unwrap_or_default())
            .collect();
        assert_eq!(
            reported, ascending,
            "case {set:?}: expected the rules in ascending rule number order ({ascending:?}), \
             got {reported:?}"
        );
    }
}

/// RULE 10: one rule number in one direction of one filter names ONE rule, and a rule needs
/// the filter that holds it — so a second create at that number in that direction is refused,
/// the same number in the other direction is not, and a filter id the account holds nothing
/// for is refused whatever it looks like.
#[tokio::test]
async fn a_repeated_rule_number_and_an_unknown_filter_are_refused() {
    let router = emulator();
    let filter_id = created_id(&router, "conflicts", &[]).await;
    let first = rule_params(&filter_id, "ingress", 100, "accept", "10.0.0.0/16", "0.0.0.0/0");
    let (status, body) = create_rule(&router, &first).await;
    assert_eq!(
        status, 200,
        "the first rule must be stored, got {status} with body {body}"
    );
    let stored = reported_rules("conflicts", &body);

    // The rows differ along what the second create changes: nothing at all, the action, and
    // the ranges — one rule number in one direction names one rule however the rest reads.
    let rows: Vec<(&str, Params)> = vec![
        ("the identical request again", first.clone()),
        (
            "the same number with another action",
            rule_params(&filter_id, "ingress", 100, "reject", "10.0.0.0/16", "0.0.0.0/0"),
        ),
        (
            "the same number over other ranges",
            rule_params(&filter_id, "ingress", 100, "accept", "10.1.0.0/16", "10.2.0.0/16"),
        ),
    ];
    for (case, params) in rows {
        let (status, body) = create_rule(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a repeated rule number, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "TrafficMirrorFilterRuleAlreadyExists",
            "case {case:?}: expected TrafficMirrorFilterRuleAlreadyExists, got {body}"
        );
    }

    // The other direction is a separate numbered set, so the same number is free there.
    let (status, body) = create_rule(
        &router,
        &rule_params(&filter_id, "egress", 100, "accept", "10.0.0.0/16", "0.0.0.0/0"),
    )
    .await;
    assert_eq!(
        status, 200,
        "the same number in the other direction must be stored, got {status} / {body}"
    );

    // A filter id that names no filter of the account, whatever shape it has.
    for unknown in [
        "tmf-00000000000000099",
        "tmf-0123abcd",
        "tmf-not-hex-at-all",
        "vpc-00000000000000001",
        "not-an-id",
    ] {
        let (status, body) = create_rule(
            &router,
            &rule_params(unknown, "ingress", 7, "accept", "10.0.0.0/16", "0.0.0.0/0"),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {unknown:?}: expected HTTP 400 for an unknown filter, got {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidTrafficMirrorFilterId.NotFound",
            "case {unknown:?}: expected InvalidTrafficMirrorFilterId.NotFound, got {body}"
        );
    }

    // Nothing a refusal reported is in the store: the filter still holds the one ingress rule
    // it was given, and the one egress rule.
    let (status, described) =
        describe(&router, &indexed("TrafficMirrorFilterId", &[&filter_id])).await;
    assert_eq!(
        status, 200,
        "the describe must report the filter, got {status} / {described}"
    );
    assert_eq!(
        rules_in_set("conflicts", &described, "ingressFilterRuleSet"),
        stored,
        "the refused creates left the one ingress rule alone, got {described}"
    );
    assert_eq!(
        rules_in_set("conflicts", &described, "egressFilterRuleSet").len(),
        1,
        "the filter holds the one egress rule it was given, got {described}"
    );
}

/// RULE 11: the create holds the request to every bound and form this crate publishes — a
/// value AT the bound is stored, one PAST it is refused — and it decides that before the
/// first write, so the same request under `DryRun` is refused with the reason it would have
/// failed for rather than with `DryRunOperation`, and no refused create leaves a rule behind.
#[tokio::test]
async fn the_rule_create_holds_every_published_bound_before_it_writes() {
    // The bounds come from the crate's published constants: `MAX_PORT` (the largest TCP or
    // UDP port), `MAX_PROTOCOL_NUMBER` (an IP protocol number occupies one byte) and
    // `MAX_CLIENT_TOKEN_CHARS`. Each is asserted AT the bound and one PAST it, beside the
    // forms a member has to take at all. `None` is a row that must be STORED; every row
    // carries a rule number of its own, so no row is refused for another row's rule.
    const MAX_PORT: i32 = ec2::state::MAX_PORT;
    const MAX_PROTOCOL_NUMBER: i32 = ec2::state::MAX_PROTOCOL_NUMBER;
    type BoundRow = (String, Params, Option<&'static str>);

    let router = emulator();
    let filter_id = created_id(&router, "bounds", &[]).await;
    let base = |rule_number: i32| {
        rule_params(
            &filter_id,
            "ingress",
            rule_number,
            "accept",
            "10.0.0.0/16",
            "0.0.0.0/0",
        )
    };
    // A row's parameters: the common ones at its own rule number, plus what it varies.
    let row = |rule_number: i32, extra: Params| [base(rule_number), extra].concat();
    // A row that drops one of the common parameters, to send a request without it.
    let without = |rule_number: i32, dropped: &str| {
        base(rule_number)
            .into_iter()
            .filter(|(key, _)| key != dropped)
            .collect::<Params>()
    };

    let rows: Vec<BoundRow> = vec![
        (
            format!("a port range at the published bound of {MAX_PORT}"),
            row(
                10,
                vec![
                    p("DestinationPortRange.FromPort", MAX_PORT.to_string()),
                    p("DestinationPortRange.ToPort", MAX_PORT.to_string()),
                ],
            ),
            None,
        ),
        (
            format!("a port of {}", MAX_PORT + 1),
            row(
                11,
                vec![
                    p("DestinationPortRange.FromPort", "1"),
                    p("DestinationPortRange.ToPort", (MAX_PORT + 1).to_string()),
                ],
            ),
            Some("InvalidParameterValue"),
        ),
        (
            "a port of -1".to_string(),
            row(
                12,
                vec![
                    p("SourcePortRange.FromPort", "-1"),
                    p("SourcePortRange.ToPort", "80"),
                ],
            ),
            Some("InvalidParameterValue"),
        ),
        (
            "a port range at the lowest port".to_string(),
            row(
                13,
                vec![
                    p("SourcePortRange.FromPort", "0"),
                    p("SourcePortRange.ToPort", "0"),
                ],
            ),
            None,
        ),
        (
            "a port range whose first port is above its last".to_string(),
            row(
                14,
                vec![
                    p("SourcePortRange.FromPort", "500"),
                    p("SourcePortRange.ToPort", "400"),
                ],
            ),
            Some("InvalidParameterValue"),
        ),
        (
            "a port range naming only its first port".to_string(),
            row(15, vec![p("SourcePortRange.FromPort", "500")]),
            Some("MissingParameter"),
        ),
        (
            "a port range naming only its last port".to_string(),
            row(16, vec![p("DestinationPortRange.ToPort", "500")]),
            Some("MissingParameter"),
        ),
        (
            format!("the protocol at the published bound of {MAX_PROTOCOL_NUMBER}"),
            row(17, vec![p("Protocol", MAX_PROTOCOL_NUMBER.to_string())]),
            None,
        ),
        (
            format!("a protocol of {}", MAX_PROTOCOL_NUMBER + 1),
            row(18, vec![p("Protocol", (MAX_PROTOCOL_NUMBER + 1).to_string())]),
            Some("InvalidParameterValue"),
        ),
        (
            "a protocol of -1".to_string(),
            row(19, vec![p("Protocol", "-1")]),
            Some("InvalidParameterValue"),
        ),
        (
            "a destination CIDR block that is no address prefix".to_string(),
            [
                without(20, "DestinationCidrBlock"),
                vec![p("DestinationCidrBlock", "10.0.0")],
            ]
            .concat(),
            Some("InvalidParameterValue"),
        ),
        (
            "a source CIDR block with an impossible prefix length".to_string(),
            [
                without(21, "SourceCidrBlock"),
                vec![p("SourceCidrBlock", "10.0.0.0/33")],
            ]
            .concat(),
            Some("InvalidParameterValue"),
        ),
        (
            "no destination CIDR block at all".to_string(),
            without(22, "DestinationCidrBlock"),
            Some("MissingParameter"),
        ),
        (
            "no source CIDR block at all".to_string(),
            without(23, "SourceCidrBlock"),
            Some("MissingParameter"),
        ),
        (
            "no traffic direction at all".to_string(),
            without(24, "TrafficDirection"),
            Some("MissingParameter"),
        ),
        (
            "no rule action at all".to_string(),
            without(25, "RuleAction"),
            Some("MissingParameter"),
        ),
        (
            "no filter id at all".to_string(),
            without(26, "TrafficMirrorFilterId"),
            Some("MissingParameter"),
        ),
        (
            "the lowest rule number".to_string(),
            base(1),
            None,
        ),
        (
            "a rule number of 0, which is also what a request naming none carries".to_string(),
            base(0),
            Some("MissingParameter"),
        ),
        (
            "a negative rule number".to_string(),
            base(-5),
            Some("InvalidParameterValue"),
        ),
        (
            format!("a ClientToken of {MAX_CLIENT_TOKEN_CHARS} characters"),
            row(
                27,
                vec![p("ClientToken", "t".repeat(MAX_CLIENT_TOKEN_CHARS))],
            ),
            None,
        ),
        (
            format!("a ClientToken of {} characters", MAX_CLIENT_TOKEN_CHARS + 1),
            row(
                28,
                vec![p("ClientToken", "t".repeat(MAX_CLIENT_TOKEN_CHARS + 1))],
            ),
            Some("InvalidParameterValue"),
        ),
        (
            format!("{MAX_TAGS_PER_RESOURCE} tags on the rule"),
            row(29, many_rule_tag_params(MAX_TAGS_PER_RESOURCE)),
            None,
        ),
        (
            format!("{} tags on the rule", MAX_TAGS_PER_RESOURCE + 1),
            row(30, many_rule_tag_params(MAX_TAGS_PER_RESOURCE + 1)),
            Some("TagLimitExceeded"),
        ),
        (
            "a tag specification naming the filter rather than the rule".to_string(),
            row(
                31,
                vec![
                    p("TagSpecification.1.ResourceType", TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE),
                    p("TagSpecification.1.Tag.1.Key", "Team"),
                    p("TagSpecification.1.Tag.1.Value", "platform"),
                ],
            ),
            Some("InvalidParameterValue"),
        ),
        (
            "a tag specification naming no resource type".to_string(),
            row(
                32,
                vec![
                    p("TagSpecification.1.Tag.1.Key", "Team"),
                    p("TagSpecification.1.Tag.1.Value", "platform"),
                ],
            ),
            Some("MissingParameter"),
        ),
    ];

    let mut stored: Vec<String> = Vec::new();
    for (case, params, refusal) in &rows {
        let (status, body) = create_rule(&router, params).await;
        match refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a value at the published bound is stored, got {status} \
                     with body {body}"
                );
                let rule_id = element(&body, "trafficMirrorFilterRuleId").unwrap_or_default();
                assert!(
                    is_traffic_mirror_filter_rule_id(&rule_id),
                    "case {case:?}: expected a minted rule id, got {rule_id:?}"
                );
                stored.push(rule_id);
            }
            Some(code) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400 for a request past the bound, got \
                     {status} with body {body}"
                );
                assert_eq!(
                    error_code(&body).as_str(),
                    *code,
                    "case {case:?}: expected {code}, got {body}"
                );
            }
        }
    }

    // The same table under `DryRun`: the flag asks whether the request WOULD have worked, so
    // a row that would have been refused keeps its own refusal and a row that would have
    // succeeded answers `DryRunOperation` (HTTP 412) — except that a row that was STORED has
    // taken its rule number, so repeating it is now refused for THAT, unless the row carried a
    // `ClientToken`, which makes the repeat a replay of the rule it already created.
    for (case, params, refusal) in &rows {
        let case = format!("{case}, under DryRun");
        let replayed = params.iter().any(|(key, _)| key == "ClientToken");
        let params = [params.clone(), vec![p("DryRun", "true")]].concat();
        let (status, body) = create_rule(&router, &params).await;
        let (expected_status, expected_code) = match refusal {
            None if replayed => (412, "DryRunOperation"),
            None => (400, "TrafficMirrorFilterRuleAlreadyExists"),
            Some(code) => (400, *code),
        };
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
    }

    // A request that would have worked, under `DryRun`, at a rule number nothing holds: the
    // verdict, and no rule stored for it.
    let (status, body) = create_rule(
        &router,
        &[base(999), vec![p("DryRun", "true")]].concat(),
    )
    .await;
    assert_eq!(
        status, 412,
        "a dry run that would have worked answers the verdict, got {status} / {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "a dry run that would have worked answers DryRunOperation, got {body}"
    );

    let (status, described) =
        describe(&router, &indexed("TrafficMirrorFilterId", &[&filter_id])).await;
    assert_eq!(
        status, 200,
        "the describe must report the filter, got {status} / {described}"
    );
    let mut reported: Vec<String> = rules_in_set("bounds", &described, "ingressFilterRuleSet")
        .into_iter()
        .filter_map(|rule| rule.rule_id)
        .collect();
    reported.sort();
    stored.sort();
    assert_eq!(
        reported, stored,
        "only the creates at the published bounds are stored, got {reported:?}"
    );
}

/// The form parameters for `count` tags in ONE specification of the resource type a rule's
/// create tags, so a row can carry a tag list of any size.
fn many_rule_tag_params(count: usize) -> Params {
    let mut params = vec![p(
        "TagSpecification.1.ResourceType",
        ec2::state::TRAFFIC_MIRROR_FILTER_RULE_TAGGABLE_RESOURCE,
    )];
    for index in 1..=count {
        params.push(p(
            &format!("TagSpecification.1.Tag.{index}.Key"),
            format!("Key{index}"),
        ));
        params.push(p(
            &format!("TagSpecification.1.Tag.{index}.Value"),
            format!("value-{index}"),
        ));
    }
    params
}

/// RULE 12: one `ClientToken` names one rule — a replay reports that rule and creates
/// nothing, even where the repeat would otherwise be refused for the rule number it takes —
/// while a create carrying no token is never a replay.
#[tokio::test]
async fn one_client_token_names_one_rule() {
    let router = emulator();
    let filter_id = created_id(&router, "idempotency", &[]).await;

    // The rows differ along how much of the record the replay shares with the first request
    // and what else it carries: the identical request, and one differing in a member a
    // replayed token makes no difference to.
    let rows: Vec<(&str, i32, Params)> = vec![
        ("the identical request", 40, Vec::new()),
        (
            "a repeat naming a description the first request did not",
            41,
            vec![p("Description", "second")],
        ),
    ];
    for (case, rule_number, differing) in rows {
        let token = format!("token-{rule_number}");
        let first = [
            rule_params(
                &filter_id,
                "ingress",
                rule_number,
                "accept",
                "10.0.0.0/16",
                "0.0.0.0/0",
            ),
            vec![p("ClientToken", token.clone())],
        ]
        .concat();
        let (status, body) = create_rule(&router, &first).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "clientToken").as_deref(),
            Some(token.as_str()),
            "case {case:?}: the response echoes the token the request carried, got {body}"
        );
        let created = reported_rules(case, &body);

        let replay = [first.clone(), differing].concat();
        let (status, replayed) = create_rule(&router, &replay).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a replay succeeds, got {status} with body {replayed}"
        );
        assert_eq!(
            reported_rules(case, &replayed),
            created,
            "case {case:?}: a replay reports the rule the token created, got {replayed}"
        );
    }

    // A create carrying no token is never a replay: "Unique, case-sensitive identifier that
    // YOU provide" is what makes one, so two identical tokenless requests at two rule numbers
    // mint two rules and neither response echoes a token.
    let mut tokenless: Vec<String> = Vec::new();
    for rule_number in [50, 51] {
        let (status, body) = create_rule(
            &router,
            &rule_params(
                &filter_id,
                "egress",
                rule_number,
                "accept",
                "10.0.0.0/16",
                "0.0.0.0/0",
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {rule_number}: expected HTTP 200, got {status} with body {body}"
        );
        assert!(
            element(&body, "clientToken").is_none(),
            "case {rule_number}: a request carrying no token is answered with none, got {body}"
        );
        tokenless.push(element(&body, "trafficMirrorFilterRuleId").unwrap_or_default());
    }
    assert_ne!(
        tokenless[0], tokenless[1],
        "two tokenless creates mint two rules, got {tokenless:?}"
    );

    let (status, described) =
        describe(&router, &indexed("TrafficMirrorFilterId", &[&filter_id])).await;
    assert_eq!(
        status, 200,
        "the describe must report the filter, got {status} / {described}"
    );
    assert_eq!(
        rules_in_set("idempotency", &described, "ingressFilterRuleSet").len(),
        2,
        "each token stored one ingress rule, got {described}"
    );
    let egress: Vec<String> = rules_in_set("idempotency", &described, "egressFilterRuleSet")
        .into_iter()
        .filter_map(|rule| rule.rule_id)
        .collect();
    assert_eq!(
        egress, tokenless,
        "every tokenless create is stored on its own, got {described}"
    );
}
