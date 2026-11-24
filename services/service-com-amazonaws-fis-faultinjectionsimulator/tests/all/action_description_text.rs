// SPDX-License-Identifier: CC0-1.0

//! The DESCRIPTION text FIS's service-owned action catalog serves, end to end over HTTP.
//!
//! Every assertion here goes through the router, so each case exercises routing,
//! deserialization, the handler and serialization — the same path a client takes.
//!
//! The rule under test: an action's `description` is fixed service-owned text keyed by the
//! action's id. A reader therefore reports the string published for THAT id, character for
//! character — wording, parentheses and closing punctuation included — and never a sentence
//! of this emulator's own about what the action does. Because one table is behind both
//! readers, the consequences are assertable without knowing a single string: the listing and
//! the point read answer the same text for the same id, the text does not depend on the page
//! the id is reported on, on the Region the request is signed for, or on how often it is
//! read, and every string stays inside `ActionDescription`'s modelled bounds.
//!
//! SOURCE for the expected strings in [`PUBLISHED_ACTION_TEXT`]: the published FIS action
//! catalog, as the recorded `ListActions` page for these ids establishes it — the Smithy
//! model constrains `ActionDescription` (`@length(min: 0, max: 512)`, `pattern ^[\s\S]+$`)
//! and documents it only as "The description for the action.", so the TEXT is published data
//! and not something the model carries. The strings are written out here rather than imported
//! from `src/state/catalog.rs`, so a row restated in that table fails this suite instead of
//! agreeing with it. Rows whose published string is not established carry no row here: an
//! expectation written from this emulator's own table would assert nothing.
//!
//! The HTTP harness is the shared one in `experiment_lifecycle.rs`.

use std::collections::BTreeMap;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::experiment_lifecycle::{emulator, send, FALLBACK_REGION};

/// `ActionDescription`'s modelled ceiling (`@length(min: 0, max: 512)`).
const DESCRIPTION_CEILING: usize = 512;

/// `ListActionsMaxResults`' modelled bounds (`@range(min: 1, max: 100)`).
const MAX_RESULTS_FLOOR: i32 = 1;
/// See [`MAX_RESULTS_FLOOR`].
const MAX_RESULTS_CEILING: i32 = 100;

/// A second Region to sign with: an action is service-owned data, so the Region decides the
/// ARN a reader reports and nothing about the text.
const OTHER_REGION: &str = "eu-west-1";

/// The published description of each action id whose string the recorded catalog page
/// establishes.
///
/// The rows differ along what the rule generalises over, because the published strings follow
/// no one pattern and a row's text cannot be derived from a sibling's:
///
/// * the opening — `It ` and the command the fault runs (`aws:ecs:task-kill-process`), the
///   verb in the third person (`aws:eks:pod-*-stress`, `aws:eks:inject-kubernetes-custom-
///   resource`), or the imperative (`aws:rds:reboot-db-instances`);
/// * the length and shape — a bare sentence, a sentence carrying a parenthetical with a
///   comma-separated value list and a `|` alternation (`aws:ecs:task-network-latency`,
///   `aws:ecs:task-network-packet-loss`), and a sentence whose subject clause explains when
///   the fault is useful (`aws:ecs:task-network-blackhole-port`);
/// * the subject the text names — the process, the traffic, the pods, the cluster, the DB
///   instances — which is published per action and not a plural of the action's own id;
/// * case the reference spells unconventionally and which is published as-is
///   (`kubernetes`, lower case, for `aws:eks:inject-kubernetes-custom-resource`).
const PUBLISHED_ACTION_TEXT: &[(&str, &str)] = &[
    (
        "aws:ecs:task-kill-process",
        "It kills a particular process by name, using the killall command.",
    ),
    (
        "aws:ecs:task-network-blackhole-port",
        "It drops incoming or outgoing traffic for a configurable protocol (tcp or udp) and \
         port which is useful for simulating dependency failures.",
    ),
    (
        "aws:ecs:task-network-latency",
        "It adds latency, with jitter, to outgoing or incoming traffic from a configurable \
         list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3).",
    ),
    (
        "aws:ecs:task-network-packet-loss",
        "It adds packet loss to outgoing or incoming traffic from a configurable list of \
         sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3).",
    ),
    (
        "aws:eks:inject-kubernetes-custom-resource",
        "Injects the specified kubernetes custom resource in the target EKS cluster.",
    ),
    (
        "aws:eks:pod-cpu-stress",
        "Runs CPU stress on the target pods.",
    ),
    ("aws:eks:pod-io-stress", "Runs IO stress on the target pods."),
    (
        "aws:eks:pod-memory-stress",
        "Runs memory stress on the target pods.",
    ),
    (
        "aws:rds:reboot-db-instances",
        "Reboot the specified DB instances.",
    ),
];

/// Every action `ListActions` reports, walked at `page_size` and concatenated in the order
/// the pages reported them.
async fn listed_actions(
    router: &MultiServiceRouter,
    page_size: Option<i32>,
    region: &str,
) -> Vec<Value> {
    let case = format!("ListActions at maxResults={page_size:?} signed for {region}");
    let mut collected: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    for page in 0..=256 {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("maxResults={size}"));
        }
        if let Some(token) = &next {
            query.push(format!("nextToken={token}"));
        }
        let uri = if query.is_empty() {
            "/actions".to_string()
        } else {
            format!("/actions?{}", query.join("&"))
        };
        let answer = send(router, "GET", &uri, None, region).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?} page {page}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let actions = answer
            .member("actions")
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?} page {page}: expected an actions list, got {:?}",
                    answer.body
                )
            })
            .clone();
        collected.extend(actions);
        match answer.body["nextToken"].as_str() {
            None => {
                assert!(
                    !collected.is_empty(),
                    "case {case:?}: expected the published catalog to report some action"
                );
                return collected;
            }
            Some(token) => next = Some(token.to_string()),
        }
    }
    panic!("case {case:?}: pagination did not terminate");
}

