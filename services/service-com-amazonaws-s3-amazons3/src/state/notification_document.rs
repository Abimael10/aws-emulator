// SPDX-License-Identifier: CC0-1.0

//! The VOCABULARY a notification document names, judged from the document itself.
//!
//! Two of the values a `NotificationConfiguration` carries are judged by Amazon S3 rather
//! than by the schema the request boundary validates against, and each is judged differently
//! from what that schema alone would say:
//!
//! * a key-name filter rule's `Name` is read CASE-INSENSITIVELY — `FilterRuleName`'s values
//!   are the lower-case wire tokens, a stored rule is reported under the capitalised spelling
//!   ([`super::canonical_filter_rule_name`]), and a client that reads a configuration back and
//!   puts it again is sending the capitalised one, which S3 accepts;
//! * a value that is NEITHER, and an `Events` entry outside the published `Event` set, are
//!   refused as the ARGUMENT they are — `InvalidArgument` with the sentence S3 publishes for
//!   that class and the `<ArgumentName>` / `<ArgumentValue>` pair naming the field and echoing
//!   the value as sent — rather than as a document that does not validate against the
//!   published schema.
//!
//! The boundary cannot answer either: it refuses any value outside a modelled enum, in a
//! document that carries a code and a sentence and no room for the pair. So a document this
//! module reports a rejection for is withheld from the codec in full
//! (`crate::service::wire`), leaving `PutBucketNotificationConfiguration` the one place such a
//! document's refusal is worded, and a rule name in another case is folded onto the published
//! spelling before the codec reads the document.
//!
//! Read from the DOCUMENT and not from the parsed input because that is the only place these
//! values survive: a request the boundary refused parses to nothing at all, and the stand-in
//! the codec is handed carries no configuration.

use super::*;

/// The element a bucket event is named in, as restXml writes the member
/// (`smithy_query(query="shape",shape="QueueConfiguration")` reports `Events` with
/// `xmlName: Event` and `xmlFlattened`, so each value is its own `<Event>` child).
const EVENT_ELEMENT: &[u8] = b"Event";

/// The element a key-name filter rule's name is carried in: the member's own declared name,
/// which restXml writes it under (`smithy_query(query="shape",shape="FilterRule")` reports no
/// `xmlName` on `Name`).
const FILTER_RULE_NAME_ELEMENT: &[u8] = b"Name";

/// Each destination configuration's element, under the `xmlName` the model puts on
/// `NotificationConfiguration`'s members (`smithy_query(query="shape",
/// shape="NotificationConfiguration")` reports `TopicConfiguration`, `QueueConfiguration` and
/// `CloudFunctionConfiguration`), beside the destination it configures.
///
/// What it is for: a `<Name>` is a filter rule's name only INSIDE one of these, so a document
/// that spells that element anywhere else is not naming a filter rule at all.
const CONFIGURATION_ELEMENTS: &[(&[u8], NotificationDestination)] = &[
    (b"TopicConfiguration", NotificationDestination::Topic),
    (b"QueueConfiguration", NotificationDestination::Queue),
    (
        b"CloudFunctionConfiguration",
        NotificationDestination::LambdaFunction,
    ),
];

/// Whether `element` opens one destination configuration, and which.
fn configured_destination(element: &[u8]) -> Option<NotificationDestination> {
    CONFIGURATION_ELEMENTS
        .iter()
        .find(|(name, _)| *name == element)
        .map(|(_, destination)| *destination)
}

