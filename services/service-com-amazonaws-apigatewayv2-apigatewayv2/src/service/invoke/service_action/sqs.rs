// SPDX-License-Identifier: CC0-1.0

//! The Amazon SQS actions a route performs first class: the message it SENDS to a queue, the
//! messages it READS off one, the message it DELETES and the queue it PURGES.
//!
//! The parameters of each are the members of that operation's own input
//! (`smithy_query(query="operation", operation="SendMessage,ReceiveMessage,DeleteMessage,
//! PurgeQueue")` on the Amazon SQS model), which is what the action takes. Every one of them
//! names the queue it acts on (`QueueUrl`), the send names the message
//! ([`SEND_MESSAGE_REQUIRED_PARAMETERS`]) and the delete names the handle it deletes by
//! ([`DELETE_MESSAGE_REQUIRED_PARAMETERS`]) — the members those inputs mark required — so an
//! integration mapping none of them names no call at all and the request is answered as an
//! integration that produced no response, never as a message nothing enqueued or a queue
//! nothing emptied, while a request that carries no value for the expression the integration
//! maps onto one of them is a bad request, which is the parent module's own rule for every
//! action's required parameters.
//!
//! The queue URL is a parameter of the in-process call and NOT a destination this process
//! connects to: the client is the emulator's own transport, addressed at the Amazon SQS
//! endpoint of the api's Region, so every one of these calls is served by the queue this
//! emulator holds.
//!
//! What the call then does is Amazon SQS's own: whether the calling role may act on that
//! queue, what the queue's own configuration does with a message, which messages a receive
//! takes and what a handle still refers to. Nothing of that is decided here; this module maps
//! the input and makes the call.
//!
//! # The form an answer is reported in
//!
//! The caller of such a route is answered the queue api's own `{Action}Response` envelope —
//! the members of `{Action}Result` beside the `ResponseMetadata` that names the request — as
//! `text/xml` ([`query_envelope`]). An action whose own output declares no member at all
//! (`DeleteMessage`, `PurgeQueue`, both `Unit`) carries no result element: there is no member
//! to state, and that interface writes none, so the envelope is the response element around
//! the `ResponseMetadata` alone.
//!
//! That is the form RECORDED for these subtypes, and relaying it is what the route exists
//! for: a client fronting a queue with an HTTP api reads the id of the message it just sent,
//! or the body, id and receipt handle of the message it just read, off this document, and the
//! behavioral recording this emulator is measured against reads those out of
//! `SendMessageResponse/SendMessageResult/MessageId` and
//! `ReceiveMessageResponse/ReceiveMessageResult/Message` and compares them with what its
//! consumer then takes off the queue — a JSON object of the same members answers it nothing.
//!
//! What decides the form is therefore NOT the protocol an SDK client of Amazon SQS speaks:
//! the model in this tree declares `awsJson1_0` (and its `awsQueryCompatible` covers that
//! service's error CODE surface, not its documents), so the in-process call this module makes
//! with the SDK is itself answered a JSON document. It is which INTERFACE of the queue api
//! API Gateway performs a first-class action over, and for these subtypes the recording shows
//! the query one. Nothing of the envelope is a shape this gateway invented for itself: the
//! element names are the action's own, the members are the ones its output shape declares, in
//! the order it declares them, and every value in it — the digests, the ids, the handles, the
//! bodies, the request named — is what Amazon SQS answered the call with.

use ::base64::Engine as _;
use aws_sdk_sqs::operation::RequestId as _;
use aws_sdk_sqs::types::{Message, MessageAttributeValue};
use aws_smithy_schema_server::xml_escape;
use smithy_server_runtime::OperationContext;

use super::credentials::SigningIdentity;
use super::parameters::Mapped;
use super::{Performed, Unperformed};

/// The `IntegrationSubtype` that names each of these actions, taken from the catalog that
/// states which subtypes are first-class (`crate::state::integration_subtype`), so the value
/// the control plane admits and the one this data plane performs are one constant.
pub const SEND_MESSAGE_SUBTYPE: &str = crate::state::integration_subtype::SQS_SEND_MESSAGE.name;
pub const RECEIVE_MESSAGE_SUBTYPE: &str =
    crate::state::integration_subtype::SQS_RECEIVE_MESSAGE.name;