/// The id -> description map a page walk reported.
fn text_by_id(case: &str, listed: &[Value]) -> BTreeMap<String, String> {
    listed
        .iter()
        .map(|action| {
            let id = action["id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no id on a listed action: {action:?}"))
                .to_string();
            let description = action["description"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no description on the action {id:?}: {action:?}")
                })
                .to_string();
            (id, description)
        })
        .collect()
}

/// The `description` `GetAction` reports for `id`.
async fn read_description(router: &MultiServiceRouter, id: &str, region: &str) -> String {
    let answer = send(router, "GET", &format!("/actions/{id}"), None, region).await;
    assert_eq!(
        answer.status, 200,
        "case {id:?}: expected HTTP 200 reading a published action, got {} with {:?}",
        answer.status, answer.body
    );
    answer.member("action")["description"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {id:?}: expected GetAction to report a description, got {:?}",
                answer.body
            )
        })
        .to_string()
}

#[tokio::test]
async fn an_action_reports_its_published_description_from_both_readers() {
    let router = emulator();
    // A page size that cuts the catalog into several pages, so a row's text is asserted
    // wherever in the listing it falls.
    let listed = text_by_id(
        "published rows",
        &listed_actions(&router, Some(4), FALLBACK_REGION).await,
    );

    for (id, published) in PUBLISHED_ACTION_TEXT {
        let case = *id;
        let reported = listed.get(*id).map(String::as_str);
        assert_eq!(
            reported,
            Some(*published),
            "case {case:?}: expected the listing to publish {published:?}, got {reported:?}"
        );

        let read = read_description(&router, id, FALLBACK_REGION).await;
        assert_eq!(
            read, *published,
            "case {case:?}: expected the point read to report {published:?}, got {read:?}"
        );
    }
}

#[tokio::test]
async fn the_whole_catalog_reports_one_text_per_id_from_either_reader() {
    let router = emulator();
    let listed = text_by_id(
        "the whole catalog",
        &listed_actions(&router, None, FALLBACK_REGION).await,
    );

    // Driven from the listing itself, so this holds for every published action and not only
    // for the rows written out above.
    for (id, published) in &listed {
        let case = id.as_str();
        let read = read_description(&router, id, FALLBACK_REGION).await;
        assert_eq!(
            &read, published,
            "case {case:?}: expected the point read to report the text the listing published \
             ({published:?}), got {read:?}"
        );

        // A read reports published data and changes nothing, so asking again — and asking the
        // other reader again — answers the same text.
        let again = read_description(&router, id, FALLBACK_REGION).await;
        assert_eq!(
            again, read,
            "case {case:?}: expected a second read to report {read:?}, got {again:?}"
        );

        assert!(
            !published.is_empty() && published.chars().count() <= DESCRIPTION_CEILING,
            "case {case:?}: expected a description matching ^[\\s\\S]+$ of at most \
             {DESCRIPTION_CEILING} characters, got {published:?}"
        );
    }

    let relisted = text_by_id(
        "the whole catalog, re-listed",
        &listed_actions(&router, None, FALLBACK_REGION).await,
    );
    assert_eq!(
        relisted, listed,
        "case re-list: expected the listing to publish the same text it did before, got \
         {relisted:?}"
    );
}

#[tokio::test]
async fn the_published_text_does_not_depend_on_the_page_size_or_the_region() {
    let router = emulator();
    let catalog_size = i32::try_from(
        listed_actions(&router, Some(MAX_RESULTS_CEILING), FALLBACK_REGION)
            .await
            .len(),
    )
    .expect("the catalog size fits an i32");
    let expected = text_by_id(
        "baseline",
        &listed_actions(&router, None, FALLBACK_REGION).await,
    );

    // Each case is a page size: the modelled floor (one action per page), a size that cuts
    // the catalog into several pages, exactly the catalog, one past it, and the modelled
    // ceiling; and, at the end, the whole catalog read under another signing Region, which
    // decides the ARN a reader reports and nothing about the text.
    for page_size in [
        Some(MAX_RESULTS_FLOOR),
        Some(7),
        Some(catalog_size),
        Some(catalog_size + 1),
        Some(MAX_RESULTS_CEILING),
    ] {
        let case = format!("maxResults={page_size:?}");
        let served = text_by_id(&case, &listed_actions(&router, page_size, FALLBACK_REGION).await);
        assert_eq!(
            served, expected,
            "case {case:?}: expected the published text of every action to be the text the \
             unpaged listing published, got {served:?}"
        );
    }

    let case = format!("signed for {OTHER_REGION}");
    let served = text_by_id(&case, &listed_actions(&router, Some(9), OTHER_REGION).await);
    assert_eq!(
        served, expected,
        "case {case:?}: expected the published text of every action to be Region-independent, \
         got {served:?}"
    );
    for id in expected.keys() {
        let read = read_description(&router, id, OTHER_REGION).await;
        let published = &expected[id];
        assert_eq!(
            &read, published,
            "case {case:?} ({id:?}): expected the point read to report {published:?}, got \
             {read:?}"
        );
    }
}
