// SPDX-License-Identifier: CC0-1.0

//! What a deployed method's RESPONSE TEMPLATE reads off the document the integration
//! answered, over the two document forms an integration answers one in.
//!
//! RULE: the payload a mapping template's `$input.json`/`$input.path` query is applied to is
//! read as the DOCUMENT its bytes carry — "takes a JSONPath expression string (x) and returns
//! a JSON object representation of the result" (`$input.path`) — so an XML answer, which is
//! what every AWS QUERY-protocol service answers, resolves its elements by name exactly as a
//! JSON answer resolves its members: a nested element is a member of its parent, repeated
//! sibling elements are a list, a text-only element is its text, and the result is named both
//! through the document's root element and as the document's own members. That is what makes
//! the standard service-proxy integration — an `AWS` integration to a query api whose
//! integration response renders members of the answer, the `Publish` pattern among them —
//! serve a method response at all, and what makes the error integration response of such a
//! method surface the refusing service's own code, fault type and sentence.
//!
//! The rows vary what the rule generalises over: the FORM of the answered document (an XML
//! document, an XML document whose root carries a namespace attribute, an XML error envelope,
//! an XML document with repeated siblings, an element with no text, an element whose text
//! carries an escaped character, and a JSON document, which is read as it always was), the
//! DEPTH the query reaches into it, whether the query names the result through the root or as
//! a member of the document, and whether it asks for the node itself (`$input.path`) or for
//! its JSON spelling (`$input.json`).
//!
//! The second test drives the same rule end to end through the integration a client actually
//! configures: a non-proxy `AWS` integration to an Amazon SNS topic under a role, whose 200
//! integration response renders the `MessageId` the topic minted out of that api's XML answer
//! and whose 403 integration response surfaces the `AuthorizationError` a role without the
//! publish earns. What a client reads is checked against what the topic DELIVERED — the
//! envelope subscriber's own copy of that MessageId — so no member of the served document is
//! one this suite could have invented.

use bytes::Bytes;
use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    deploy, integration_response, json_template, method_response, mock_resource, put_integration,
    put_method,
};
use crate::rest_api_lifecycle::{
    create_api, create_resource, gateway_with_sns_sqs_and_iam, string_member, Gateway,
};

/// The region every request of this suite is addressed in.
const REGION: &str = "us-east-1";

/// The stage the apis are deployed to.
const STAGE: &str = "live";

/// The api version every `awsQuery` request to Amazon SNS states: that service's model
/// version (`smithy_query(query="service_info")` of that service's model).
const SNS_VERSION: &str = "2010-03-31";

/// The topic the service-proxy integration publishes to.
const TOPIC: &str = "service-proxy-orders";

/// The queue subscribed to that topic for ENVELOPE delivery, where the SNS JSON the
/// subscriber is handed is read.
const ENVELOPE_QUEUE: &str = "service-proxy-envelope";

/// The queue subscribed to it for RAW delivery, under a filter policy on the publish's own
/// `color` attribute.
const RAW_QUEUE: &str = "service-proxy-raw";

/// The media type the method requests declare, which is the key the request and response
/// templates are stored under.
const JSON: &str = "application/json";

/// The attribute value the admitted publish carries, which is both the filter policy's one
/// admitted value and the `x-color` header the client sends.
const ADMITTED_COLOR: &str = "indigo";

/// The attribute value the filter policy does NOT admit.
const FILTERED_COLOR: &str = "ochre";

/// The error code Amazon SNS refuses a call the role may not make with, and the fault type of
/// that refusal: both fixed text of the class, surfaced by the error integration response.
const AUTHORIZATION_ERROR: &str = "AuthorizationError";

/// See [`AUTHORIZATION_ERROR`].
const SENDER_FAULT: &str = "Sender";

/// One answered invoke of a deployed stage: its status and its body verbatim.
struct Invoked {
    status: u16,
    body: String,
}