pub const DELETE_MESSAGE_SUBTYPE: &str = crate::state::integration_subtype::SQS_DELETE_MESSAGE.name;
pub const PURGE_QUEUE_SUBTYPE: &str = crate::state::integration_subtype::SQS_PURGE_QUEUE.name;

/// Every parameter an integration may map onto the send: the members of
/// `SendMessageRequest` this emulator carries into the call.
///
/// The two attribute MAPS the operation also declares (`MessageAttributes`,
/// `MessageSystemAttributes`) are not among them, so an integration mapping one is reported
/// rather than sent without it ([`Mapped::beyond`]).
pub const SEND_MESSAGE_PARAMETERS: &[&str] = &[
    "DelaySeconds",
    "MessageBody",
    "MessageDeduplicationId",
    "MessageGroupId",
    "QueueUrl",
];

/// The parameters the send must carry a value for: the members `SendMessageRequest` marks
/// required, which are the arguments the control plane already holds an integration of this
/// subtype to (`crate::state::integration_subtype`).
pub const SEND_MESSAGE_REQUIRED_PARAMETERS: &[&str] =
    crate::state::integration_subtype::SQS_SEND_MESSAGE.required_parameters;

/// Every parameter an integration may map onto the receive: the SCALAR members of
/// `ReceiveMessageRequest` this emulator carries into the call.
///
/// The three name LISTS the operation also declares (`AttributeNames`,
/// `MessageSystemAttributeNames`, `MessageAttributeNames`) are not among them — a mapping
/// supplies one value per parameter, not a list — so an integration mapping one is reported
/// rather than read without it ([`Mapped::beyond`]), the way the send's attribute maps are.
pub const RECEIVE_MESSAGE_PARAMETERS: &[&str] = &[
    "MaxNumberOfMessages",
    "QueueUrl",
    "ReceiveRequestAttemptId",
    "VisibilityTimeout",
    "WaitTimeSeconds",
];

/// The parameters the receive must carry a value for: the one member
/// `ReceiveMessageRequest` marks required, which is the argument the control plane already
/// holds an integration of this subtype to.
pub const RECEIVE_MESSAGE_REQUIRED_PARAMETERS: &[&str] =
    crate::state::integration_subtype::SQS_RECEIVE_MESSAGE.required_parameters;

/// Every parameter an integration may map onto the delete: the members of
/// `DeleteMessageRequest`, both of which that input marks required.
pub const DELETE_MESSAGE_PARAMETERS: &[&str] = &["QueueUrl", "ReceiptHandle"];

/// The parameters the delete must carry a value for, from the same catalog.
pub const DELETE_MESSAGE_REQUIRED_PARAMETERS: &[&str] =
    crate::state::integration_subtype::SQS_DELETE_MESSAGE.required_parameters;

/// Every parameter an integration may map onto the purge: the one member
/// `PurgeQueueRequest` declares, which it marks required.
pub const PURGE_QUEUE_PARAMETERS: &[&str] = &["QueueUrl"];

/// The parameters the purge must carry a value for, from the same catalog.
pub const PURGE_QUEUE_REQUIRED_PARAMETERS: &[&str] =
    crate::state::integration_subtype::SQS_PURGE_QUEUE.required_parameters;

/// The `arnNamespace` Amazon SQS publishes its endpoint and signs under, which is the
/// endpoint label the in-process client is addressed at.
const SQS_ENDPOINT: &str = "sqs";

/// The action of the Amazon SQS api each subtype performs, which is what the envelope its
/// answer is reported in is named after (`{Action}Response` / `{Action}Result`).
const SEND_MESSAGE_ACTION: &str = "SendMessage";
const RECEIVE_MESSAGE_ACTION: &str = "ReceiveMessage";
const DELETE_MESSAGE_ACTION: &str = "DeleteMessage";
const PURGE_QUEUE_ACTION: &str = "PurgeQueue";

