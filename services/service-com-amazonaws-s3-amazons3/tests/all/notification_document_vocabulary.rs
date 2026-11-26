// SPDX-License-Identifier: CC0-1.0

//! The VOCABULARY a `NotificationConfiguration` document names, and what S3 answers a value
//! outside it: the key-name filter rule `Name`, the `Events` entries, and the destination
//! reference each configuration carries.
//!
//! The rules under test, each stated over the class rather than over one request:
//!
//! * A key-name filter rule's `Name` is read CASE-INSENSITIVELY — `FilterRuleName`'s values
//!   are the lower-case wire tokens `prefix` and `suffix`
//!   (`smithy_query(query="shape",shape="FilterRuleName")`), a stored rule is reported under
//!   the capitalised spelling, and a client that reads a configuration back and puts it again
//!   is sending that one, which S3 accepts. A `Name` that is no `FilterRuleName` in any case
//!   is refused as the ARGUMENT it is: HTTP 400 `InvalidArgument` with the fixed sentence for
//!   that class and the `<ArgumentName>` / `<ArgumentValue>` pair naming the member at fault by
//!   its dotted path, `FilterRule.Name`, and echoing the value as sent — not as a document that
//!   does not validate.
//! * An `Events` entry outside the published `Event` set is refused the same way, naming the
//!   `Event` and echoing the value as sent.
//! * A destination reference is judged against the ARN form its member requires: a value no
//!   ARN can be read out of is refused with the fixed "could not be parsed" sentence, and a
//!   `CloudFunction` that IS an ARN and does not satisfy the pattern that member publishes is
//!   refused by QUOTING that pattern. Both name the member in `<ArgumentName>` and echo the
//!   value.
//! * One configuration's own `Events` list may not overlap itself: its entries all carry that
//!   configuration's filter, so a list naming a type's wildcard beside an event of that type
//!   is refused, while two different events of one type are kept.
//! * A configuration that names no `Id` reads back with the one Amazon S3 assigned, in the
//!   SHAPE S3 mints: the base64 of a UUID's text.
//!
//! Every assertion drives a real HTTP request through the router, so what is pinned is the
//! wire behaviour a client sees — status, code, message and the error document's own members
//! — and the configuration a later read reports.

use data_encoding::BASE64;
use service_com_amazonaws_s3_amazons3::state::{
    CLOUD_FUNCTION_ARN_PATTERN, EVENT_VALUES, FILTER_RULE_NAME_VALUES,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, Answer};

/// The XML namespace a client's S3 payload carries.
///
/// `pub(crate)`, like the harness below it: the round-trip suite
/// ([`super::notification_configuration_round_trip`]) writes its own container — one that also
/// names an `Id` — and puts the same namespace on it rather than spelling a second one.
pub(crate) const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The code every one of these refusals carries, and the status it is rendered with.
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;

/// The fixed `<Message>` of each class under test.
const INVALID_FILTER_RULE_NAME: &str = "filter rule name must be either prefix or suffix";
const UNSUPPORTED_EVENT: &str = "The event is not supported for notifications";
const UNPARSEABLE_DESTINATION: &str = "The ARN could not be parsed";
const OVERLAPPING_EVENTS: &str = "Configurations overlap. Configurations on the same bucket \
                                  cannot share a common event type.";

/// The `<ArgumentName>` each class names: the field the caller has to change — for a refusal
/// about one filter rule's `Name`, that MEMBER by its dotted path rather than the
/// `FilterRule` element carrying it.
const FILTER_RULE_NAME_ARGUMENT: &str = "FilterRule.Name";
const EVENT_ARGUMENT: &str = "Event";

/// A destination of each kind, in this emulator's account. Validation of the destinations
/// themselves is pinned by [`super::notification_destination_validation`]; this suite is
/// about the FORM of the reference, so the puts it expects to succeed carry
/// `x-amz-skip-destination-validation`.
const TOPIC_ARN: &str = "arn:aws:sns:us-east-1:123456789012:object-events";
const QUEUE_ARN: &str = "arn:aws:sqs:us-east-1:123456789012:object-events";
const FUNCTION_ARN: &str = "arn:aws:lambda:us-east-1:123456789012:function:object-events";

