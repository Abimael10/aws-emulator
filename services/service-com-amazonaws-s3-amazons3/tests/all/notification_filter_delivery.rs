// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what a notification configuration's KEY-NAME FILTER does to the
//! delivery of an object event: the one rule that makes the negative leg of a filter
//! observable.
//!
//! RULE: a destination configuration carrying a key-name `Filter` is notified of an object
//! change when the object's key passes EVERY rule of that filter — "the Amazon S3 API supports
//! filtering based on the prefix and suffix of the object key name"
//! (`NotificationConfigurationFilter`) — so a key the filter admits produces exactly ONE
//! notification at the destination and a key it does not produces NONE. The negative leg is
//! observable only because the positive one delivers, which is why both are asserted of the
//! SAME configuration and the SAME queue, with the key the filter rejects written FIRST: the
//! admitted key's record arriving is the barrier that proves the delivery path ran at all, and
//! the rejected key's absence is then an absence rather than a silence.
//!
//! The table spans what the rule generalises over: which rules the filter carries (a prefix, a
//! suffix, both, none at all), and — for each — a key that passes every rule beside keys that
//! fail one of them. Every call goes over the wire through a router carrying S3 beside the
//! Amazon SQS the destination lives in, and the deliveries are read with `ReceiveMessage`, so
//! what is pinned is the message a consumer receives.
//!
//! The harness is [`super::object_event_notifications`]'s, which also asserts the `s3:TestEvent`
//! a validated queue destination receives when the configuration is INSTALLED — so each row
//! below reads only what its own object changes delivered.
//!
//! A second table spans the same rule for a filter value carrying PERCENT ESCAPES: such a
//! value is stored and reported back verbatim and url-decoded before it is compared with a
//! key, so the rule admits the keys its escapes stand for and not the keys whose own text
//! carries `%xx`.

use serde_json::Value;

use super::object_event_notifications::{queue_arn, Destination, Emulator};

/// The event class every row subscribes to: the wildcard form, which "stands for every event
/// of that type", so the row's filter is the only thing that can exclude a write.
const CREATED: &str = "s3:ObjectCreated:*";

/// The `eventName` a `PutObject` of an admitted key must deliver.
const CREATED_BY_PUT: &str = "ObjectCreated:Put";

/// One row of the filter table: the rules the configuration's `Filter` carries, the key it
/// admits, and the keys it does not.
struct Row {
    case: &'static str,
    /// `(Name, Value)` per `FilterRule`, as the request spells them (the `FilterRuleName`
    /// vocabulary is lower case).
    filter_rules: &'static [(&'static str, &'static str)],
    /// A key every rule of the filter admits.
    admitted: &'static str,
    /// Keys at least one rule rejects, each of which must deliver nothing.
    rejected: &'static [&'static str],
}

/// The table. The two rule kinds are the two values `FilterRuleName` publishes
/// (`smithy_query(query="shape",shape="FilterRuleName")` reports `[prefix, suffix]`), and the
/// rows carry each alone, both together, and neither.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a prefix rule",
            filter_rules: &[("prefix", "images/")],
            admitted: "images/photo.jpg",
            rejected: &["logs/photo.jpg", "photo.jpg"],
        },
        Row {
            case: "a suffix rule",
            filter_rules: &[("suffix", ".jpg")],
            admitted: "images/photo.jpg",
            rejected: &["images/photo.txt", "notes.md"],
        },
        Row {
            case: "a prefix and a suffix, both of which must hold",
            filter_rules: &[("prefix", "images/"), ("suffix", ".jpg")],
            admitted: "images/holiday/photo.jpg",
            rejected: &["images/photo.txt", "logs/photo.jpg"],
        },
        Row {
            case: "no filter at all, which restricts nothing",
            filter_rules: &[],
            admitted: "anything/at/all.bin",
            rejected: &[],
        },
    ]
}