impl Invoked {
    /// The body as JSON, `Null` for a body that is not a JSON document.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// Issue one request to `api`'s own invoke endpoint carrying `body` and `headers`, as a client
/// of a public method sends it: no credential at all, on the api's own host.
async fn invoke(
    gateway: &Gateway,
    api: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Invoked {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", JSON);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Bytes::from(body.to_owned()))
        .expect("the invoke request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).into_owned();
    Invoked { status, body }
}

/// One Amazon SNS `awsQuery` call on the same router that has to succeed, answering its body.
async fn sns_ok(gateway: &Gateway, case: &str, action: &str, fields: &str) -> String {
    let host = format!("sns.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sns\
                 /aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(Bytes::from(format!(
            "Action={action}&Version={SNS_VERSION}&{fields}"
        )))
        .expect("a well-formed request");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).into_owned();
    assert_eq!(
        status, 200,
        "setup {case:?}: expected HTTP 200 from Amazon SNS {action}, got {status} with {body:?}"
    );
    body
}

/// One Amazon SQS call on the same router that has to succeed, as its parsed body.
async fn sqs_ok(gateway: &Gateway, case: &str, operation: &str, body: Value) -> Value {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sqs\
                 /aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "setup {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One IAM role API Gateway may assume, carrying `document` as its inline policy; answers the
/// ARN IAM holds it under.
async fn given_role(gateway: &Gateway, name: &str, document: Value) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "apigateway.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    });
    let host = format!("iam.{REGION}.amazonaws.com");
    let send = |body: String| async {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", &host)
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam\
                     /aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = gateway.send(request).await;
        response.status().as_u16()
    };
    let created = send(format!(
        "Action=CreateRole&Version=2010-05-08&RoleName={name}&AssumeRolePolicyDocument={}",
        urlencoded(&trust.to_string())
    ))
    .await;
    assert_eq!(created, 200, "setup CreateRole({name}): got {created}");
    let attached = send(format!(
        "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}&PolicyName=calls\
         &PolicyDocument={}",
        urlencoded(&document.to_string())
    ))
    .await;
    assert_eq!(attached, 200, "setup PutRolePolicy({name}): got {attached}");
    format!(
        "arn:aws:iam::{}:role/{name}",
        ::engine::emulator_account_id()
    )
}

