// SPDX-License-Identifier: CC0-1.0

//! Whether a directory's event-topic binding is reported as one that HOLDS, driven end to
//! end over the wire with Directory Service and Amazon SNS mounted on ONE engine — the
//! shape `core/router/src/lib.rs` mounts them in — so the topic a binding points at is
//! resolved where a client creates and deletes it.
//!
//! The rule under test: the `Status` `DescribeEventTopics` reports for a registered
//! association is `Registered` only while the service that OWNS Amazon SNS topics holds the
//! topic the association's ARN names, and the modelled `Topic not found` whenever it does
//! not — whether the topic was deleted after the binding was made, was never created, or
//! exists only under another name or in another Region. A directory's event topic is its
//! ONLY notification channel, so a binding reported `Registered` while nothing can be
//! published to it is silent misconfiguration.
//!
//! The rows differ along what the rule generalises over: whether the topic is held at all,
//! WHEN it stopped being held (deleted after the binding, never created), the FORM and
//! LENGTH of the topic name (both published `TopicName` bounds, mixed case, underscores and
//! hyphens), the REGION the topic was created in, and HOW MANY bindings one directory
//! carries, so a status cannot pass by being the only one in the answer.

use std::sync::Arc;

use serde_json::json;
// Every value the modelled `TopicStatus` enum declares, taken from the list this crate
// generates off the model rather than restated here: a reported status has to be one of
// them.
use service_com_amazonaws_directoryservice_directoryservice_20150416::ops::types::TOPIC_STATUS_VALUES;
use smithy_server_runtime::MultiServiceRouter;

use crate::directory_lifecycle::{create_directory, emulator_on, minimal_create, ok, REGION};
use crate::event_topics::describe;

/// The `TopicStatus` a binding whose topic SNS holds reports, and the one it reports when
/// SNS holds no such topic: two values of the modelled `TopicStatus` enum
/// (`smithy_query(query="shape",shape="TopicStatus")` answers
/// `["Registered", "Topic not found", "Failed", "Deleted"]`).
pub(crate) const STATUS_REGISTERED: &str = "Registered";
pub(crate) const STATUS_TOPIC_NOT_FOUND: &str = "Topic not found";

/// The published upper bound of the modelled `TopicName` shape (`length 1..256`), which is
/// also SNS's own documented bound on `CreateTopicInput$Name`.
const TOPIC_NAME_MAX_LEN: usize = 256;

/// A Region that is NOT the one the Directory Service calls here are signed for, for the
/// row whose topic exists only somewhere else: "This Amazon SNS topic must be in the same
/// region as the specified Directory ID" (`RegisterEventTopicRequest$TopicName`).
const OTHER_REGION: &str = "eu-west-1";

/// Directory Service and Amazon SNS on ONE engine, which is what lets a binding's topic be
/// created and deleted by the service that owns it.
///
/// `pub(crate)` so the sibling event-topic suites drive the same two services rather than
/// standing up a second harness.
pub(crate) fn ds_and_sns() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = emulator_on(Arc::clone(&engine));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(engine),
    );
    router
}

/// One Amazon SNS call over its `awsQuery` protocol, signed (in scope only) for `region`,
/// returning the HTTP status and the response body.
async fn sns_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
    region: &str,
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&percent_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDSNSTEST/20260101/{region}/sns/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// Create the topic `name` in `region` through Amazon SNS' own API, returning the `TopicArn`
/// that service answered with.
///
/// `pub(crate)` so the sibling event-topic suites give a topic to its owner the same way.
pub(crate) async fn create_topic(
    router: &MultiServiceRouter,
    name: &str,
    region: &str,
    case: &str,
) -> String {
    let (status, body) = sns_call(router, "CreateTopic", &[("Name", name)], region).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected sns:CreateTopic for {name:?} to answer 200, got {status} with \
         {body}"
    );
    element(&body, "TopicArn").unwrap_or_else(|| {
        panic!("case {case:?}: expected a TopicArn for {name:?}, got {body}")
    })
}

/// Delete the topic `topic_arn` through Amazon SNS' own API, so the binding that points at
/// it points at nothing.
async fn delete_topic(router: &MultiServiceRouter, topic_arn: &str, region: &str, case: &str) {
    let (status, body) = sns_call(
        router,
        "DeleteTopic",
        &[("TopicArn", topic_arn)],
        region,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected sns:DeleteTopic for {topic_arn} to answer 200, got {status} \
         with {body}"
    );
}