/// The members of the mapping these actions read, spelled as an integration's
/// `RequestParameters` spell them.
const QUEUE_URL: &str = "QueueUrl";
const MESSAGE_BODY: &str = "MessageBody";
const RECEIPT_HANDLE: &str = "ReceiptHandle";
const DELAY_SECONDS: &str = "DelaySeconds";
const MESSAGE_DEDUPLICATION_ID: &str = "MessageDeduplicationId";
const MESSAGE_GROUP_ID: &str = "MessageGroupId";
const MAX_NUMBER_OF_MESSAGES: &str = "MaxNumberOfMessages";
const VISIBILITY_TIMEOUT: &str = "VisibilityTimeout";
const WAIT_TIME_SECONDS: &str = "WaitTimeSeconds";
const RECEIVE_REQUEST_ATTEMPT_ID: &str = "ReceiveRequestAttemptId";

/// The namespace the query interface of the queue api states its documents in: the
/// `doc/{api version}` namespace it publishes, for the api version the model declares
/// (`smithy_query(query="service_info")` reports `"version": "2012-11-05"`).
const SQS_QUERY_NAMESPACE: &str = "http://queue.amazonaws.com/doc/2012-11-05/";

/// The queue the mapping names, or the reason there is no call to make.
///
/// Every one of these actions acts on one queue, and an integration mapping nothing at all
/// for that member names none: that is the "integration produced no response" class rather
/// than a call against some other queue.
fn queue_url<'m>(mapped: &'m Mapped, subtype: &str) -> Result<&'m str, Unperformed> {
    mapped.get(QUEUE_URL).ok_or_else(|| {
        Unperformed::NotPerformed(format!(
            "the integration maps no {QUEUE_URL}, which {subtype} requires"
        ))
    })
}

/// One mapped value as the INTEGER member `parameter` is: a value that is not one is not an
/// argument the action can be called with, so the request is reported rather than called
/// without it.
fn integer(value: &str, parameter: &str) -> Result<i32, Unperformed> {
    value.parse::<i32>().map_err(|error| {
        Unperformed::NotPerformed(format!(
            "the integration maps a {parameter} this action cannot be called with: {error}"
        ))
    })
}

/// Send the mapped message, and answer the document Amazon SQS returned.
pub async fn send_message(
    ctx: &OperationContext<'_>,
    region: &str,
    identity: Option<&SigningIdentity>,
    mapped: &Mapped,
) -> Result<Performed, Unperformed> {
    let (Some(queue_url), Some(body)) = (mapped.get(QUEUE_URL), mapped.get(MESSAGE_BODY)) else {
        return Err(Unperformed::NotPerformed(format!(
            "the integration maps no {QUEUE_URL} and {MESSAGE_BODY}, which \
             {SEND_MESSAGE_SUBTYPE} requires"
        )));
    };
    let client = action_client!(aws_sdk_sqs, ctx.callback(), SQS_ENDPOINT, region, identity);
    let mut call = client.send_message().queue_url(queue_url).message_body(body);
    if let Some(delay) = mapped.get(DELAY_SECONDS) {
        // `DelaySeconds` is an integer member, so a mapped value that is not one is not a
        // delay the action can be called with.
        call = call.delay_seconds(integer(delay, DELAY_SECONDS)?);
    }
    if let Some(deduplication_id) = mapped.get(MESSAGE_DEDUPLICATION_ID) {
        call = call.message_deduplication_id(deduplication_id);
    }
    if let Some(group_id) = mapped.get(MESSAGE_GROUP_ID) {
        call = call.message_group_id(group_id);
    }
    let sent = call
        .send()
        .await
        .map_err(|error| super::unperformed(SEND_MESSAGE_SUBTYPE, &error))?;
    // The members of the action's result, in the order its output shape declares them.
    let answered: [(&str, Option<&str>); 5] = [
        ("MD5OfMessageBody", sent.md5_of_message_body()),
        ("MD5OfMessageAttributes", sent.md5_of_message_attributes()),
        (
            "MD5OfMessageSystemAttributes",
            sent.md5_of_message_system_attributes(),
        ),
        ("MessageId", sent.message_id()),
        ("SequenceNumber", sent.sequence_number()),
    ];
    Ok(Performed::query_envelope(query_envelope(
        SEND_MESSAGE_ACTION,
        Some(&members(&answered)),
        sent.request_id(),
    )))
}

