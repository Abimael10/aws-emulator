// SPDX-License-Identifier: CC0-1.0

//! The ROUND TRIP of a bucket's notification configuration: the document
//! `GetBucketNotificationConfiguration` reports is itself a document
//! `PutBucketNotificationConfiguration` accepts, and re-applying it converges.
//!
//! The rule under test, stated over the class rather than over one request: a key-name filter
//! rule's `Name` is read CASE-INSENSITIVELY over the vocabulary `FilterRuleName` publishes
//! (`smithy_query(query="shape",shape="FilterRuleName")` reports `[prefix, suffix]` — the
//! REQUEST vocabulary) and is stored under the canonical capitalised spelling a read reports,
//! while a rule's `Value` is stored and reported exactly as sent; a put replaces the whole
//! subresource, so a configuration that names an `Id` keeps it and one that names none is
//! assigned a fresh one. Together those make the subresource a FIXPOINT: feeding a read's own
//! bytes back into the put converges on the same stored configuration, which is what every
//! tool that refreshes state and re-applies it does.
//!
//! What this suite adds to its siblings, which own the single-request halves of the same rule:
//! the FIXPOINT itself — a read's bytes put back verbatim, and the identical document with the
//! assigned ids dropped — rather than another table of rule-name spellings. The case folding
//! and the canonical projection are pinned by `notification_document_vocabulary`'s
//! `a_filter_rule_name_is_read_in_any_case_and_reported_capitalised`, and a `Name` outside the
//! published vocabulary — refused `InvalidArgument` with that class's fixed sentence, leaving
//! the configuration the bucket already carried untouched — by that same suite's
//! `a_filter_rule_name_outside_the_published_set_is_refused_as_an_argument` and by
//! `bucket_notification_configuration`'s `a_refused_notification_configuration_changes_nothing`.

use quick_xml::escape::escape;
use service_com_amazonaws_s3_amazons3::state::{
    canonical_filter_rule_name, FILTER_RULE_NAME_VALUES,
};

use super::bucket_lifecycle::{router, Answer};
use super::notification_document_vocabulary::{bucket_on, put, read, Destination, XMLNS};

/// A `NotificationConfiguration` document naming one configuration of `destination`, notified
/// about `events`, filtering on `rules` (no `Filter` at all when the list is empty), carrying
/// `id` when one is named.
///
/// Its own builder rather than the sibling suite's: this one names an `Id` — which is what a
/// read reports and what tells a kept id from a freshly minted one — and escapes the rule text
/// the way a client's XML writer does, so a value carrying XML syntax can be sent at all.
fn document(
    destination: Destination,
    id: Option<&str>,
    events: &[&str],
    rules: &[(&str, &str)],
) -> String {
    let (element, member) = (destination.element(), destination.member());
    let mut body = format!("<NotificationConfiguration xmlns=\"{XMLNS}\"><{element}>");
    if let Some(id) = id {
        body.push_str(&format!("<Id>{}</Id>", escape(id)));
    }
    body.push_str(&format!(
        "<{member}>{}</{member}>",
        destination.reference()
    ));
    for event in events {
        body.push_str(&format!("<Event>{event}</Event>"));
    }
    if !rules.is_empty() {
        body.push_str("<Filter><S3Key>");
        for (name, value) in rules {
            body.push_str(&format!(
                "<FilterRule><Name>{}</Name><Value>{}</Value></FilterRule>",
                escape(*name),
                escape(*value)
            ));
        }
        body.push_str("</S3Key></Filter>");
    }
    body.push_str(&format!("</{element}></NotificationConfiguration>"));
    body
}

/// The body of an answer as text.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).into_owned()
}

/// `document` with every `<Id>` element removed — the document a client that drops the ids S3
/// assigned re-applies, which is what makes the put mint fresh ones.
fn without_ids(document: &str) -> String {
    let mut remaining = document;
    let mut stripped = String::with_capacity(document.len());
    while let Some((before, rest)) = remaining.split_once("<Id>") {
        stripped.push_str(before);
        remaining = match rest.split_once("</Id>") {
            Some((_, after)) => after,
            None => {
                stripped.push_str("<Id>");
                rest
            }
        };
    }
    stripped.push_str(remaining);
    stripped
}

/// The canonical capitalised spelling of a published filter rule name, through the one reading
/// the service itself uses.
fn canonical(published: &str) -> &'static str {
    canonical_filter_rule_name(published)
        .unwrap_or_else(|| panic!("case {published:?}: a published name must canonicalise"))
}

/// One row: the case, and the filter rules the first put sends, as `(name, value)`.
type FilterRow = (String, Vec<(String, String)>);