/// The `Records` one delivered message carries, with the key and the event name of each.
fn records_of(message: &Value) -> Vec<(String, String)> {
    message["Records"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|record| {
            (
                record["s3"]["object"]["key"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                record["eventName"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_configurations_filter_decides_which_writes_reach_its_queue() {
    for (index, row) in rows().into_iter().enumerate() {
        let emulator = Emulator::new();
        let bucket = format!("notification-filter-bucket-{index}");
        let queue = format!("notification-filter-queue-{index}");
        emulator.create_bucket(&bucket).await;
        let queue_url = emulator.create_queue(&queue).await;
        let arn = queue_arn(&queue);
        // The put is validated against the queue's own policy and takes the `s3:TestEvent`
        // that validation publishes, so what is read below is this row's own deliveries.
        emulator
            .put_notification(
                &bucket,
                &Destination::queue(&arn),
                &[CREATED],
                row.filter_rules,
                false,
            )
            .await;

        // The keys the filter REJECTS are written first, so the admitted key's record is the
        // barrier that proves the delivery path ran: nothing of these may be on the queue
        // once it arrives.
        for key in row.rejected {
            let written = emulator.put_object(&bucket, key, b"rejected").await;
            assert_eq!(
                written.status,
                200,
                "case {:?}: writing {key:?} must succeed, got {} with {}",
                row.case,
                written.status,
                written.text()
            );
        }
        let written = emulator.put_object(&bucket, row.admitted, b"admitted").await;
        assert_eq!(
            written.status,
            200,
            "case {:?}: writing {:?} must succeed, got {} with {}",
            row.case,
            row.admitted,
            written.status,
            written.text()
        );

        let delivered = emulator
            .delivered(&queue_url)
            .await
            .unwrap_or_else(|| panic!("case {:?}: the admitted key must be delivered", row.case));
        let records = records_of(&delivered);
        assert_eq!(
            records,
            vec![(row.admitted.to_string(), CREATED_BY_PUT.to_string())],
            "case {:?}: expected one record naming {:?}, got {records:?} in {delivered}",
            row.case,
            row.admitted
        );

        // And nothing else: a key the filter rejects delivers no record of its own, early or
        // late, so the queue is empty once the admitted key's record has been taken.
        let late = emulator.nothing_delivered(&queue_url).await;
        assert!(
            late.is_empty(),
            "case {:?}: expected the rejected keys {:?} to deliver nothing, got {late:?}",
            row.case,
            row.rejected
        );
    }
}

/// One row of the DECODING table: the rules the configuration's `Filter` carries as the
/// request spells them, the key the decoded rules admit (with the form a delivered record
/// spells it in), and the keys they do not.
struct DecodingRow {
    case: &'static str,
    /// `(Name, Value)` per `FilterRule`, carrying the value verbatim — percent escapes and
    /// all — which is also what a read of the configuration has to report back.
    filter_rules: &'static [(&'static str, &'static str)],
    /// A key every rule admits once its value is url-decoded.
    admitted: &'static str,
    /// That same key as a delivered record spells it: "URL encoded" (Amazon S3 User Guide,
    /// *Event message structure*), with a space as a `+`.
    admitted_published: &'static str,
    /// Keys at least one rule rejects, among them the key whose own literal text IS the
    /// rule's value as spelled.
    rejected: &'static [&'static str],
}

/// The decoding table. The rows span which rule kind carries the escape (`prefix`, `suffix`,
/// both), which character the escape stands for (`=`, a space, `+`, a non-ASCII character),
/// the case of its hex digits, a `%` that opens no escape at all, and values with no escape
/// in them.
fn decoding_rows() -> Vec<DecodingRow> {
    vec![
        DecodingRow {
            case: "a prefix whose escape stands for an equals sign",
            filter_rules: &[("prefix", "folder%3Dtag/")],
            admitted: "folder=tag/decoded-hit.txt",
            admitted_published: "folder%3Dtag/decoded-hit.txt",
            rejected: &["folder%3Dtag/literal-encoded-miss.txt", "folder/tag.txt"],
        },
        DecodingRow {
            case: "a prefix whose escape is spelled with lower-case hex digits",
            filter_rules: &[("prefix", "folder%3dtag/")],
            admitted: "folder=tag/lower-case-escape.txt",
            admitted_published: "folder%3Dtag/lower-case-escape.txt",
            rejected: &["folder%3dtag/literal-lower-case.txt"],
        },
        DecodingRow {
            case: "a prefix whose escape stands for a space",
            filter_rules: &[("prefix", "my%20files/")],
            admitted: "my files/report.txt",
            admitted_published: "my+files/report.txt",
            rejected: &["my%20files/report.txt", "myfiles/report.txt"],
        },
        DecodingRow {
            case: "a prefix whose escapes stand for a non-ASCII character",
            filter_rules: &[("prefix", "caf%C3%A9/")],
            admitted: "café/menu.txt",
            admitted_published: "caf%C3%A9/menu.txt",
            rejected: &["caf%C3%A9/menu.txt", "cafe/menu.txt"],
        },
        DecodingRow {
            case: "a suffix whose escape stands for a plus",
            filter_rules: &[("suffix", "%2Bfinal.csv")],
            admitted: "reports/q3+final.csv",
            admitted_published: "reports/q3%2Bfinal.csv",
            rejected: &["reports/q3%2Bfinal.csv", "reports/q3-final.csv"],
        },
        DecodingRow {
            case: "an escaped prefix beside a plain suffix, both of which must hold",
            filter_rules: &[("prefix", "data%3Draw/"), ("suffix", ".csv")],
            admitted: "data=raw/day.csv",
            admitted_published: "data%3Draw/day.csv",
            rejected: &["data=raw/day.json", "data%3Draw/day.csv"],
        },
        DecodingRow {
            // A `%` that opens no escape is no escape: decoding leaves it alone, so the rule
            // admits the key whose own text carries it.
            case: "a prefix carrying a percent that opens no escape",
            filter_rules: &[("prefix", "50%off/")],
            admitted: "50%off/deal.txt",
            admitted_published: "50%25off/deal.txt",
            rejected: &["50off/deal.txt"],
        },
        DecodingRow {
            // The control row: a value with no escape in it is its own decoding, so such a
            // filter admits exactly what it always did.
            case: "rules with no escape in them at all",
            filter_rules: &[("prefix", "plain/"), ("suffix", ".txt")],
            admitted: "plain/note.txt",
            admitted_published: "plain/note.txt",
            rejected: &["other/note.txt", "plain/note.log"],
        },
    ]
}

/// RULE: a key-name `FilterRule` value is stored and reported back exactly as the request
/// spelled it, and is URL-DECODED before it is compared with an object key — so such a rule
/// admits the keys its escapes stand for and not the keys whose own text carries `%xx`.
///
/// The control plane cannot tell the two behaviours apart on its own (the read-back is
/// identical under both), so each row asserts the read-back AND which write reaches the queue.
#[tokio::test]
async fn a_filter_rule_value_is_echoed_verbatim_and_url_decoded_before_matching() {
    for (index, row) in decoding_rows().into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let bucket = format!("notification-filter-decoding-bucket-{index}");
        let queue = format!("notification-filter-decoding-queue-{index}");
        emulator.create_bucket(&bucket).await;
        let queue_url = emulator.create_queue(&queue).await;
        let arn = queue_arn(&queue);
        emulator
            .put_notification(
                &bucket,
                &Destination::queue(&arn),
                &[CREATED],
                row.filter_rules,
                false,
            )
            .await;

        // The configured value comes back character for character: the decoding is about
        // MATCHING and about nothing else.
        let reported = emulator
            .s3_xml("GET", &format!("/{bucket}?notification"), "")
            .await;
        assert_eq!(
            reported.status, 200,
            "case {case:?}: GetBucketNotificationConfiguration must answer 200, got {} with {}",
            reported.status,
            reported.text()
        );
        let document = reported.text();
        for (name, value) in row.filter_rules {
            let element = format!("<Value>{value}</Value>");
            assert!(
                document.contains(&element),
                "case {case:?}: the {name} rule's value must be reported verbatim as \
                 {element:?}, got {document}"
            );
        }

        // The rejected keys first, so the admitted key's record is the barrier that proves
        // the delivery path ran — the same ordering the filter table above uses. Each write
        // is asserted to have succeeded by the harness itself
        // ([`super::object_event_notifications::Emulator::put_object`]), so a key a filter
        // rejects cannot deliver nothing merely because the write of it was refused.
        for key in row.rejected {
            emulator.put_object(&bucket, key, b"rejected").await;
        }
        emulator.put_object(&bucket, row.admitted, b"admitted").await;

        let delivered = emulator.delivered(&queue_url).await.unwrap_or_else(|| {
            panic!(
                "case {case:?}: the key {:?} the decoded rules admit must be delivered",
                row.admitted
            )
        });
        let records = records_of(&delivered);
        assert_eq!(
            records,
            vec![(
                row.admitted_published.to_string(),
                CREATED_BY_PUT.to_string()
            )],
            "case {case:?}: expected one record naming {:?}, got {records:?} in {delivered}",
            row.admitted_published
        );
        let late = emulator.nothing_delivered(&queue_url).await;
        assert!(
            late.is_empty(),
            "case {case:?}: expected the keys {:?} the decoded rules reject to deliver \
             nothing, got {late:?}",
            row.rejected
        );
    }
}