/// `RegisterEventTopic` for `topic_name`, asserting the binding was accepted: a binding is
/// recorded whether or not SNS holds the topic — which status it is reported in is the
/// rule under test.
async fn bind(router: &MultiServiceRouter, directory_id: &str, topic_name: &str, case: &str) {
    ok(
        router,
        "RegisterEventTopic",
        json!({ "DirectoryId": directory_id, "TopicName": topic_name }),
        case,
    )
    .await;
}

/// The `Status` the one association for `topic_name` is reported in, through
/// `DescribeEventTopics` narrowed to that directory.
async fn reported_status(
    router: &MultiServiceRouter,
    directory_id: &str,
    topic_name: &str,
    case: &str,
) -> String {
    let reported = describe(router, json!({ "DirectoryId": directory_id }), case).await;
    let found = reported
        .iter()
        .find(|topic| topic["TopicName"].as_str() == Some(topic_name))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an association for {topic_name:?}, got {reported:?}")
        });
    let status = found["Status"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Status, got {found}"))
        .to_string();
    assert!(
        TOPIC_STATUS_VALUES.contains(&status.as_str()),
        "case {case:?}: expected a modelled TopicStatus one of {TOPIC_STATUS_VALUES:?}, got \
         {status:?}"
    );
    status
}

/// Percent-encode an `awsQuery` form value with the `percent-encoding` crate: encoding
/// every non-alphanumeric byte is a superset of what a form value needs and covers the `:`
/// of a topic ARN, so the value survives the round trip into SNS' own parser.
fn percent_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<tag>…</tag>` element of an `awsQuery` response envelope.
fn element(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = body.find(&open)? + open.len();
    let rest = &body[start..];
    let end = rest.find(&format!("</{tag}>"))?;
    Some(rest[..end].to_string())
}

/// How the topic a binding points at comes to be (or not be) held by its owner.
enum Topic {
    /// Created in the signing Region and left there.
    Held,
    /// Created in the signing Region, then deleted — the channel that broke after it was
    /// wired up.
    Deleted,
    /// Never created at all: a name SNS never held.
    Absent,
    /// Created under `name` in the signing Region, while the binding names something else:
    /// the binding resolves to a topic nobody created.
    HeldUnderAnotherName(&'static str),
    /// Created in another Region, where a directory's topic may not live.
    HeldInAnotherRegion,
}

/// One row: the topic name a binding carries, how its topic is (not) held, and the status
/// the binding must therefore be reported in.
struct StatusCase {
    case: &'static str,
    topic_name: String,
    topic: Topic,
    expected: &'static str,
}

#[tokio::test]
async fn the_status_follows_whether_sns_still_holds_the_topic() {
    let router = ds_and_sns();

    let rows = vec![
        StatusCase {
            case: "a topic SNS holds",
            topic_name: "ds-status".to_string(),
            topic: Topic::Held,
            expected: STATUS_REGISTERED,
        },
        StatusCase {
            case: "a mixed-form name SNS holds",
            topic_name: "Topic_1-ok".to_string(),
            topic: Topic::Held,
            expected: STATUS_REGISTERED,
        },
        StatusCase {
            case: "the shortest name the model admits, held",
            topic_name: "a".to_string(),
            topic: Topic::Held,
            expected: STATUS_REGISTERED,
        },
        StatusCase {
            case: "the longest name the model admits, held",
            topic_name: "x".repeat(TOPIC_NAME_MAX_LEN),
            topic: Topic::Held,
            expected: STATUS_REGISTERED,
        },
        StatusCase {
            case: "a topic deleted after the binding was made",
            topic_name: "ds-status-gone".to_string(),
            topic: Topic::Deleted,
            expected: STATUS_TOPIC_NOT_FOUND,
        },
        StatusCase {
            case: "the longest name the model admits, deleted",
            topic_name: "y".repeat(TOPIC_NAME_MAX_LEN),
            topic: Topic::Deleted,
            expected: STATUS_TOPIC_NOT_FOUND,
        },
        StatusCase {
            case: "a name SNS never held",
            topic_name: "ds-evt-never-created".to_string(),
            topic: Topic::Absent,
            expected: STATUS_TOPIC_NOT_FOUND,
        },
        StatusCase {
            case: "a name differing from a held one in case alone",
            topic_name: "ds-casing".to_string(),
            topic: Topic::HeldUnderAnotherName("DS-CASING"),
            expected: STATUS_TOPIC_NOT_FOUND,
        },
        StatusCase {
            case: "a name differing from a held one by one character",
            topic_name: "ds-status-2".to_string(),
            topic: Topic::HeldUnderAnotherName("ds-status-3"),
            expected: STATUS_TOPIC_NOT_FOUND,
        },
        StatusCase {
            case: "a topic held only in another Region",
            topic_name: "ds-elsewhere".to_string(),
            topic: Topic::HeldInAnotherRegion,
            expected: STATUS_TOPIC_NOT_FOUND,
        },
    ];

    // Every row's binding is made on the SAME directory, so each status is reported beside
    // the others rather than as the only association in the answer.
    let directory_id = create_directory(&router, minimal_create("corp.example.com"), "status").await;

    for row in &rows {
        let case = row.case;
        match &row.topic {
            Topic::Held => {
                create_topic(&router, &row.topic_name, REGION, case).await;
            }
            Topic::Deleted => {
                let arn = create_topic(&router, &row.topic_name, REGION, case).await;
                bind(&router, &directory_id, &row.topic_name, case).await;
                // While it is still held, the binding is the positive control: it reports
                // `Registered` before the topic goes away.
                let before = reported_status(&router, &directory_id, &row.topic_name, case).await;
                assert_eq!(
                    before, STATUS_REGISTERED,
                    "case {case:?}: expected {STATUS_REGISTERED:?} while SNS still held the \
                     topic, got {before:?}"
                );
                delete_topic(&router, &arn, REGION, case).await;
            }
            Topic::Absent => {}
            Topic::HeldUnderAnotherName(other) => {
                create_topic(&router, other, REGION, case).await;
            }
            Topic::HeldInAnotherRegion => {
                create_topic(&router, &row.topic_name, OTHER_REGION, case).await;
            }
        }
        bind(&router, &directory_id, &row.topic_name, case).await;
    }

    for row in &rows {
        let case = row.case;
        let status = reported_status(&router, &directory_id, &row.topic_name, case).await;
        assert_eq!(
            status, row.expected,
            "case {case:?}: expected the binding for {:?} to be reported {:?}, got {status:?}",
            row.topic_name, row.expected
        );
    }

    // The account-wide read reports the same statuses as the per-directory one: the status
    // is a fact about the topic, not about which filter asked.
    let account_wide = describe(&router, json!({}), "status: account-wide").await;
    for row in &rows {
        let found = account_wide
            .iter()
            .find(|topic| topic["TopicName"].as_str() == Some(row.topic_name.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the account-wide read to carry the binding, got \
                     {account_wide:?}",
                    row.case
                )
            });
        assert_eq!(
            found["Status"],
            json!(row.expected),
            "case {:?}: expected the account-wide read to report {:?}, got {found}",
            row.case,
            row.expected
        );
    }

    // A read changes nothing a later caller sees: the second answer is the first.
    let again = describe(&router, json!({}), "status: repeated read").await;
    assert_eq!(
        again, account_wide,
        "case \"repeated read\": expected DescribeEventTopics to report the same associations, \
         got {again:?}"
    );
}

#[tokio::test]
async fn the_status_follows_the_topic_each_time_it_comes_and_goes() {
    let router = ds_and_sns();
    let directory_id =
        create_directory(&router, minimal_create("flip.example.com"), "flip").await;
    let topic_name = "ds-flip";

    // The binding is made ONCE, before the topic exists, and nothing registers it again:
    // every later status is the resolution of that one record against its owner.
    bind(&router, &directory_id, topic_name, "flip: before the topic exists").await;

    // (case, whether the topic is held after this step, the expected status)
    let steps: [(&str, bool, &str); 5] = [
        ("before the topic is created", false, STATUS_TOPIC_NOT_FOUND),
        ("once the topic is created", true, STATUS_REGISTERED),
        ("once the topic is deleted", false, STATUS_TOPIC_NOT_FOUND),
        ("once it is created again", true, STATUS_REGISTERED),
        ("once it is deleted again", false, STATUS_TOPIC_NOT_FOUND),
    ];

    let mut held: Option<String> = None;
    for (case, should_hold, expected) in steps {
        match (should_hold, held.take()) {
            (true, None) => {
                held = Some(create_topic(&router, topic_name, REGION, case).await);
            }
            (false, Some(arn)) => {
                delete_topic(&router, &arn, REGION, case).await;
            }
            (_, carried) => held = carried,
        }
        let status = reported_status(&router, &directory_id, topic_name, case).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected the binding to be reported {expected:?}, got {status:?}"
        );
    }
}