/// `value` percent-encoded for a form field, by the maintained coder for that encoding, as
/// this target's sibling suites do.
fn urlencoded(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// One row of the document table: the case, the body the client sends as the document the
/// integration answers, the query a response template applies to it, and the text that query
/// has to render to.
struct Read {
    case: &'static str,
    answered: &'static str,
    query: &'static str,
    rendered: &'static str,
    /// Whether the template writes the query inside a quoted string, which is how a template
    /// renders a TEXT member; a query that names a whole node renders the document's own
    /// spelling of it, so it stands unquoted.
    quoted: bool,
}

/// The documents an integration answers and what a response template reads off each.
///
/// The `MOCK` integration is the one that passes the method request's own payload through as
/// the document it answered ("passes the method request body through the integration request
/// without transformation", `Integration$passthroughBehavior`), so each row's `answered` is
/// the document the response template is rendered against.
const READS: &[Read] = &[
    Read {
        case: "a nested element of an XML answer, the form a query api's result takes",
        answered: "<PublishResponse><PublishResult><MessageId>8f1a</MessageId>\
                   </PublishResult></PublishResponse>",
        query: "$input.path('$.PublishResponse.PublishResult.MessageId')",
        rendered: "8f1a",
        quoted: true,
    },
    Read {
        case: "the same document under the namespace attribute such an answer carries",
        answered: "<PublishResponse xmlns=\"http://sns.amazonaws.com/doc/2010-03-31/\">\
                   <PublishResult><MessageId>c3d4</MessageId></PublishResult>\
                   <ResponseMetadata><RequestId>r-1</RequestId></ResponseMetadata>\
                   </PublishResponse>",
        query: "$input.path('$.PublishResponse.PublishResult.MessageId')",
        rendered: "c3d4",
        quoted: true,
    },
    Read {
        case: "the code of an XML error envelope, which an error integration response \
               surfaces",
        answered: "<ErrorResponse><Error><Type>Sender</Type><Code>AuthorizationError</Code>\
                   <Message>no</Message></Error><RequestId>r-2</RequestId></ErrorResponse>",
        query: "$input.path('$.ErrorResponse.Error.Code')",
        rendered: "AuthorizationError",
        quoted: true,
    },
    Read {
        case: "the fault type of the same envelope, two levels in",
        answered: "<ErrorResponse><Error><Type>Sender</Type><Code>AuthorizationError</Code>\
                   <Message>no</Message></Error><RequestId>r-2</RequestId></ErrorResponse>",
        query: "$input.path('$.ErrorResponse.Error.Type')",
        rendered: "Sender",
        quoted: true,
    },
    Read {
        case: "the same code named as a member of the document rather than through its root",
        answered: "<ErrorResponse><Error><Type>Sender</Type><Code>AuthorizationError</Code>\
                   <Message>no</Message></Error><RequestId>r-2</RequestId></ErrorResponse>",
        query: "$input.path('$.Error.Code')",
        rendered: "AuthorizationError",
        quoted: true,
    },
    Read {
        case: "a result named as a member of the document, past its root",
        answered: "<PublishResponse><PublishResult><MessageId>5c6e</MessageId>\
                   </PublishResult></PublishResponse>",
        query: "$input.path('$.PublishResult.MessageId')",
        rendered: "5c6e",
        quoted: true,
    },
    Read {
        case: "repeated sibling elements, which are a list",
        answered: "<Topics><member><TopicArn>one</TopicArn></member>\
                   <member><TopicArn>two</TopicArn></member></Topics>",
        query: "$input.json('$.Topics.member')",
        rendered: r#"[{"TopicArn":"one"},{"TopicArn":"two"}]"#,
        quoted: false,
    },
    Read {
        case: "an element carrying no text at all",
        answered: "<GetTopicAttributesResponse><Attributes/></GetTopicAttributesResponse>",
        query: "$input.path('$.GetTopicAttributesResponse.Attributes')",
        rendered: "",
        quoted: true,
    },
    Read {
        case: "text carrying a character the document escapes",
        answered: "<ErrorResponse><Error><Message>User: a &amp; b is not authorized\
                   </Message></Error></ErrorResponse>",
        query: "$input.path('$.ErrorResponse.Error.Message')",
        rendered: "User: a & b is not authorized",
        quoted: true,
    },
    Read {
        case: "a JSON answer, read as its members as it always was",
        answered: r#"{"PublishResponse":{"PublishResult":{"MessageId":"7e2b"}}}"#,
        query: "$input.path('$.PublishResponse.PublishResult.MessageId')",
        rendered: "7e2b",
        quoted: true,
    },
    Read {
        case: "the whole JSON answer, spelled back out",
        answered: r#"{"published":true}"#,
        query: "$input.json('$')",
        rendered: r#"{"published":true}"#,
        quoted: false,
    },
];

#[tokio::test]
async fn a_response_template_reads_the_members_of_the_document_the_integration_answered() {
    let gateway = gateway_with_sns_sqs_and_iam();
    let (api, root) = create_api(&gateway, "service-proxy-document-reads").await;
    for (index, row) in READS.iter().enumerate() {
        let body = match row.quoted {
            true => format!("{{\"read\":\"{}\"}}", row.query),
            false => format!("{{\"read\":{}}}", row.query),
        };
        mock_resource(
            &gateway,
            &api,
            &root,
            &format!("read-{index}"),
            "POST",
            "200",
            json_template(&body),
        )
        .await;
    }
    deploy(&gateway, &api, STAGE).await;

    for (index, row) in READS.iter().enumerate() {
        let case = row.case;
        let answered = invoke(
            &gateway,
            &api,
            &format!("/{STAGE}/read-{index}"),
            &[],
            row.answered,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the status the integration response names, got {} with \
             {:?}",
            answered.status, answered.body
        );
        // The member as the row's own form of it: the text a quoted query rendered, and the
        // document an unquoted one wrote out.
        let expected = match row.quoted {
            true => Value::String(row.rendered.to_string()),
            false => serde_json::from_str(row.rendered).expect("the row's own expectation"),
        };
        assert_eq!(
            answered.json().get("read"),
            Some(&expected),
            "case {case:?}: expected {query:?} to render {rendered:?}, got {body:?}",
            query = row.query,
            rendered = row.rendered,
            body = answered.body
        );
    }
}

/// The topic this suite publishes to, with the two subscribers the service-proxy pattern fans
/// out to: one delivered the SNS ENVELOPE, one delivered the message RAW under a filter policy
/// on the publish's `color` attribute. Answers the topic's ARN and both queue URLs.
async fn topic_with_subscribers(gateway: &Gateway) -> (String, String, String) {
    let created = sns_ok(gateway, "CreateTopic", "CreateTopic", &format!("Name={TOPIC}")).await;
    let topic_arn = xml_element(&created, "TopicArn")
        .unwrap_or_else(|| panic!("setup CreateTopic: no <TopicArn> in {created}"));
    let mut urls = Vec::new();
    for (queue, raw) in [(ENVELOPE_QUEUE, false), (RAW_QUEUE, true)] {
        let url = string_member(
            &sqs_ok(gateway, queue, "CreateQueue", json!({ "QueueName": queue })).await,
            "QueueUrl",
            "CreateQueue",
        );
        let attributes = sqs_ok(
            gateway,
            queue,
            "GetQueueAttributes",
            json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
        )
        .await;
        let queue_arn = string_member(
            attributes.get("Attributes").unwrap_or(&Value::Null),
            "QueueArn",
            "GetQueueAttributes",
        );
        // The queue policy AWS documents for a queue subscribed to a topic: the service
        // principal `sns.amazonaws.com`, allowed `sqs:SendMessage` on that queue.
        sqs_ok(
            gateway,
            queue,
            "SetQueueAttributes",
            json!({
                "QueueUrl": url,
                "Attributes": {
                    "Policy": json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Sid": "AllowSNSDelivery",
                            "Effect": "Allow",
                            "Principal": { "Service": "sns.amazonaws.com" },
                            "Action": "sqs:SendMessage",
                            "Resource": queue_arn,
                        }],
                    })
                    .to_string(),
                },
            }),
        )
        .await;
        let mut fields = format!("TopicArn={topic_arn}&Protocol=sqs&Endpoint={queue_arn}");
        if raw {
            let policy = urlencoded(&json!({ "color": [ADMITTED_COLOR] }).to_string());
            fields.push_str(
                "&Attributes.entry.1.key=RawMessageDelivery&Attributes.entry.1.value=true\
                 &Attributes.entry.2.key=FilterPolicy&Attributes.entry.2.value=",
            );
            fields.push_str(&policy);
        }
        sns_ok(gateway, queue, "Subscribe", &fields).await;
        urls.push(url);
    }
    (topic_arn, urls[0].clone(), urls[1].clone())
}

