// SPDX-License-Identifier: CC0-1.0

//! The TRANSPORT clause of a notification destination's own resource policy, judged against the
//! publication Amazon S3 really makes to it.
//!
//! RULE under test: **a destination's resource policy is decided FOR THE REQUEST THE PUBLICATION
//! ITSELF MAKES — Amazon S3's own service principal, acting for the publishing bucket and the
//! account that owns it, over an encrypted transport — so the decision carries a value for every
//! global condition key that request answers for, `aws:SecureTransport` among them, and declares
//! each of them as a key it models. A statement conditioned on one of those keys is therefore
//! DECIDED rather than left undecided: the guardrail AWS publishes for a resource that requires
//! encrypted transport — a `Deny` of every principal whose request did NOT arrive over TLS —
//! does not apply to the publication and the grant beside it stands, while a statement whose
//! clause DOES hold for the publication applies to it, and a clause over a key the publication
//! answers nothing for still leaves its statement undecided and so still refuses.**
//!
//! Why that is the rule: `aws:SecureTransport` is "whether the request was sent using SSL" (AWS
//! General Reference, *Global condition context keys*), and a destination's policy is what
//! `PutBucketNotificationConfiguration` validates each destination against — "after Amazon S3
//! receives this request, it first verifies that any Amazon Simple Notification Service (Amazon
//! SNS) or Amazon Simple Queue Service (Amazon SQS) destination exists, and that the bucket owner
//! has permission to publish to it by sending a test notification … If the message fails, the
//! entire PUT action will fail, and Amazon S3 will not add the configuration to your bucket". The
//! publication is made over the same encrypted transport every call this emulator serves arrives
//! on, so the key has a value, and a decision taken without it reads the guardrail `Deny` as a
//! statement that MIGHT apply — which the shared evaluator reads fail-closed, refusing a
//! destination whose policy grants the publication outright.
//!
//! Two other crates hold the destinations and the documents the decision is read from — Amazon
//! SQS holds the queue and its access policy, Amazon SNS the topic and its own — so the suite
//! lives here: a crate-local suite can only ever see one side of it. Its siblings reach the forms
//! of the GRANT itself ([`crate::s3_notification_queue_send_grant_gates_the_put`] for a queue,
//! [`crate::cloudformation_sns_topic_policy_admits_s3_notifications`] for a topic); what only
//! this suite reaches is the statement a policy carries BESIDE the grant, and the condition
//! context the publication answers it with.
//!
//! The rows differ along what the rule generalises over: which destination kind holds the policy
//! (a queue, a topic), which EFFECT the conditioned statement states (`Allow`, `Deny`), which
//! OPERATOR states the clause (`Bool`, `Null`, `StringEquals`), whether the value stated is the
//! one the publication carries or the other one, and whether the key is one the publication
//! answers for at all. A Lambda destination holds no such policy to carry: a function's
//! resource-based policy is assembled from `AddPermission` grants, which state `Allow` and carry
//! no `Deny`, so there is no guardrail statement to write on one.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, opt_element};
use crate::s3_notification_invokes_its_lambda_target::{
    authorization, build_router, create_bucket, driven, query, s3, Answer,
};

/// The Region every request here is signed for, and so the Region every queue and topic below
/// lives in — the Region the shared S3 harness signs for too.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes a bucket's notifications under — "for Amazon Web
/// Services services, the principal is a domain-style identifier that the service defines, such
/// as s3.amazonaws.com" (`AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// The condition keys the publication answers for: the bucket it is made for, the account that
/// owns that bucket, and the transport it is made over.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";
const SECURE_TRANSPORT_KEY: &str = "aws:SecureTransport";

/// A global condition key no publication of this emulator carries a value for, so a clause over
/// it can be decided by nothing — the row that pins the fail-closed reading still in force.
const UNANSWERED_KEY: &str = "aws:SourceVpc";