/// The table: the filters a configuration may carry, differing along what the rule
/// generalises over — which published rule kinds are named, in which ORDER, how many, and what
/// the values look like (a plain prefix, the empty value, and a value that carries XML
/// syntax).
///
/// The kinds come from [`FILTER_RULE_NAME_VALUES`] rather than being spelled here, so a
/// regenerated model cannot leave the table behind: each published kind is driven on its own,
/// and all of them together in both orders.
fn filter_rows() -> Vec<FilterRow> {
    let mut rows: Vec<FilterRow> = vec![("no filter at all".to_string(), Vec::new())];
    for published in FILTER_RULE_NAME_VALUES {
        rows.push((
            format!("one {published} rule"),
            vec![((*published).to_string(), "raw/2026".to_string())],
        ));
        rows.push((
            format!("one {published} rule filtering on the empty value"),
            vec![((*published).to_string(), String::new())],
        ));
        rows.push((
            format!("one {published} rule whose value carries XML syntax"),
            vec![((*published).to_string(), "a&b <c>/".to_string())],
        ));
    }
    let every: Vec<(String, String)> = FILTER_RULE_NAME_VALUES
        .iter()
        .enumerate()
        .map(|(position, published)| ((*published).to_string(), format!("value-{position}/")))
        .collect();
    rows.push(("every published rule kind".to_string(), every.clone()));
    let mut reversed = every;
    reversed.reverse();
    rows.push((
        "every published rule kind in the other order".to_string(),
        reversed,
    ));
    rows
}

#[tokio::test]
async fn the_document_a_read_reports_is_accepted_verbatim_by_a_put() {
    let router = router();

    // Every destination kind, because the rule is about the FILTER a configuration carries and
    // all three carry the same one.
    let mut index = 0usize;
    for destination in [Destination::Topic, Destination::Queue, Destination::Function] {
        for (filter, rules) in filter_rows() {
            let case = format!("{filter} on a {}", destination.element());
            let bucket = format!("notification-round-trip-{index}");
            index += 1;
            bucket_on(&router, &bucket).await;

            // The first put spells every rule name the way the enum does — the request
            // vocabulary — and names no `Id`, so S3 assigns one.
            let sent: Vec<(&str, &str)> = rules
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let first = put(
                &router,
                &bucket,
                &document(destination, None, &["s3:ObjectCreated:*"], &sent),
            )
            .await;
            assert_eq!(
                first.status, 200,
                "case {case:?}: the put must be accepted, got {} with body {:?}",
                first.status,
                text(&first)
            );

            // What the read reports: the canonical capitalised name of each rule, the values
            // exactly as sent, and one assigned id.
            let reported = read(&router, &bucket).await;
            let expected_names: Vec<String> = rules
                .iter()
                .map(|(name, _)| canonical(name).to_string())
                .collect();
            let expected_values: Vec<String> =
                rules.iter().map(|(_, value)| value.clone()).collect();
            assert_eq!(
                reported.elements("Name"),
                expected_names,
                "case {case:?}: expected the names {expected_names:?}, got body {:?}",
                text(&reported)
            );
            assert_eq!(
                reported.elements("Value"),
                expected_values,
                "case {case:?}: expected the values {expected_values:?}, got body {:?}",
                text(&reported)
            );
            let assigned = reported.element("Id").unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a configuration naming no Id must read back with one, got \
                     body {:?}",
                    text(&reported)
                )
            });

            // THE ROUND TRIP: the read's own bytes, fed straight back into the put. It is
            // accepted, and the subresource is a fixpoint — the next read reports the very
            // same document, the named `Id` included.
            let document = text(&reported);
            let again = put(&router, &bucket, &document).await;
            assert_eq!(
                again.status, 200,
                "case {case:?}: the document a read reports must be accepted verbatim, got {} \
                 with body {:?}",
                again.status,
                text(&again)
            );
            let after = read(&router, &bucket).await;
            assert_eq!(
                text(&after),
                document,
                "case {case:?}: re-applying a read's own document must converge, got body {:?}",
                text(&after)
            );

            // And the same document with the assigned ids dropped: accepted too, converging on
            // the same filter, with a FRESH id — a put replaces the whole subresource, so a
            // configuration that names no `Id` is assigned one each time.
            let anonymous = without_ids(&document);
            let third = put(&router, &bucket, &anonymous).await;
            assert_eq!(
                third.status, 200,
                "case {case:?}: the read's document without its ids must be accepted, got {} \
                 with body {:?}",
                third.status,
                text(&third)
            );
            let minted = read(&router, &bucket).await;
            assert_eq!(
                minted.elements("Name"),
                expected_names,
                "case {case:?}: the filter must survive the re-application, got body {:?}",
                text(&minted)
            );
            assert_eq!(
                minted.elements("Value"),
                expected_values,
                "case {case:?}: the values must survive the re-application, got body {:?}",
                text(&minted)
            );
            let fresh = minted.element("Id").unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a put naming no Id must mint one, got body {:?}",
                    text(&minted)
                )
            });
            assert_ne!(
                fresh, assigned,
                "case {case:?}: a put naming no Id must mint a NEW id, got {fresh:?} twice"
            );
        }
    }
}