/// Every message waiting on the queue at `url`, as its body and its message attributes, with
/// each one removed as it is read.
async fn drained(gateway: &Gateway, case: &str, url: &str) -> Vec<(String, Value)> {
    let received = sqs_ok(
        gateway,
        case,
        "ReceiveMessage",
        json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    let messages = received
        .get("Messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut read = Vec::new();
    for message in &messages {
        read.push((
            message
                .get("Body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            message
                .get("MessageAttributes")
                .cloned()
                .unwrap_or(Value::Null),
        ));
        sqs_ok(
            gateway,
            case,
            "DeleteMessage",
            json!({
                "QueueUrl": url,
                "ReceiptHandle": string_member(message, "ReceiptHandle", case),
            }),
        )
        .await;
    }
    read
}

/// The service-proxy method the standard "publish without a function" pattern configures: an
/// `AWS` integration addressing Amazon SNS's own root through the `path` form, under
/// `credentials`, passing nothing through, mapping the form media type onto the integration
/// request and rendering the publish as that api's wire form — with the two integration
/// responses a client reads the outcome through.
async fn publish_method(
    gateway: &Gateway,
    api: &str,
    resource: &str,
    topic_arn: &str,
    credentials: &str,
) {
    put_method(
        gateway,
        api,
        resource,
        "POST",
        json!({
            "authorizationType": "NONE",
            "requestParameters": { "method.request.header.x-color": true },
        }),
    )
    .await;
    put_integration(
        gateway,
        api,
        resource,
        "POST",
        json!({
            "type": "AWS",
            "httpMethod": "POST",
            "uri": format!("arn:aws:apigateway:{REGION}:sns:path//"),
            "credentials": credentials,
            "passthroughBehavior": "NEVER",
            "requestParameters": {
                "integration.request.header.Content-Type":
                    "'application/x-www-form-urlencoded'",
            },
            "requestTemplates": {
                JSON: format!(
                    "Action=Publish&TopicArn={topic_arn}\
                     &Message=$util.urlEncode($input.body)\
                     &MessageAttributes.entry.1.Name=color\
                     &MessageAttributes.entry.1.Value.DataType=String\
                     &MessageAttributes.entry.1.Value.StringValue=\
                     $util.urlEncode($input.params('x-color'))\
                     &MessageAttributes.entry.2.Name=qty\
                     &MessageAttributes.entry.2.Value.DataType=Number\
                     &MessageAttributes.entry.2.Value.StringValue=7"
                ),
            },
        }),
    )
    .await;
    method_response(gateway, api, resource, "POST", "200", json!({})).await;
    integration_response(
        gateway,
        api,
        resource,
        "POST",
        "200",
        json!({
            "statusCode": "200",
            "responseTemplates": {
                JSON: "{\"published\": true, \"messageId\": \
                       \"$input.path('$.PublishResponse.PublishResult.MessageId')\"}",
            },
        }),
    )
    .await;
    method_response(gateway, api, resource, "POST", "403", json!({})).await;
    integration_response(
        gateway,
        api,
        resource,
        "POST",
        "403",
        json!({
            "statusCode": "403",
            "selectionPattern": "403",
            "responseTemplates": {
                JSON: "{\"published\": false, \
                       \"surfacedCode\": \"$input.path('$.ErrorResponse.Error.Code')\", \
                       \"surfacedType\": \"$input.path('$.ErrorResponse.Error.Type')\", \
                       \"surfacedMessage\": \
                       \"$util.escapeJavaScript($input.path('$.ErrorResponse.Error.Message'))\"}",
            },
        }),
    )
    .await;
}

#[tokio::test]
async fn a_stage_publishing_to_a_topic_answers_the_members_of_the_answer_it_mapped() {
    let gateway = gateway_with_sns_sqs_and_iam();
    let (topic_arn, envelope_url, raw_url) = topic_with_subscribers(&gateway).await;
    let publisher = given_role(
        &gateway,
        "service-proxy-publisher",
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "sns:Publish",
                "Resource": topic_arn,
            }],
        }),
    )
    .await;
    // The role of the denied case: it may READ the topic and may not publish to it, which is
    // what makes the refusal the service's own authorization decision rather than a role that
    // may do nothing at all.
    let reader = given_role(
        &gateway,
        "service-proxy-reader",
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "sns:GetTopicAttributes",
                "Resource": topic_arn,
            }],
        }),
    )
    .await;

    let (api, root) = create_api(&gateway, "service-proxy-publish").await;
    for (path_part, role) in [("publish", &publisher), ("publish-denied", &reader)] {
        let resource = string_member(
            &create_resource(&gateway, &api, &root, path_part).await,
            "id",
            path_part,
        );
        publish_method(&gateway, &api, &resource, &topic_arn, role).await;
    }
    deploy(&gateway, &api, STAGE).await;

    // The granted route: the method response is the integration response rendered off the
    // document the topic answered.
    let sent = r#"{"order":"a service proxy order","qty":7}"#;
    let answered = invoke(
        &gateway,
        &api,
        &format!("/{STAGE}/publish"),
        &[("x-color", ADMITTED_COLOR)],
        sent,
    )
    .await;
    let case = "a publish the role may make";
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the 200 the integration response names, got {} with {:?}",
        answered.status, answered.body
    );
    let served = answered.json();
    assert_eq!(
        served.get("published"),
        Some(&json!(true)),
        "case {case:?}: expected the template's own member, got {:?}",
        answered.body
    );
    let message_id = served
        .get("messageId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !message_id.is_empty(),
        "case {case:?}: expected the MessageId the topic minted, got {:?}",
        answered.body
    );

    // What the ENVELOPE subscriber was delivered: the SNS JSON, carrying the very MessageId
    // the served document named and the typed attributes the integration set.
    let envelope = drained(&gateway, case, &envelope_url).await;
    assert_eq!(
        envelope.len(),
        1,
        "case {case:?}: expected the one message the stage published, got {envelope:?}"
    );
    let delivered: Value = serde_json::from_str(&envelope[0].0).unwrap_or(Value::Null);
    assert_eq!(
        delivered.get("MessageId").and_then(Value::as_str),
        Some(message_id.as_str()),
        "case {case:?}: expected the served messageId to be the one the topic delivered, got \
         {delivered}"
    );
    assert_eq!(
        delivered.get("TopicArn").and_then(Value::as_str),
        Some(topic_arn.as_str()),
        "case {case:?}: expected the topic the integration addressed, got {delivered}"
    );
    assert_eq!(
        delivered.get("Message").and_then(Value::as_str),
        Some(sent),
        "case {case:?}: expected the body the client sent, got {delivered}"
    );
    assert_eq!(
        delivered.get("MessageAttributes"),
        Some(&json!({
            "color": { "Type": "String", "Value": ADMITTED_COLOR },
            "qty": { "Type": "Number", "Value": "7" },
        })),
        "case {case:?}: expected the two typed attributes the request template set, got \
         {delivered}"
    );

    // What the RAW subscriber was delivered: the message itself, with those attributes as the
    // queue's own message attributes.
    let raw = drained(&gateway, case, &raw_url).await;
    assert_eq!(
        raw.len(),
        1,
        "case {case:?}: expected the filter policy to admit the matching publish, got {raw:?}"
    );
    assert_eq!(
        raw[0].0, sent,
        "case {case:?}: expected the raw subscriber to hold the body the client sent, got {:?}",
        raw[0].0
    );
    assert_eq!(
        raw[0].1,
        json!({
            "color": { "StringValue": ADMITTED_COLOR, "DataType": "String" },
            "qty": { "StringValue": "7", "DataType": "Number" },
        }),
        "case {case:?}: expected the published attributes as the queue's own, got {:?}",
        raw[0].1
    );

    // The same route with an attribute the filter policy does not admit: the envelope
    // subscriber holds it, the filtered one does not — and the stage still answers the 200 it
    // mapped.
    let filtered_case = "a publish the subscription's filter policy does not admit";
    let filtered = invoke(
        &gateway,
        &api,
        &format!("/{STAGE}/publish"),
        &[("x-color", FILTERED_COLOR)],
        sent,
    )
    .await;
    assert_eq!(
        filtered.status, 200,
        "case {filtered_case:?}: expected the 200 the integration response names, got {} with \
         {:?}",
        filtered.status, filtered.body
    );
    assert_eq!(
        drained(&gateway, filtered_case, &envelope_url).await.len(),
        1,
        "case {filtered_case:?}: expected the unfiltered subscriber to hold it"
    );
    assert!(
        drained(&gateway, filtered_case, &raw_url).await.is_empty(),
        "case {filtered_case:?}: expected the filter policy to admit nothing"
    );

    // The denied route: the refusal the topic's own service answered, surfaced through the
    // error integration response.
    let denied_case = "a publish the role may not make";
    let denied = invoke(
        &gateway,
        &api,
        &format!("/{STAGE}/publish-denied"),
        &[("x-color", ADMITTED_COLOR)],
        sent,
    )
    .await;
    assert_eq!(
        denied.status, 403,
        "case {denied_case:?}: expected the 403 the error integration response names, got {} \
         with {:?}",
        denied.status, denied.body
    );
    let surfaced = denied.json();
    assert_eq!(
        surfaced.get("published"),
        Some(&json!(false)),
        "case {denied_case:?}: expected the template's own member, got {:?}",
        denied.body
    );
    assert_eq!(
        surfaced.get("surfacedCode").and_then(Value::as_str),
        Some(AUTHORIZATION_ERROR),
        "case {denied_case:?}: expected the code Amazon SNS refuses with, got {:?}",
        denied.body
    );
    assert_eq!(
        surfaced.get("surfacedType").and_then(Value::as_str),
        Some(SENDER_FAULT),
        "case {denied_case:?}: expected the fault type of that refusal, got {:?}",
        denied.body
    );
    let sentence = surfaced
        .get("surfacedMessage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The sentence's variable parts: the principal the call would have been made as — a
    // SESSION of the role the integration names, never the role's own ARN — and the topic the
    // publish addressed.
    let session = format!(
        "arn:aws:sts::{}:assumed-role/{}/",
        ::engine::emulator_account_id(),
        reader.rsplit('/').next().unwrap_or_default(),
    );
    assert!(
        sentence.contains(&session) && sentence.contains(&topic_arn),
        "case {denied_case:?}: expected the sentence to name the session {session:?} and the \
         topic {topic_arn:?}, got {sentence:?}"
    );
    assert!(
        drained(&gateway, denied_case, &envelope_url).await.is_empty()
            && drained(&gateway, denied_case, &raw_url).await.is_empty(),
        "case {denied_case:?}: a refused publish delivers nothing"
    );
}