/// The event class every configuration below is notified of: "you can enable notifications for
/// every event of a type by using the wildcard" (`Event`).
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The refusal class S3 answers a container it could not validate with: the status, the code and
/// the FIXED sentence of the class.
const INVALID_ARGUMENT_STATUS: u16 = 400;
const INVALID_ARGUMENT: &str = "InvalidArgument";
const UNABLE_TO_VALIDATE: &str = "Unable to validate the following destination configurations";

/// The header the model binds `SkipDestinationValidation` to
/// (`smithy_query(query="operation",operation="PutBucketNotificationConfiguration")`).
const SKIP_VALIDATION_HEADER: &str = "x-amz-skip-destination-validation";

/// The `Event` of the test notification a VALIDATED queue destination receives when the
/// configuration is installed.
const TEST_EVENT: &str = "s3:TestEvent";

/// How long the delivery case waits for work the clock owes, and how often it looks while
/// waiting. Deliberately generous: this suite shares its host with the rest of the test target,
/// and nothing here tests how fast that host is.
const DELIVERY_BUDGET: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// Which service owns the destination a row's policy is written on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An Amazon SQS queue, whose `Policy` attribute carries the document.
    Queue,
    /// An Amazon SNS topic, whose own `Policy` attribute carries it.
    Topic,
}

impl Kind {
    /// The action Amazon S3 performs on this destination kind to publish a notification to it —
    /// the grant its policy has to state — and the wildcard standing for every action of the
    /// service that owns it, which is what a guardrail `Deny` covers.
    fn action(self) -> &'static str {
        match self {
            Self::Queue => "sqs:SendMessage",
            Self::Topic => "sns:Publish",
        }
    }

    fn every_action(self) -> &'static str {
        match self {
            Self::Queue => "sqs:*",
            Self::Topic => "sns:*",
        }
    }

    /// The noun S3's own per-destination detail names this kind by
    /// (`crate::s3_notification_queue_send_grant_gates_the_put` pins the queue's wording).
    fn noun(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Topic => "topic",
        }
    }

    /// The element the container binds this kind's destination ARN to
    /// (`smithy_query(query="shape",shape="QueueConfiguration,TopicConfiguration")`: the ARN
    /// members are bound as `Queue` and `Topic`).
    fn element(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::Topic => "Topic",
        }
    }

    /// The per-destination detail the refusal carries for a destination of this kind that is
    /// there and whose policy withholds the publication.
    fn unpermitted_detail(self) -> String {
        format!(
            "Permissions on the destination {} do not allow S3 to publish notifications from \
             this bucket",
            self.noun()
        )
    }
}

/// What the destination's own resource policy says — the only thing that differs between the
/// rows of one kind, and so the only thing each verdict can be about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The grant the publication needs, beside the guardrail AWS publishes for a resource that
    /// requires encrypted transport: every principal denied every action of the owning service
    /// where `aws:SecureTransport` is false.
    GrantBehindTheTlsGuardrail,
    /// The same shape with the clause's value the other way round, so the `Deny` is about the
    /// requests that DID arrive over TLS — which the publication is one of.
    GrantBehindADenyOfSecureTransport,
    /// The grant beside a `Deny` of every request carrying NO value for the transport key.
    GrantBehindADenyOfAbsentTransport,
    /// The grant itself conditioned on the transport, beside no deny at all.
    GrantConditionedOnSecureTransport,
    /// The grant itself conditioned on the transport the publication is NOT made over.
    GrantConditionedOnPlaintext,
    /// The guardrail alone: nothing grants the publication.
    TheGuardrailAlone,
    /// The grant beside a `Deny` conditioned on a key the publication answers nothing for.
    GrantBehindADenyOnAnUnansweredKey,
}