/// Read the mapped queue, and answer the document Amazon SQS returned.
pub async fn receive_message(
    ctx: &OperationContext<'_>,
    region: &str,
    identity: Option<&SigningIdentity>,
    mapped: &Mapped,
) -> Result<Performed, Unperformed> {
    let queue_url = queue_url(mapped, RECEIVE_MESSAGE_SUBTYPE)?;
    let client = action_client!(aws_sdk_sqs, ctx.callback(), SQS_ENDPOINT, region, identity);
    let mut call = client.receive_message().queue_url(queue_url);
    // The three integer members of the input, each read as the integer it is.
    if let Some(maximum) = mapped.get(MAX_NUMBER_OF_MESSAGES) {
        call = call.max_number_of_messages(integer(maximum, MAX_NUMBER_OF_MESSAGES)?);
    }
    if let Some(visibility) = mapped.get(VISIBILITY_TIMEOUT) {
        call = call.visibility_timeout(integer(visibility, VISIBILITY_TIMEOUT)?);
    }
    if let Some(wait) = mapped.get(WAIT_TIME_SECONDS) {
        call = call.wait_time_seconds(integer(wait, WAIT_TIME_SECONDS)?);
    }
    if let Some(attempt) = mapped.get(RECEIVE_REQUEST_ATTEMPT_ID) {
        call = call.receive_request_attempt_id(attempt);
    }
    let received = call
        .send()
        .await
        .map_err(|error| super::unperformed(RECEIVE_MESSAGE_SUBTYPE, &error))?;
    // The one member `ReceiveMessageResult` declares is the LIST of messages the receive
    // took, which this interface writes flattened: one `Message` element per message, under
    // the name the member's own `@xmlName` states.
    let result: String = received.messages().iter().map(message_element).collect();
    Ok(Performed::query_envelope(query_envelope(
        RECEIVE_MESSAGE_ACTION,
        Some(&result),
        received.request_id(),
    )))
}

/// Delete the mapped message, and answer the document Amazon SQS returned.
pub async fn delete_message(
    ctx: &OperationContext<'_>,
    region: &str,
    identity: Option<&SigningIdentity>,
    mapped: &Mapped,
) -> Result<Performed, Unperformed> {
    let queue_url = queue_url(mapped, DELETE_MESSAGE_SUBTYPE)?;
    let Some(receipt_handle) = mapped.get(RECEIPT_HANDLE) else {
        return Err(Unperformed::NotPerformed(format!(
            "the integration maps no {RECEIPT_HANDLE}, which {DELETE_MESSAGE_SUBTYPE} requires"
        )));
    };
    let client = action_client!(aws_sdk_sqs, ctx.callback(), SQS_ENDPOINT, region, identity);
    let deleted = client
        .delete_message()
        .queue_url(queue_url)
        .receipt_handle(receipt_handle)
        .send()
        .await
        .map_err(|error| super::unperformed(DELETE_MESSAGE_SUBTYPE, &error))?;
    // `DeleteMessage` answers no member at all, so the envelope carries no result element
    // (see the module docs).
    Ok(Performed::query_envelope(query_envelope(
        DELETE_MESSAGE_ACTION,
        None,
        deleted.request_id(),
    )))
}

/// Purge the mapped queue, and answer the document Amazon SQS returned.
pub async fn purge_queue(
    ctx: &OperationContext<'_>,
    region: &str,
    identity: Option<&SigningIdentity>,
    mapped: &Mapped,
) -> Result<Performed, Unperformed> {
    let queue_url = queue_url(mapped, PURGE_QUEUE_SUBTYPE)?;
    let client = action_client!(aws_sdk_sqs, ctx.callback(), SQS_ENDPOINT, region, identity);
    let purged = client
        .purge_queue()
        .queue_url(queue_url)
        .send()
        .await
        .map_err(|error| super::unperformed(PURGE_QUEUE_SUBTYPE, &error))?;
    // `PurgeQueue` answers no member either.
    Ok(Performed::query_envelope(query_envelope(
        PURGE_QUEUE_ACTION,
        None,
        purged.request_id(),
    )))
}