/// What S3 refuses the notification document `document` for, read from the DOCUMENT rather
/// than from the configuration parsed out of it, or `None` for a document whose vocabulary it
/// refuses nothing about.
///
/// The FIRST offending value in document order is what the refusal is about, the way every
/// other refusal of this service names one: a document breaking the rule twice is refused for
/// the value a reader meets first.
///
/// The text of an element is compared with its surrounding whitespace trimmed, which is how
/// the codec reads it, so this and the parsed input cannot disagree about which values are
/// published ones.
#[must_use]
pub fn notification_document_rejection(
    document: &[u8],
) -> Option<NotificationConfigurationRejection> {
    let mut reader = ::quick_xml::Reader::from_reader(document);
    // Which element's text is being read, whether the reader is inside a destination
    // configuration (a `<Name>` outside one names no filter rule), and the text that element
    // has carried so far — an element that opens and closes with nothing between carries the
    // empty string, which is no published value either.
    let mut inside: Option<&[u8]> = None;
    let mut configured = false;
    let mut spelled = String::new();
    loop {
        match reader.read_event() {
            Ok(::quick_xml::events::Event::Start(start)) => {
                let element = start.name().as_ref().to_vec();
                if configured_destination(&element).is_some() {
                    configured = true;
                }
                inside = match element.as_slice() {
                    EVENT_ELEMENT => Some(EVENT_ELEMENT),
                    FILTER_RULE_NAME_ELEMENT if configured => Some(FILTER_RULE_NAME_ELEMENT),
                    _ => None,
                };
                spelled.clear();
            }
            // A self-closing element carries no text at all, so it names the empty value.
            Ok(::quick_xml::events::Event::Empty(empty)) => {
                let element = empty.name().as_ref().to_vec();
                let judged = match element.as_slice() {
                    EVENT_ELEMENT => Some(EVENT_ELEMENT),
                    FILTER_RULE_NAME_ELEMENT if configured => Some(FILTER_RULE_NAME_ELEMENT),
                    _ => None,
                };
                if let Some(element) = judged {
                    if let Some(rejection) = unpublished_value(element, "") {
                        return Some(rejection);
                    }
                }
                inside = None;
            }
            Ok(::quick_xml::events::Event::Text(text)) if inside.is_some() => {
                spelled.push_str(&text.xml10_content().unwrap_or_default());
            }
            Ok(::quick_xml::events::Event::End(end)) => {
                if let Some(element) = inside.take() {
                    if let Some(rejection) = unpublished_value(element, spelled.trim()) {
                        return Some(rejection);
                    }
                }
                if configured_destination(end.name().as_ref()).is_some() {
                    configured = false;
                }
                spelled.clear();
            }
            Ok(::quick_xml::events::Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// The rejection `spelled` earns as the text of `element`, or `None` when it is a value that
/// element publishes.
///
/// Each set is the model-generated one the validating walk judges the member by
/// ([`EVENT_VALUES`], [`FILTER_RULE_NAME_VALUES`] through
/// [`published_filter_rule_name`]), so this and the store's own validator cannot drift: an
/// `Event` is one of the published values exactly, and a filter rule name is one of them in
/// any case — the capitalised spelling a reader of a stored configuration gets back included.
fn unpublished_value(
    element: &[u8],
    spelled: &str,
) -> Option<NotificationConfigurationRejection> {
    match element {
        EVENT_ELEMENT => (!EVENT_VALUES.contains(&spelled)).then(|| {
            NotificationConfigurationRejection::UnsupportedEvent {
                event: spelled.to_string(),
            }
        }),
        FILTER_RULE_NAME_ELEMENT => published_filter_rule_name(spelled).is_none().then(|| {
            NotificationConfigurationRejection::InvalidFilterRuleName {
                name: spelled.to_string(),
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! The document walk, over documents that differ along what the rule generalises over:
    //! which element carries the value, which destination configuration it sits in, how the
    //! element is spelled out (text, empty element, self-closing), and whether the value is
    //! published, published in another case, or published nowhere.

    use super::*;

    /// One `<Name>` element, in a `QueueConfiguration`'s filter, carrying `name`.
    fn filtered(name: &str) -> String {
        format!(
            "<NotificationConfiguration><QueueConfiguration><Queue>arn:aws:sqs:us-east-1:\
             123456789012:q</Queue><Event>s3:ObjectCreated:*</Event><Filter><S3Key>\
             <FilterRule><Name>{name}</Name><Value>logs/</Value></FilterRule></S3Key>\
             </Filter></QueueConfiguration></NotificationConfiguration>"
        )
    }

    #[test]
    fn the_documents_vocabulary_is_judged_case_insensitively_for_a_filter_rule_name() {
        // (case, document, the rejection it earns — `None` for a document whose vocabulary
        // is published).
        let rows: Vec<(&str, String, Option<NotificationConfigurationRejection>)> = vec![
            (
                "a filter rule name as the enum spells it",
                filtered(FILTER_RULE_NAME_VALUES[0]),
                None,
            ),
            (
                "a filter rule name as a stored configuration reports it",
                filtered("Prefix"),
                None,
            ),
            (
                "a filter rule name in upper case",
                filtered("SUFFIX"),
                None,
            ),
            (
                "a filter rule name that is published nowhere",
                filtered("extension"),
                Some(NotificationConfigurationRejection::InvalidFilterRuleName {
                    name: "extension".to_string(),
                }),
            ),
            (
                "a filter rule naming nothing at all",
                filtered(""),
                Some(NotificationConfigurationRejection::InvalidFilterRuleName {
                    name: String::new(),
                }),
            ),
            (
                "an event outside the published catalog",
                "<NotificationConfiguration><TopicConfiguration><Topic>arn:aws:sns:\
                 us-east-1:123456789012:t</Topic><Event>s3:ObjectCreated:Rename</Event>\
                 </TopicConfiguration></NotificationConfiguration>"
                    .to_string(),
                Some(NotificationConfigurationRejection::UnsupportedEvent {
                    event: "s3:ObjectCreated:Rename".to_string(),
                }),
            ),
            (
                "a published event differing only in case",
                "<NotificationConfiguration><CloudFunctionConfiguration><CloudFunction>\
                 arn:aws:lambda:us-east-1:123456789012:function:f</CloudFunction>\
                 <Event>S3:ObjectCreated:Put</Event></CloudFunctionConfiguration>\
                 </NotificationConfiguration>"
                    .to_string(),
                Some(NotificationConfigurationRejection::UnsupportedEvent {
                    event: "S3:ObjectCreated:Put".to_string(),
                }),
            ),
            (
                "an empty event element",
                "<NotificationConfiguration><QueueConfiguration><Queue>arn:aws:sqs:\
                 us-east-1:123456789012:q</Queue><Event/></QueueConfiguration>\
                 </NotificationConfiguration>"
                    .to_string(),
                Some(NotificationConfigurationRejection::UnsupportedEvent {
                    event: String::new(),
                }),
            ),
            (
                "the last published event of the catalog",
                format!(
                    "<NotificationConfiguration><QueueConfiguration><Queue>arn:aws:sqs:\
                     us-east-1:123456789012:q</Queue><Event>{}</Event>\
                     </QueueConfiguration></NotificationConfiguration>",
                    EVENT_VALUES[EVENT_VALUES.len() - 1]
                ),
                None,
            ),
            (
                "the empty container, which turns notifications off",
                "<NotificationConfiguration></NotificationConfiguration>".to_string(),
                None,
            ),
            (
                // A `<Name>` outside a destination configuration names no filter rule:
                // the element is not the filter rule's there.
                "a Name element outside any configuration",
                "<NotificationConfiguration><Name>whatever</Name>\
                 </NotificationConfiguration>"
                    .to_string(),
                None,
            ),
        ];

        for (case, document, expected) in rows {
            let actual = notification_document_rejection(document.as_bytes());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