impl Form {
    /// The document to set on the destination: the statements this form carries.
    fn document(self, kind: Kind, resource: &str, bucket: &str) -> String {
        let statements = match self {
            Self::GrantBehindTheTlsGuardrail => vec![
                grant(kind, resource, bucket, None),
                guardrail(kind, resource, json!({"Bool": {SECURE_TRANSPORT_KEY: "false"}})),
            ],
            Self::GrantBehindADenyOfSecureTransport => vec![
                grant(kind, resource, bucket, None),
                publication_deny(kind, resource, json!({"Bool": {SECURE_TRANSPORT_KEY: "true"}})),
            ],
            Self::GrantBehindADenyOfAbsentTransport => vec![
                grant(kind, resource, bucket, None),
                guardrail(kind, resource, json!({"Null": {SECURE_TRANSPORT_KEY: "true"}})),
            ],
            Self::GrantConditionedOnSecureTransport => vec![grant(
                kind,
                resource,
                bucket,
                Some(json!({"Bool": {SECURE_TRANSPORT_KEY: "true"}})),
            )],
            Self::GrantConditionedOnPlaintext => vec![grant(
                kind,
                resource,
                bucket,
                Some(json!({"Bool": {SECURE_TRANSPORT_KEY: "false"}})),
            )],
            Self::TheGuardrailAlone => vec![guardrail(
                kind,
                resource,
                json!({"Bool": {SECURE_TRANSPORT_KEY: "false"}}),
            )],
            Self::GrantBehindADenyOnAnUnansweredKey => vec![
                grant(kind, resource, bucket, None),
                publication_deny(
                    kind,
                    resource,
                    json!({"StringEquals": {UNANSWERED_KEY: "vpc-0dcba9876543210ff"}}),
                ),
            ],
        };
        json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
    }
}

/// The grant a destination's policy states for the publication: the one action Amazon S3 performs
/// on this destination kind, for the Amazon S3 service principal, narrowed to this bucket and the
/// account that owns it the way a published grant narrows it — and carrying `condition` INSTEAD
/// where a row writes one of its own.
///
/// An S3 bucket's ARN carries no Region and no account, because a bucket name is globally unique;
/// the account the narrowing names is the one this emulator serves
/// ([`engine::emulator_account_id`]), which is every bucket's reported owner.
fn grant(kind: Kind, resource: &str, bucket: &str, condition: Option<Value>) -> Value {
    json!({
        "Sid": "AllowS3Publish",
        "Effect": "Allow",
        "Principal": {"Service": S3_PRINCIPAL},
        "Action": kind.action(),
        "Resource": resource,
        "Condition": condition.unwrap_or_else(|| json!({
            "ArnLike": {SOURCE_ARN_KEY: format!("arn:aws:s3:::{bucket}")},
            "StringEquals": {SOURCE_ACCOUNT_KEY: engine::emulator_account_id()},
        })),
    })
}

/// The guardrail statement, carrying `condition`: every principal denied every action of the
/// service that owns the destination. This is the shape AWS publishes for a resource that
/// requires encrypted transport, and `{"AWS": "*"}` is the spelling of "every principal" such a
/// statement names.
///
/// It is written only on the rows whose clause does NOT hold for the publication: a statement
/// denying every principal every action of the owning service denies the READ Amazon S3 makes of
/// the destination's own attributes as well, so a row that wants the clause to APPLY writes it on
/// the publication itself ([`publication_deny`]) and leaves the rest of the destination's API
/// alone — otherwise the verdict would be the destination's own service refusing the read rather
/// than the decision this suite is about.
fn guardrail(kind: Kind, resource: &str, condition: Value) -> Value {
    json!({
        "Sid": "DenyUnderTheClause",
        "Effect": "Deny",
        "Principal": {"AWS": "*"},
        "Action": kind.every_action(),
        "Resource": resource,
        "Condition": condition,
    })
}

/// A `Deny` of the PUBLICATION under `condition`: the Amazon S3 service principal, on the one
/// action a notification to this destination kind performs — see [`guardrail`] for why the rows
/// whose clause holds state the deny this narrowly.
fn publication_deny(kind: Kind, resource: &str, condition: Value) -> Value {
    json!({
        "Sid": "DenyS3PublishUnderTheClause",
        "Effect": "Deny",
        "Principal": {"Service": S3_PRINCIPAL},
        "Action": kind.action(),
        "Resource": resource,
        "Condition": condition,
    })
}