/// The document the QUERY INTERFACE of the queue api answers `action` with: the result the
/// service answered under `{action}Result`, and the `ResponseMetadata` that names the
/// request, both inside the `{action}Response` element that interface wraps them in (see the
/// module docs for why this is the form a first-class action is relayed in).
///
/// `result` is the content of that result element, already written as this interface writes
/// the action's own members; `None` is an action whose output declares NO member, for which
/// that interface writes no result element at all.
///
/// Written the way this emulator's own query serializer writes an answer
/// (`vendor/smithy-schema-server/src/query.rs`), with every value escaped through the one
/// XML escaper the framework exposes, so the envelope is well formed whatever the service
/// answered. A member the service answered nothing for is LEFT OFF, which is how an absent
/// member is written on this wire.
///
/// The request named is the one the SERVICE named for the call it served, since this envelope
/// is that call's answer; a call whose answer named none is named by a freshly minted id, the
/// way every other `ResponseMetadata/RequestId` this tree reports is.
fn query_envelope(action: &str, result: Option<&str>, request: Option<&str>) -> String {
    let mut envelope = format!(
        "<{action}Response xmlns=\"{}\">",
        xml_escape(SQS_QUERY_NAMESPACE)
    );
    if let Some(result) = result {
        envelope.push_str(&format!("<{action}Result>{result}</{action}Result>"));
    }
    let request_id = request
        .map(str::to_string)
        .unwrap_or_else(::service_common::query_error_envelope::new_request_id);
    envelope.push_str(&format!(
        "<ResponseMetadata><RequestId>{}</RequestId></ResponseMetadata></{action}Response>",
        xml_escape(&request_id)
    ));
    envelope
}

/// The members of one result or one structure, each written as `<member>value</member>` and
/// each one the service answered nothing for left off.
fn members(answered: &[(&str, Option<&str>)]) -> String {
    let mut written = String::new();
    for (member, value) in answered {
        if let Some(value) = value {
            written.push_str(&element(member, value));
        }
    }
    written
}

/// One element carrying one value, escaped.
fn element(name: &str, value: &str) -> String {
    format!("<{name}>{}</{name}>", xml_escape(value))
}

/// One `<Message>` element of a receive's result: the members `Message` declares, in the
/// order it declares them, each left off when the queue answered nothing for it.
///
/// The two MAPS among them are written as this interface writes a map: one entry element per
/// pair, carrying the `Name` and `Value` elements the map's own key and value `@xmlName`
/// state, under the entry name the member's own `@xmlName` states (`Attribute` for the
/// system attributes, `MessageAttribute` for the message's own). The entries of each are
/// written in NAME order, because the map Amazon SQS answered is unordered and an answer has
/// to be one document.
fn message_element(message: &Message) -> String {
    let mut written = members(&[
        ("MessageId", message.message_id()),
        ("ReceiptHandle", message.receipt_handle()),
        ("MD5OfBody", message.md5_of_body()),
        ("Body", message.body()),
    ]);
    if let Some(attributes) = message.attributes() {
        let mut entries: Vec<(&str, &str)> = attributes
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        entries.sort_unstable();
        for (name, value) in entries {
            written.push_str(&format!(
                "<Attribute>{}{}</Attribute>",
                element("Name", name),
                element("Value", value)
            ));
        }
    }
    written.push_str(&members(&[(
        "MD5OfMessageAttributes",
        message.md5_of_message_attributes(),
    )]));
    if let Some(attributes) = message.message_attributes() {
        let mut names: Vec<&String> = attributes.keys().collect();
        names.sort_unstable();
        for name in names {
            let Some(value) = attributes.get(name) else {
                continue;
            };
            written.push_str(&format!(
                "<MessageAttribute>{}<Value>{}</Value></MessageAttribute>",
                element("Name", name),
                attribute_value(value)
            ));
        }
    }
    format!("<Message>{written}</Message>")
}