/// Which destination a configuration publishes to: the element it is carried in, and the
/// element its reference is carried in — the `xmlName`s the model puts on
/// `NotificationConfiguration`'s members and on each configuration's ARN member
/// (`smithy_query(query="shape",shape="NotificationConfiguration")` and
/// `smithy_query(query="shape",shape="LambdaFunctionConfiguration")`).
///
/// `pub(crate)`, with the three readers below it: the round-trip suite
/// ([`super::notification_configuration_round_trip`]) drives the same three configuration
/// elements through this one naming of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Destination {
    Topic,
    Queue,
    Function,
}

impl Destination {
    pub(crate) fn element(self) -> &'static str {
        match self {
            Self::Topic => "TopicConfiguration",
            Self::Queue => "QueueConfiguration",
            Self::Function => "CloudFunctionConfiguration",
        }
    }

    /// The element the reference is carried in, which is also the field a refusal about it
    /// names.
    pub(crate) fn member(self) -> &'static str {
        match self {
            Self::Topic => "Topic",
            Self::Queue => "Queue",
            Self::Function => "CloudFunction",
        }
    }

    /// A destination of this kind in this emulator's account — the reference a configuration
    /// whose subject is the DOCUMENT rather than the destination carries.
    pub(crate) fn reference(self) -> &'static str {
        match self {
            Self::Topic => TOPIC_ARN,
            Self::Queue => QUEUE_ARN,
            Self::Function => FUNCTION_ARN,
        }
    }
}

/// The document a request carries: one configuration of `destination` naming `reference`,
/// notified about `events`, with `rules` as its key-name filter (no `Filter` at all when the
/// list is empty).
fn payload(
    destination: Destination,
    reference: &str,
    events: &[&str],
    rules: &[(&str, &str)],
) -> String {
    let mut body = format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\"><{element}><{member}>{reference}\
         </{member}>",
        element = destination.element(),
        member = destination.member()
    );
    for event in events {
        body.push_str(&format!("<Event>{event}</Event>"));
    }
    if !rules.is_empty() {
        body.push_str("<Filter><S3Key>");
        for (name, value) in rules {
            body.push_str(&format!(
                "<FilterRule><Name>{name}</Name><Value>{value}</Value></FilterRule>"
            ));
        }
        body.push_str("</S3Key></Filter>");
    }
    body.push_str(&format!(
        "</{element}></NotificationConfiguration>",
        element = destination.element()
    ));
    body
}

/// `PUT /<bucket>?notification` carrying `body`, with destination validation skipped so the
/// answer is about the DOCUMENT rather than about whether the destination exists.
///
/// `pub(crate)`, with the two helpers below it: the round-trip suite
/// ([`super::notification_configuration_round_trip`]) drives the subresource through this one
/// harness instead of standing up a second one.
pub(crate) async fn put(router: &MultiServiceRouter, bucket: &str, body: &str) -> Answer {
    send(
        router,
        "PUT",
        &format!("/{bucket}?notification"),
        &[("x-amz-skip-destination-validation", "true")],
        body,
    )
    .await
}

/// `GET /<bucket>?notification`.
pub(crate) async fn read(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send(router, "GET", &format!("/{bucket}?notification"), &[], "").await
}