/// One row of the table: the destination kind, the policy form its destination carries, whether
/// the put asks for the documented opt-out, and whether the container is RECORDED.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    kind: Kind,
    form: Form,
    skip_validation: bool,
    recorded: bool,
}

/// Every row, for both destination kinds: the forms above, plus the opt-out over the one form
/// that grants nothing.
fn rows() -> Vec<Row> {
    let forms: &[(&'static str, Form, bool, bool)] = &[
        (
            "the grant behind the TLS guardrail AWS publishes",
            Form::GrantBehindTheTlsGuardrail,
            false,
            true,
        ),
        (
            "the grant behind a deny of the transport the publication is made over",
            Form::GrantBehindADenyOfSecureTransport,
            false,
            false,
        ),
        (
            "the grant behind a deny of a request carrying no transport value",
            Form::GrantBehindADenyOfAbsentTransport,
            false,
            true,
        ),
        (
            "the grant itself conditioned on the transport the publication is made over",
            Form::GrantConditionedOnSecureTransport,
            false,
            true,
        ),
        (
            "the grant itself conditioned on the transport the publication is not made over",
            Form::GrantConditionedOnPlaintext,
            false,
            false,
        ),
        (
            "the TLS guardrail with nothing granting the publication",
            Form::TheGuardrailAlone,
            false,
            false,
        ),
        (
            "the grant behind a deny over a key the publication answers nothing for",
            Form::GrantBehindADenyOnAnUnansweredKey,
            false,
            false,
        ),
        (
            "the TLS guardrail alone, with destination validation skipped",
            Form::TheGuardrailAlone,
            true,
            true,
        ),
    ];
    let mut built = Vec::new();
    for kind in [Kind::Queue, Kind::Topic] {
        for (case, form, skip_validation, recorded) in forms {
            built.push(Row {
                case,
                kind,
                form: *form,
                skip_validation: *skip_validation,
                recorded: *recorded,
            });
        }
    }
    built
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must be answered.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SNS call (awsQuery, API version 2010-03-31) that must be answered.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "sns", "2010-03-31", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SNS {action}, got {status} with {body}"
    );
    body
}

/// A queue called `name` carrying the document `form` describes: its url and its ARN, as Amazon
/// SQS itself reports them.
async fn queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    form: Form,
    bucket: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {reported}"))
        .to_string();
    let document = form.document(Kind::Queue, &arn, bucket);
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": document } }),
    )
    .await;
    (url, arn)
}

/// A topic called `name` carrying the document `form` describes, as its ARN.
async fn topic(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    form: Form,
    bucket: &str,
) -> String {
    let created = sns(router, case, "CreateTopic", &[("Name", name)]).await;
    let arn = element(&created, "TopicArn");
    assert!(
        !arn.is_empty(),
        "case {case:?}: expected a TopicArn from CreateTopic, got {created}"
    );
    let document = form.document(Kind::Topic, &arn, bucket);
    sns(
        router,
        case,
        "SetTopicAttributes",
        &[
            ("TopicArn", &arn),
            ("AttributeName", "Policy"),
            ("AttributeValue", &document),
        ],
    )
    .await;
    arn
}

/// One configuration of the container, as the request's XML carries it: the destination of
/// `kind`, notified of object creations under `prefix`.
fn configuration(kind: Kind, id: &str, arn: &str, prefix: &str) -> String {
    let container = match kind {
        Kind::Queue => "QueueConfiguration",
        Kind::Topic => "TopicConfiguration",
    };
    let bound = kind.element();
    format!(
        "<{container}><Id>{id}</Id><{bound}>{arn}</{bound}><Event>{OBJECT_CREATED}</Event>\
         <Filter><S3Key><FilterRule><Name>prefix</Name><Value>{prefix}</Value></FilterRule>\
         </S3Key></Filter></{container}>"
    )
}