/// The members of one `MessageAttributeValue`, in the order that structure declares them: the
/// two values an attribute carries, the two lists it reserves, and the `DataType` it marks
/// required. A blob is written as the base64 this wire states one in.
fn attribute_value(value: &MessageAttributeValue) -> String {
    let mut written = members(&[("StringValue", value.string_value())]);
    if let Some(binary) = value.binary_value() {
        written.push_str(&element(
            "BinaryValue",
            &::base64::engine::general_purpose::STANDARD.encode(binary.as_ref()),
        ));
    }
    for listed in value.string_list_values() {
        written.push_str(&element("StringListValue", listed));
    }
    for listed in value.binary_list_values() {
        written.push_str(&element(
            "BinaryListValue",
            &::base64::engine::general_purpose::STANDARD.encode(listed.as_ref()),
        ));
    }
    written.push_str(&element("DataType", value.data_type()));
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the envelope carries the members the service answered, under the result element
    /// the action names, and leaves off every member it answered nothing for.
    #[test]
    fn the_envelope_carries_the_members_the_service_answered() {
        /// One case: what the service answered, the request it named for the call it served,
        /// the fragments the envelope must then carry, and the ones it must not.
        struct Row {
            case: &'static str,
            answered: &'static [(&'static str, Option<&'static str>)],
            request: Option<&'static str>,
            expected: &'static [&'static str],
            absent: &'static [&'static str],
        }
        // Rows differ along WHAT the service answered: every member, the two a standard
        // queue's send answers, nothing at all, and a value carrying XML metacharacters.
        let rows: &[Row] = &[
            Row {
                case: "every member of the result",
                answered: &[
                    ("MD5OfMessageBody", Some("7299ee64a9c302c7c74ebf3dc3dcc03f")),
                    ("MessageId", Some("m-1")),
                    ("SequenceNumber", Some("18849496460467696128")),
                ],
                request: Some("27daac76-34dd-47df-bd01-1f6e873584a0"),
                expected: &[
                    "<MD5OfMessageBody>7299ee64a9c302c7c74ebf3dc3dcc03f</MD5OfMessageBody>",
                    "<MessageId>m-1</MessageId>",
                    "<SequenceNumber>18849496460467696128</SequenceNumber>",
                    "<RequestId>27daac76-34dd-47df-bd01-1f6e873584a0</RequestId>",
                ],
                absent: &[],
            },
            Row {
                case: "the members a standard queue's send answers",
                answered: &[
                    ("MD5OfMessageBody", Some("d41d8cd98f00b204e9800998ecf8427e")),
                    ("MessageId", Some("m-2")),
                    ("SequenceNumber", None),
                ],
                request: Some("9f0a1b2c-3d4e-5f60-7182-93a4b5c6d7e8"),
                expected: &["<MessageId>m-2</MessageId>"],
                absent: &["SequenceNumber"],
            },
            Row {
                case: "nothing at all, and no request named",
                answered: &[("MessageId", None)],
                request: None,
                expected: &["<SendMessageResult></SendMessageResult>"],
                absent: &["MessageId"],
            },
            Row {
                case: "a value carrying XML metacharacters",
                answered: &[("MessageId", Some("a<b&c"))],
                request: None,
                expected: &["<MessageId>a&lt;b&amp;c</MessageId>"],
                absent: &["a<b&c"],
            },
        ];
        for row in rows {
            let case = row.case;
            let envelope = query_envelope(
                SEND_MESSAGE_ACTION,
                Some(&members(row.answered)),
                row.request,
            );
            assert!(
                envelope.starts_with(&format!(
                    "<SendMessageResponse xmlns=\"{SQS_QUERY_NAMESPACE}\"><SendMessageResult>"
                )),
                "case {case:?}: expected the action's own envelope, got {envelope:?}"
            );
            assert!(
                envelope.ends_with("</SendMessageResponse>"),
                "case {case:?}: expected a closed envelope, got {envelope:?}"
            );
            for fragment in row.expected {
                assert!(
                    envelope.contains(fragment),
                    "case {case:?}: expected {fragment:?} in {envelope:?}"
                );
            }
            for fragment in row.absent {
                assert!(
                    !envelope.contains(fragment),
                    "case {case:?}: expected no {fragment:?} in {envelope:?}"
                );
            }
        }
    }

    /// RULE: an action whose own output declares no member carries no result element at all —
    /// the response element holds the `ResponseMetadata` alone — while one that declares
    /// members always carries that element, empty when the service answered nothing.
    #[test]
    fn an_action_answering_no_member_carries_no_result_element() {
        // Rows differ along the action and what it answered: the two Amazon SQS actions whose
        // output is `Unit`, and one that answers members but answered none of them here.
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("the delete", DELETE_MESSAGE_ACTION, None),
            ("the purge", PURGE_QUEUE_ACTION, None),
            (
                "a receive that took no message",
                RECEIVE_MESSAGE_ACTION,
                Some(""),
            ),
        ];
        for (case, action, result) in rows {
            let envelope = query_envelope(action, *result, Some("27daac76-34dd-47df-bd01-1f"));
            let expected = match result {
                Some(_) => format!(
                    "<{action}Response xmlns=\"{SQS_QUERY_NAMESPACE}\"><{action}Result>\
                     </{action}Result><ResponseMetadata><RequestId>\
                     27daac76-34dd-47df-bd01-1f</RequestId></ResponseMetadata>\
                     </{action}Response>"
                ),
                None => format!(
                    "<{action}Response xmlns=\"{SQS_QUERY_NAMESPACE}\"><ResponseMetadata>\
                     <RequestId>27daac76-34dd-47df-bd01-1f</RequestId></ResponseMetadata>\
                     </{action}Response>"
                ),
            };
            assert_eq!(
                envelope, expected,
                "case {case:?}: expected {expected:?}, got {envelope:?}"
            );
        }
    }

    /// RULE: a message is written as the members `Message` declares, in that order, each one
    /// the queue answered nothing for left off, and each map's entries in name order.
    #[test]
    fn a_message_is_written_as_the_members_it_declares() {
        use ::aws_smithy_types::Blob;
        use aws_sdk_sqs::types::MessageSystemAttributeName;

        /// One case: the message the queue answered, and the document it is written as.
        struct Row {
            case: &'static str,
            message: Message,
            expected: String,
        }
        let rows: Vec<Row> = vec![
            Row {
                case: "the members every receive answers",
                message: Message::builder()
                    .message_id("m-1")
                    .receipt_handle("h-1")
                    .md5_of_body("d41d8cd98f00b204e9800998ecf8427e")
                    .body("a marker")
                    .build(),
                expected: "<Message><MessageId>m-1</MessageId><ReceiptHandle>h-1\
                           </ReceiptHandle><MD5OfBody>d41d8cd98f00b204e9800998ecf8427e\
                           </MD5OfBody><Body>a marker</Body></Message>"
                    .to_string(),
            },
            Row {
                case: "a message the queue answered nothing but its body for",
                message: Message::builder().body("a marker").build(),
                expected: "<Message><Body>a marker</Body></Message>".to_string(),
            },
            Row {
                case: "a body carrying XML metacharacters",
                message: Message::builder().body("a<b&c").build(),
                expected: "<Message><Body>a&lt;b&amp;c</Body></Message>".to_string(),
            },
            Row {
                case: "the system attributes a receive asked for, out of name order",
                message: Message::builder()
                    .message_id("m-2")
                    .attributes(MessageSystemAttributeName::SentTimestamp, "1793241840725")
                    .attributes(MessageSystemAttributeName::ApproximateReceiveCount, "1")
                    .build(),
                expected: "<Message><MessageId>m-2</MessageId><Attribute><Name>\
                           ApproximateReceiveCount</Name><Value>1</Value></Attribute>\
                           <Attribute><Name>SentTimestamp</Name><Value>1793241840725</Value>\
                           </Attribute></Message>"
                    .to_string(),
            },
            Row {
                case: "a message attribute of each kind of value",
                message: Message::builder()
                    .message_id("m-3")
                    .md5_of_message_attributes("3ae8f24a165a8cedc005670c81a27295")
                    .message_attributes(
                        "note",
                        MessageAttributeValue::builder()
                            .string_value("placed")
                            .data_type("String")
                            .build()
                            .expect("a DataType is all this value requires"),
                    )
                    .message_attributes(
                        "blob",
                        MessageAttributeValue::builder()
                            .binary_value(Blob::new(&b"\x00\x01"[..]))
                            .data_type("Binary")
                            .build()
                            .expect("a DataType is all this value requires"),
                    )
                    .build(),
                expected: "<Message><MessageId>m-3</MessageId><MD5OfMessageAttributes>\
                           3ae8f24a165a8cedc005670c81a27295</MD5OfMessageAttributes>\
                           <MessageAttribute><Name>blob</Name><Value><BinaryValue>AAE=\
                           </BinaryValue><DataType>Binary</DataType></Value>\
                           </MessageAttribute><MessageAttribute><Name>note</Name><Value>\
                           <StringValue>placed</StringValue><DataType>String</DataType>\
                           </Value></MessageAttribute></Message>"
                    .to_string(),
            },
        ];
        for row in &rows {
            let case = row.case;
            let written = message_element(&row.message);
            assert_eq!(
                written, row.expected,
                "case {case:?}: expected {:?}, got {written:?}",
                row.expected
            );
        }
    }

    /// RULE: the `ResponseMetadata` names the request the SERVICE named for the call it
    /// served, and a call whose answer named none is named by a fresh id of its own.
    #[test]
    fn the_envelope_names_the_request_the_service_named() {
        let case = "the request the service named";
        let named = query_envelope(
            SEND_MESSAGE_ACTION,
            Some(&members(&[("MessageId", Some("m-1"))])),
            Some("27daac76-34dd-47df-bd01-1f6e873584a0"),
        );
        assert_eq!(
            request_id_of(&named).as_deref(),
            Some("27daac76-34dd-47df-bd01-1f6e873584a0"),
            "case {case:?}: expected the service's own request id, got {named:?}"
        );
        let case = "three answers naming no request";
        let envelopes: Vec<String> = (0..3)
            .map(|_| {
                query_envelope(
                    SEND_MESSAGE_ACTION,
                    Some(&members(&[("MessageId", Some("m-1"))])),
                    None,
                )
            })
            .collect();
        let mut ids = Vec::new();
        for envelope in &envelopes {
            let id = request_id_of(envelope)
                .unwrap_or_else(|| panic!("case {case:?}: no request id in {envelope:?}"));
            assert!(
                ::service_common::query_error_envelope::is_request_id(&id),
                "case {case:?}: expected the form every AWS request id carries, got {id:?}"
            );
            ids.push(id);
        }
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            envelopes.len(),
            "case {case:?}: expected each answer to name its own request, got {ids:?}"
        );
    }

    /// RULE: a mapping that names no queue names no call at all, and is reported as an
    /// integration that produced no response rather than performed against another queue.
    #[test]
    fn a_mapping_naming_no_queue_names_no_call() {
        let unmapped = Mapped::default();
        for subtype in [
            RECEIVE_MESSAGE_SUBTYPE,
            DELETE_MESSAGE_SUBTYPE,
            PURGE_QUEUE_SUBTYPE,
        ] {
            let reported = queue_url(&unmapped, subtype);
            assert!(
                matches!(reported, Err(Unperformed::NotPerformed(_))),
                "case {subtype:?}: expected no call to be named, got {reported:?}"
            );
        }
    }

    /// RULE: an integer member is read as the integer it is, and a mapped value that is not
    /// one is no argument the action can be called with.
    #[test]
    fn an_integer_parameter_is_read_as_the_integer_it_is() {
        // Rows differ along the VALUE mapped: the bounds a count carries, zero, a negative
        // one, text, a decimal and an empty value.
        let rows: &[(&str, Option<i32>)] = &[
            ("1", Some(1)),
            ("10", Some(10)),
            ("0", Some(0)),
            ("-1", Some(-1)),
            ("43200", Some(43_200)),
            ("ten", None),
            ("1.5", None),
            ("", None),
        ];
        for (value, expected) in rows {
            let read = integer(value, MAX_NUMBER_OF_MESSAGES);
            match expected {
                Some(expected) => assert_eq!(
                    read.as_ref().ok(),
                    Some(expected),
                    "case {value:?}: expected {expected}, got {read:?}"
                ),
                None => assert!(
                    matches!(read, Err(Unperformed::NotPerformed(_))),
                    "case {value:?}: expected no argument at all, got {read:?}"
                ),
            }
        }
    }

    /// The id one envelope's `ResponseMetadata` names the request by.
    fn request_id_of(envelope: &str) -> Option<String> {
        let opened = "<ResponseMetadata><RequestId>";
        let start = envelope.find(opened)? + opened.len();
        let rest = envelope.get(start..)?;
        let end = rest.find("</RequestId>")?;
        rest.get(..end).map(str::to_string)
    }
}