/// Create `bucket` and answer the router it was created on.
pub(crate) async fn bucket_on(router: &MultiServiceRouter, bucket: &str) {
    let created = create_bucket(router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "case {bucket:?}: the bucket must be created first, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
}

/// Assert that `answer` is the refusal `message` names, carrying the `<ArgumentName>` /
/// `<ArgumentValue>` pair `argument` and `value`.
fn assert_refused(case: &str, answer: &Answer, message: &str, argument: &str, value: &str) {
    assert_eq!(
        answer.status, INVALID_ARGUMENT_STATUS,
        "case {case:?}: expected HTTP {INVALID_ARGUMENT_STATUS}, got {} with body {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.error_code(),
        INVALID_ARGUMENT,
        "case {case:?}: expected the code {INVALID_ARGUMENT}, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        answer.error_message(),
        message,
        "case {case:?}: expected the message {message:?}, got {:?}",
        answer.error_message()
    );
    assert_eq!(
        answer.element("ArgumentName").as_deref(),
        Some(argument),
        "case {case:?}: expected the refusal to name the argument {argument:?}, got body {:?}",
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.element("ArgumentValue").as_deref(),
        Some(value),
        "case {case:?}: expected the refusal to echo the value {value:?}, got body {:?}",
        String::from_utf8_lossy(&answer.body)
    );
}

#[tokio::test]
async fn a_filter_rule_name_is_read_in_any_case_and_reported_capitalised() {
    let router = router();

    // Every published name in every case that differs from the published spelling: the
    // lower-case wire token, the capitalised spelling a read reports, upper case and a mixed
    // case no row of the recorded corpus names. Each must be accepted and read back under
    // the capitalised spelling, whatever the request sent.
    let mut index = 0usize;
    for published in FILTER_RULE_NAME_VALUES {
        let capitalised = {
            let mut characters = published.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                None => String::new(),
            }
        };
        let mixed: String = published
            .chars()
            .enumerate()
            .map(|(position, character)| {
                if position % 2 == 0 {
                    character.to_ascii_uppercase()
                } else {
                    character
                }
            })
            .collect();
        for sent in [
            published.to_string(),
            capitalised.clone(),
            published.to_uppercase(),
            mixed,
        ] {
            let case = format!("a {published} rule sent as {sent:?}");
            let bucket = format!("filter-rule-case-{index}");
            index += 1;
            bucket_on(&router, &bucket).await;

            let answer = put(
                &router,
                &bucket,
                &payload(
                    Destination::Queue,
                    QUEUE_ARN,
                    &["s3:ObjectCreated:Put"],
                    &[(&sent, "incoming/")],
                ),
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a published filter rule name must be accepted in any case, \
                 got {} with body {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );

            let reported = read(&router, &bucket).await;
            assert_eq!(
                reported.element("Name").as_deref(),
                Some(capitalised.as_str()),
                "case {case:?}: expected the stored rule to report {capitalised:?}, got body \
                 {:?}",
                String::from_utf8_lossy(&reported.body)
            );
            assert_eq!(
                reported.element("Value").as_deref(),
                Some("incoming/"),
                "case {case:?}: the rule's value must survive the re-casing, got body {:?}",
                String::from_utf8_lossy(&reported.body)
            );
        }
    }
}

#[tokio::test]
async fn a_filter_rule_name_outside_the_published_set_is_refused_as_an_argument() {
    let router = router();

    // Names that are no `FilterRuleName` in ANY case: one past each published value, a name
    // of another attribute, a name with inner whitespace, and the empty name. (A name with
    // surrounding whitespace is not one of these: the codec reads an element's text trimmed,
    // so `prefix ` names the published rule.)
    let rows = ["Prefixes", "suffixes", "extension", "pre fix", ""];

    for (index, sent) in rows.into_iter().enumerate() {
        let case = format!("a filter rule named {sent:?}");
        let bucket = format!("filter-rule-unpublished-{index}");
        bucket_on(&router, &bucket).await;

        let answer = put(
            &router,
            &bucket,
            &payload(
                Destination::Queue,
                QUEUE_ARN,
                &["s3:ObjectCreated:Put"],
                &[(sent, "incoming/")],
            ),
        )
        .await;
        assert_refused(
            &case,
            &answer,
            INVALID_FILTER_RULE_NAME,
            FILTER_RULE_NAME_ARGUMENT,
            sent.trim(),
        );

        // Nothing was stored: the bucket still reports the empty configuration.
        let reported = read(&router, &bucket).await;
        assert_eq!(
            reported.element("FilterRule"),
            None,
            "case {case:?}: a refused document must store no rule, got body {:?}",
            String::from_utf8_lossy(&reported.body)
        );
    }
}

#[tokio::test]
async fn an_event_outside_the_published_catalog_is_refused_as_an_argument() {
    let router = router();

    // (case, the events the document names, the value the refusal must echo). The rows
    // differ along WHERE the unpublished value sits and HOW it differs from a published one:
    // a detail the catalog does not carry, a published value in another case, a type that is
    // not one, an unpublished value beside published ones, and the empty value.
    let rows: Vec<(&str, Vec<&str>, &str)> = vec![
        (
            "a detail outside the catalog",
            vec!["s3:ObjectCreated:Rename"],
            "s3:ObjectCreated:Rename",
        ),
        (
            "a published value in another case",
            vec!["S3:ObjectCreated:Put"],
            "S3:ObjectCreated:Put",
        ),
        (
            "a type that is not published",
            vec!["s3:ObjectVanished:*"],
            "s3:ObjectVanished:*",
        ),
        (
            "an unpublished value after published ones",
            vec!["s3:ObjectCreated:Put", "s3:ObjectVanished"],
            "s3:ObjectVanished",
        ),
        ("no event text at all", vec![""], ""),
    ];

    for (index, (case, events, echoed)) in rows.into_iter().enumerate() {
        let bucket = format!("event-unpublished-{index}");
        bucket_on(&router, &bucket).await;

        let answer = put(
            &router,
            &bucket,
            &payload(Destination::Topic, TOPIC_ARN, &events, &[]),
        )
        .await;
        assert_refused(case, &answer, UNSUPPORTED_EVENT, EVENT_ARGUMENT, echoed);
    }

    // And EVERY published value is accepted, so the refusals above are about the value
    // being outside the catalog rather than about the member.
    for (index, published) in EVENT_VALUES.iter().enumerate() {
        let bucket = format!("event-published-{index}");
        bucket_on(&router, &bucket).await;
        let answer = put(
            &router,
            &bucket,
            &payload(Destination::Topic, TOPIC_ARN, &[published], &[]),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {published:?}: a published event must be accepted, got {} with body {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
    }
}

#[tokio::test]
async fn a_destination_reference_is_judged_against_the_form_its_member_requires() {
    let router = router();

    // (case, the destination, the reference the document carries, the message the refusal
    // must carry — `None` for a reference the member admits). The rows differ along WHICH
    // member carries the reference, and HOW the value fails: not an ARN at all, an ARN of
    // another service, an ARN with a field missing, and — for the one member that publishes a
    // pattern — an ARN that parses and does not satisfy it, at each published bound.
    let quoted_pattern = format!(
        "The ARN failed to satisfy constraint: Member must satisfy regular expression \
         pattern: {CLOUD_FUNCTION_ARN_PATTERN}"
    );
    let at_the_bound = format!(
        "arn:aws:lambda:us-east-1:123456789012:function:{}",
        "f".repeat(64)
    );
    let past_the_bound = format!(
        "arn:aws:lambda:us-east-1:123456789012:function:{}",
        "f".repeat(65)
    );
    let rows: Vec<(&str, Destination, &str, Option<&str>)> = vec![
        (
            "a topic ARN in the topic member",
            Destination::Topic,
            TOPIC_ARN,
            None,
        ),
        (
            "a bare topic name",
            Destination::Topic,
            "object-events",
            Some(UNPARSEABLE_DESTINATION),
        ),
        (
            "a queue ARN in the topic member",
            Destination::Topic,
            QUEUE_ARN,
            Some(UNPARSEABLE_DESTINATION),
        ),
        (
            "a queue ARN in the queue member",
            Destination::Queue,
            QUEUE_ARN,
            None,
        ),
        (
            "a queue ARN with no resource",
            Destination::Queue,
            "arn:aws:sqs:us-east-1:123456789012:",
            Some(UNPARSEABLE_DESTINATION),
        ),
        (
            "a queue reference with too few sections",
            Destination::Queue,
            "arn:aws:sqs:object-events",
            Some(UNPARSEABLE_DESTINATION),
        ),
        (
            "a function ARN in the function member",
            Destination::Function,
            FUNCTION_ARN,
            None,
        ),
        (
            "a function ARN naming a version",
            Destination::Function,
            "arn:aws:lambda:us-east-1:123456789012:function:object-events:7",
            None,
        ),
        (
            "a function ARN naming an alias",
            Destination::Function,
            "arn:aws:lambda:us-east-1:123456789012:function:object-events:live",
            None,
        ),
        (
            "a function name of the longest the pattern admits",
            Destination::Function,
            &at_the_bound,
            None,
        ),
        (
            "a function name one character past that bound",
            Destination::Function,
            &past_the_bound,
            Some(&quoted_pattern),
        ),
        (
            "a bare function name",
            Destination::Function,
            "object-events",
            Some(UNPARSEABLE_DESTINATION),
        ),
        (
            "a function ARN whose account is not twelve digits",
            Destination::Function,
            "arn:aws:lambda:us-east-1:12345:function:object-events",
            Some(&quoted_pattern),
        ),
        (
            "a function ARN that names no function",
            Destination::Function,
            "arn:aws:lambda:us-east-1:123456789012:object-events",
            Some(&quoted_pattern),
        ),
        (
            "a function ARN carrying the $LATEST qualifier",
            Destination::Function,
            "arn:aws:lambda:us-east-1:123456789012:function:object-events:$LATEST",
            Some(&quoted_pattern),
        ),
    ];

    for (index, (case, destination, reference, refused)) in rows.into_iter().enumerate() {
        let bucket = format!("destination-form-{index}");
        bucket_on(&router, &bucket).await;

        let answer = put(
            &router,
            &bucket,
            &payload(
                destination,
                reference,
                &["s3:ObjectCreated:Put"],
                &[("prefix", "incoming/")],
            ),
        )
        .await;
        match refused {
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: the reference must be admitted, got {} with body {:?}",
                    answer.status,
                    String::from_utf8_lossy(&answer.body)
                );
                let reported = read(&router, &bucket).await;
                assert_eq!(
                    reported.element(destination.member()).as_deref(),
                    Some(reference),
                    "case {case:?}: the stored reference must read back verbatim, got body \
                     {:?}",
                    String::from_utf8_lossy(&reported.body)
                );
            }
            Some(message) => assert_refused(
                case,
                &answer,
                message,
                destination.member(),
                reference,
            ),
        }
    }
}

#[tokio::test]
async fn one_configurations_own_event_list_may_not_overlap_itself() {
    let router = router();

    // (case, the events one configuration names, the pair the refusal must echo — `None` for
    // a list whose entries are notified of no event in common). Two entries of one list carry
    // that configuration's filter, so nothing tells them apart.
    let rows: Vec<(&str, Vec<&str>, Option<&str>)> = vec![
        (
            "a type's wildcard beside an event of that type",
            vec!["s3:ObjectCreated:*", "s3:ObjectCreated:Put"],
            Some("s3:ObjectCreated:*, s3:ObjectCreated:Put"),
        ),
        (
            "an event of a type beside that type's wildcard",
            vec!["s3:ObjectTagging:Delete", "s3:ObjectTagging:*"],
            Some("s3:ObjectTagging:Delete, s3:ObjectTagging:*"),
        ),
        (
            "a wildcard among events of other types",
            vec![
                "s3:ObjectAcl:Put",
                "s3:ObjectRemoved:*",
                "s3:ObjectRemoved:Delete",
            ],
            Some("s3:ObjectRemoved:*, s3:ObjectRemoved:Delete"),
        ),
        (
            "two different details of one type",
            vec!["s3:ObjectRemoved:Delete", "s3:ObjectRemoved:DeleteMarkerCreated"],
            None,
        ),
        (
            "one event named twice",
            vec!["s3:ObjectCreated:Put", "s3:ObjectCreated:Put"],
            None,
        ),
        (
            "two wildcards of two types",
            vec!["s3:ObjectCreated:*", "s3:ObjectRemoved:*"],
            None,
        ),
        (
            "a value that carries no detail beside a wildcard",
            vec!["s3:ReducedRedundancyLostObject", "s3:ObjectCreated:*"],
            None,
        ),
    ];

    for (index, (case, events, overlapping)) in rows.into_iter().enumerate() {
        let bucket = format!("self-overlap-{index}");
        bucket_on(&router, &bucket).await;

        let answer = put(
            &router,
            &bucket,
            &payload(
                Destination::Function,
                FUNCTION_ARN,
                &events,
                &[("prefix", "incoming/")],
            ),
        )
        .await;
        match overlapping {
            Some(pair) => {
                assert_refused(case, &answer, OVERLAPPING_EVENTS, EVENT_ARGUMENT, pair);
                let reported = read(&router, &bucket).await;
                assert_eq!(
                    reported.element("CloudFunction"),
                    None,
                    "case {case:?}: a refused document must store nothing, got body {:?}",
                    String::from_utf8_lossy(&reported.body)
                );
            }
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: the list must be applied as given, got {} with body {:?}",
                    answer.status,
                    String::from_utf8_lossy(&answer.body)
                );
                let reported = read(&router, &bucket).await;
                for event in &events {
                    assert!(
                        reported.elements("Event").contains(&(*event).to_string()),
                        "case {case:?}: the stored configuration must report {event:?}, got \
                         body {:?}",
                        String::from_utf8_lossy(&reported.body)
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn an_assigned_configuration_id_is_the_base64_of_a_uuid() {
    let router = router();
    let bucket = "assigned-id-shape";
    bucket_on(&router, bucket).await;

    // Three configurations, none naming an `Id`: "if you don't provide one, Amazon S3 will
    // assign an ID" (`NotificationId`), and the id S3 assigns is the base64 of a UUID's text.
    let body = format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\">\
         <TopicConfiguration><Topic>{TOPIC_ARN}</Topic><Event>s3:ObjectCreated:Put</Event>\
         </TopicConfiguration>\
         <QueueConfiguration><Queue>{QUEUE_ARN}</Queue><Event>s3:ObjectRemoved:Delete</Event>\
         </QueueConfiguration>\
         <CloudFunctionConfiguration><CloudFunction>{FUNCTION_ARN}</CloudFunction>\
         <Event>s3:ObjectRestore:Post</Event></CloudFunctionConfiguration>\
         </NotificationConfiguration>"
    );
    let answer = put(&router, bucket, &body).await;
    assert_eq!(
        answer.status, 200,
        "case \"three configurations with no id\": the put must succeed, got {} with body {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );

    let reported = read(&router, bucket).await;
    let assigned = reported.elements("Id");
    assert_eq!(
        assigned.len(),
        3,
        "case \"three configurations with no id\": each must read back with an assigned id, \
         got {assigned:?}"
    );
    for id in &assigned {
        let decoded = BASE64.decode(id.as_bytes()).unwrap_or_else(|error| {
            panic!("case {id:?}: an assigned id must be base64, got {error}")
        });
        let text = String::from_utf8(decoded)
            .unwrap_or_else(|error| panic!("case {id:?}: it must decode to text, got {error}"));
        let groups: Vec<usize> = text.split('-').map(str::len).collect();
        assert_eq!(
            groups,
            vec![8, 4, 4, 4, 12],
            "case {id:?}: it must decode to a hyphenated UUID, got {text:?}"
        );
        assert!(
            text.chars().all(|character| {
                character == '-' || character.is_ascii_hexdigit() && !character.is_uppercase()
            }),
            "case {id:?}: a UUID's text is lower-case hexadecimal, got {text:?}"
        );
    }
    let mut unique = assigned.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        assigned.len(),
        "case \"three configurations with no id\": assigned ids must be unique, got {assigned:?}"
    );

    // Stable across reads: a read reports, it does not assign.
    let again = read(&router, bucket).await;
    assert_eq!(
        again.elements("Id"),
        assigned,
        "case \"three configurations with no id\": two reads must report the same ids, got {:?}",
        again.elements("Id")
    );
}