/// `PUT /<bucket>?notification` with `configured` as the whole container, asking for the
/// documented opt-out when `skip_validation` — which is the only thing the header changes.
async fn put_notification(
    router: &MultiServiceRouter,
    bucket: &str,
    configured: &[String],
    skip_validation: bool,
) -> Answer {
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">{}</NotificationConfiguration>",
        configured.concat()
    );
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut building = http::Request::builder()
        .method("PUT")
        .uri(format!("https://{host}/{bucket}?notification"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream");
    if skip_validation {
        building = building.header(SKIP_VALIDATION_HEADER, "true");
    }
    let request = building
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// The destination ARNs `GetBucketNotificationConfiguration` reports for `bucket` under the
/// element `kind` binds its ARN to.
async fn reported(router: &MultiServiceRouter, case: &str, bucket: &str, kind: Kind) -> Vec<String> {
    let answer = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetBucketNotificationConfiguration({bucket:?}) must answer 200, got {} \
         with {}",
        answer.status, answer.body
    );
    elements(&answer.body, kind.element())
}

/// `PUT /<bucket>/<key>` — the object write whose event the configuration in force publishes.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        "the object the notification is published for",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// Take every message waiting on the queue at `url` off it, as the text each body carries.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let mut found = Vec::new();
    loop {
        let answered = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = answered["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return found;
        }
        for message in messages {
            found.push(message["Body"].as_str().unwrap_or_default().to_string());
            if let Some(receipt) = message["ReceiptHandle"].as_str() {
                sqs(
                    router,
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": receipt }),
                )
                .await;
            }
        }
    }
}

/// Look at the queue at `url` until a body it took satisfies `held`, or [`DELIVERY_BUDGET`]
/// passes — the CONDITION's deadline, not a measurement of it. Answers every body seen.
async fn awaited<Held>(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    held: Held,
) -> Vec<String>
where
    Held: Fn(&str) -> bool,
{
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut seen = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        if seen.iter().any(|body| held(body)) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Drive `rows` against ONE assembled emulator: every call S3 makes to validate a destination is
/// dispatched back into this same router, so the documents it reads are the documents this suite
/// wrote.
async fn drive(rows: &[Row]) {
    let router = build_router();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let kind = row.kind;
        let bucket = format!("s3-transport-clause-{}-{index}", kind.noun());
        create_bucket(&router, case, &bucket).await;
        let name = format!("s3-transport-clause-{}-{index}", kind.noun());
        let arn = match kind {
            Kind::Queue => queue(&router, case, &name, row.form, &bucket).await.1,
            Kind::Topic => topic(&router, case, &name, row.form, &bucket).await,
        };

        let container = [configuration(kind, "the-destination", &arn, "ev/")];
        let answered = put_notification(&router, &bucket, &container, row.skip_validation).await;
        if row.recorded {
            assert_eq!(
                answered.status, 200,
                "case {case:?} on a {}: expected the container to be recorded with HTTP 200, got \
                 {} with {}",
                kind.noun(),
                answered.status,
                answered.body
            );
        } else {
            assert_eq!(
                answered.status, INVALID_ARGUMENT_STATUS,
                "case {case:?} on a {}: expected the container to be refused HTTP \
                 {INVALID_ARGUMENT_STATUS}, got {} with {}",
                kind.noun(),
                answered.status,
                answered.body
            );
            assert_eq!(
                element(&answered.body, "Code"),
                INVALID_ARGUMENT,
                "case {case:?} on a {}: expected the error code {INVALID_ARGUMENT:?}, got {}",
                kind.noun(),
                answered.body
            );
            assert_eq!(
                element(&answered.body, "Message"),
                UNABLE_TO_VALIDATE,
                "case {case:?} on a {}: expected the message {UNABLE_TO_VALIDATE:?}, got {}",
                kind.noun(),
                answered.body
            );
            assert_eq!(
                (
                    opt_element(&answered.body, "ArgumentName1"),
                    opt_element(&answered.body, "ArgumentValue1"),
                ),
                (Some(arn.clone()), Some(kind.unpermitted_detail())),
                "case {case:?} on a {}: expected the refusal to name the destination and its \
                 reason, got {}",
                kind.noun(),
                answered.body
            );
        }

        // What a READ reports: the destination where the container was recorded, and nothing at
        // all where it was refused — the put is atomic, so a refused one records nothing.
        let held = reported(&router, case, &bucket, kind).await;
        let expected: Vec<String> = if row.recorded {
            vec![arn.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(
            held, expected,
            "case {case:?} on a {}: expected the bucket to report {expected:?}, got {held:?}",
            kind.noun()
        );
    }
}

/// The rows whose container is recorded.
fn recorded_rows() -> Vec<Row> {
    rows().into_iter().filter(|row| row.recorded).collect()
}

/// The rows whose container is refused.
fn refused_rows() -> Vec<Row> {
    rows().into_iter().filter(|row| !row.recorded).collect()
}

#[test]
fn a_destination_policy_whose_clause_admits_the_publication_is_recorded() {
    driven("s3-transport-clause-admissions", || {
        Box::pin(async {
            drive(&recorded_rows()).await;
        })
    });
}

#[test]
fn a_destination_policy_whose_clause_withholds_the_publication_is_refused() {
    driven("s3-transport-clause-refusals", || {
        Box::pin(async {
            drive(&refused_rows()).await;
        })
    });
}

#[test]
fn destinations_behind_the_tls_guardrail_are_validated_and_then_published_to() {
    driven("s3-transport-clause-delivery", || {
        Box::pin(async {
            let case = "a queue and a topic each granting the publication behind the TLS guardrail";
            let router = build_router();
            let bucket = "s3-transport-clause-pipeline";
            create_bucket(&router, case, bucket).await;
            let (queue_url, queue_arn) = queue(
                &router,
                case,
                "s3-transport-clause-pipeline-queue",
                Form::GrantBehindTheTlsGuardrail,
                bucket,
            )
            .await;
            let topic_arn = topic(
                &router,
                case,
                "s3-transport-clause-pipeline-topic",
                Form::GrantBehindTheTlsGuardrail,
                bucket,
            )
            .await;

            // One container naming both destinations, on key-name prefixes that overlap in
            // nothing — so the two configurations are told apart and no ambiguity refusal can be
            // what fires.
            let container = [
                configuration(Kind::Queue, "QueueNotification", &queue_arn, "CategoryA/"),
                configuration(Kind::Topic, "TopicNotification", &topic_arn, "CategoryB/"),
            ];
            let answered = put_notification(&router, bucket, &container, false).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: the container must be recorded with HTTP 200, got {} with {}",
                answered.status, answered.body
            );
            let queues = reported(&router, case, bucket, Kind::Queue).await;
            let topics = reported(&router, case, bucket, Kind::Topic).await;
            assert_eq!(
                (queues.as_slice(), topics.as_slice()),
                (
                    [queue_arn.clone()].as_slice(),
                    [topic_arn.clone()].as_slice()
                ),
                "case {case:?}: expected the bucket to report both destinations, got {queues:?} \
                 and {topics:?}"
            );

            // Validation published the queue the test notification, which is what a consumer of
            // a validated destination finds waiting on it …
            let seen = awaited(&router, case, &queue_url, |body| body.contains(TEST_EVENT)).await;
            assert!(
                seen.iter().any(|body| body.contains(TEST_EVENT)),
                "case {case:?}: installing the container must send the validated queue the \
                 {TEST_EVENT} probe, saw {seen:?}"
            );

            // … and the configuration in force then publishes the object event to it: the same
            // grant, behind the same guardrail, decides the publication too.
            let key = "CategoryA/the-object.txt";
            put_object(&router, case, bucket, key).await;
            let bodies = awaited(&router, case, &queue_url, |body| body.contains(key)).await;
            let keys: Vec<String> = bodies
                .iter()
                .filter_map(|text| serde_json::from_str::<Value>(text).ok())
                .flat_map(|parsed| parsed["Records"].as_array().cloned().unwrap_or_default())
                .map(|record| {
                    record["s3"]["object"]["key"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            assert!(
                keys.iter().any(|named| named == key),
                "case {case:?}: the queue must take the record for {key:?}, took {keys:?}"
            );
        })
    });
}
