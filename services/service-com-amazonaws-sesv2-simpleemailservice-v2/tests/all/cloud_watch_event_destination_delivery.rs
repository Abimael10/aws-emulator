// SPDX-License-Identifier: CC0-1.0

//! What a configuration set's CloudWatch event destination DELIVERS: the `AWS/SES`
//! datapoints one accepted send leaves behind, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service
//! and Amazon CloudWatch mounted on ONE engine, which is what makes the store a send
//! publishes into the store CloudWatch reads — and asserts the metric where a client reads
//! it: `cloudwatch:GetMetricStatistics` and `cloudwatch:ListMetrics`.
//!
//! The RULE these cases pin: for each event an accepted send produces, every event
//! destination of the configuration set the message named that is `Enabled`, whose
//! `MatchingEventTypes` names that event's type and that carries a `CloudWatchDestination`
//! counts the event as ONE observation of the `AWS/SES` metric named after that event,
//! dimensioned by EXACTLY the dimensions the destination declared — each one resolved from
//! the send by its own `CloudWatchDimensionConfiguration$DimensionValueSource` ("the
//! location where the Amazon SES API v2 finds the value of a dimension": a message tag of
//! the send, which is one the send's own parameter supplies, one the message declares in
//! its `X-SES-MESSAGE-TAGS` header, or the configuration set SES tags every send through a
//! set with, or a header of the message), falling back to that configuration's
//! `DefaultDimensionValue` where the send supplies no value for it — and by nothing else,
//! so a dimension combination the destination never declared names no series at all.
//!
//! The table spans what the rule generalises over: how many dimensions a destination
//! declares, which `DimensionValueSource` each one names (every value of that enum,
//! `smithy_query(query="shape",shape="DimensionValueSource")`), where a message tag was
//! written, whether the send supplies a
//! value for the dimension, the case a header name is spelled in, whether the header is one
//! the caller supplied or one SES assembled, which event types the destination matches,
//! whether the destination is on at all, and which events the send's recipient produces
//! (acceptance only, a delivery, a failure, a complaint). The recorded case — the set, a
//! custom message tag and a custom MIME header, matching `SEND` — is one row of it.
//!
//! The rule holds for every FORM an accepted send takes, so the cases below drive each one
//! that carries a message: a simple message, whose headers are members of the request; a
//! raw message, which IS a MIME document and writes its headers inside it; and a bulk send,
//! which composes one message per entry and resolves each message's dimensions from what is
//! that entry's own.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::ComAmazonawsSesv2SimpleemailserviceV2State;
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The sender every send is from, and the domain identity that authorises it.
const SENDER: &str = "sender@senders.example.com";
const SENDER_DOMAIN: &str = "senders.example.com";

/// The subject every send carries, which is also a header of the message SES assembles —
/// so a declared `EMAIL_HEADER` dimension named `Subject` resolves from it.
const SUBJECT: &str = "a message under test";

/// The recipient whose mailbox accepts a message, so the send produces the acceptance and
/// a delivery and no failure.
const ACCEPTING_RECIPIENT: &str = "success@simulator.amazonses.com";

/// The stored template a bulk send composes its messages from.
const TEMPLATE: &str = "receipt";

/// The namespace Amazon SES publishes a configuration set's email events to.
const NAMESPACE: &str = "AWS/SES";

/// The `x-amz-target` prefix CloudWatch's `awsJson1_0` surface serves (its
/// `OPERATIONS.md` "Wire protocol").
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The period a read lays over the series, in seconds: one minute, the resolution these
/// datapoints are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of the send a read's window reaches, in seconds. Generous on
/// purpose: the window is not what any case is about.
const WINDOW_SECS: f64 = 600.0;

/// How many times a read is repeated while waiting for the series a row expects to appear.
///
/// An email event is published ASYNCHRONOUSLY — the message is accepted and answered before
/// any event of it exists — so the series appears on a task of its own. Each pass reads the
/// store again and yields to the runtime; nothing here waits for a SPAN, and the loop stops
/// as soon as what it is waiting for is there.
const READS_WHILE_PUBLISHING: usize = 2_000;

/// How many further reads confirm that what the store holds has settled — so a row that
/// expects one series, or none at all, would also see one it must not have.
const SETTLED_READS: usize = 32;

/// One dimension a destination declares: the name, its `DimensionValueSource` and the
/// default value the configuration carries for a send that supplies none.
#[derive(Clone, Copy)]
struct Dimension {
    name: &'static str,
    source: &'static str,
    default: &'static str,
}

/// One series the `AWS/SES` namespace must hold: the metric name and the dimension set,
/// written out per row rather than derived, so the expectation does not come from the
/// behaviour under test.
#[derive(Clone, Copy)]
struct Series {
    metric: &'static str,
    dimensions: &'static [(&'static str, &'static str)],
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The configuration set the row creates and sends through.
    set: &'static str,
    /// `EventDestinationDefinition$Enabled`.
    enabled: bool,
    /// The event types the destination matches, as the definition names them.
    matching: &'static [&'static str],
    /// The dimensions the destination declares.
    dimensions: &'static [Dimension],
    /// `SendEmailRequest$EmailTags`.
    email_tags: &'static [(&'static str, &'static str)],
    /// `Message$Headers`: the headers the send adds to the message.
    headers: &'static [(&'static str, &'static str)],
    /// The one recipient of the send, whose reaction decides which events it produces.
    recipient: &'static str,
    /// Exactly the series the send must leave in `AWS/SES`, each counting ONE event.
    expected: &'static [Series],
}

/// Every value of the `EventType` enum
/// (`smithy_query(query="shape",shape="EventType")`).
const EVERY_EVENT_TYPE: &[&str] = &[
    "BOUNCE",
    "CLICK",
    "COMPLAINT",
    "DELIVERY",
    "DELIVERY_DELAY",
    "OPEN",
    "REJECT",
    "RENDERING_FAILURE",
    "SEND",
    "SUBSCRIPTION",
];

/// The message tag SES applies to every send made through a configuration set.
const CONFIGURATION_SET_TAG: &str = "ses:configuration-set";

const ROWS: &[Row] = &[
    Row {
        case: "the set, a custom message tag and a custom MIME header, matching SEND",
        set: "cw-events",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[
            Dimension {
                name: CONFIGURATION_SET_TAG,
                source: "MESSAGE_TAG",
                default: "none",
            },
            Dimension {
                name: "campaign",
                source: "MESSAGE_TAG",
                default: "none",
            },
            Dimension {
                name: "X-Custom-Header",
                source: "EMAIL_HEADER",
                default: "none",
            },
        ],
        email_tags: &[("campaign", "spring-launch")],
        headers: &[("X-Custom-Header", "header-of-this-send")],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                (CONFIGURATION_SET_TAG, "cw-events"),
                ("campaign", "spring-launch"),
                ("X-Custom-Header", "header-of-this-send"),
            ],
        }],
    },
    Row {
        case: "a destination that declares only its own dimensions publishes no set \
               dimension",
        set: "custom_only_set",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[
            Dimension {
                name: "campaign",
                source: "MESSAGE_TAG",
                default: "none",
            },
            Dimension {
                name: "X-Custom-Header",
                source: "EMAIL_HEADER",
                default: "none",
            },
        ],
        email_tags: &[("campaign", "summer-sale")],
        headers: &[("X-Custom-Header", "second-header")],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                ("campaign", "summer-sale"),
                ("X-Custom-Header", "second-header"),
            ],
        }],
    },
    Row {
        case: "one declared dimension, and the send supplies its tag",
        set: "s",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[Dimension {
            name: "tier",
            source: "MESSAGE_TAG",
            default: "standard",
        }],
        email_tags: &[("tier", "premium")],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[("tier", "premium")],
        }],
    },
    Row {
        case: "a declared message tag the send does not supply carries its default",
        set: "defaulted-tag-set",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[
            Dimension {
                name: "campaign",
                source: "MESSAGE_TAG",
                default: "none",
            },
            Dimension {
                name: "tier",
                source: "MESSAGE_TAG",
                default: "unset",
            },
        ],
        email_tags: &[("campaign", "autumn-push")],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[("campaign", "autumn-push"), ("tier", "unset")],
        }],
    },
    Row {
        case: "a declared message tag the message declares in its own X-SES-MESSAGE-TAGS \
               header",
        set: "header-declared-tag-set",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[
            Dimension {
                name: "campaign",
                source: "MESSAGE_TAG",
                default: "none",
            },
            Dimension {
                name: "tier",
                source: "MESSAGE_TAG",
                default: "unset",
            },
        ],
        email_tags: &[("campaign", "from-the-parameter")],
        // A message tag is one the send's parameter supplies or one this header declares,
        // as a comma-separated list of name=value pairs. The header declares a tag the
        // destination declared a dimension for and one it did not.
        headers: &[("X-SES-MESSAGE-TAGS", "tier=from-the-header, region=eu-west-1")],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                ("campaign", "from-the-parameter"),
                ("tier", "from-the-header"),
            ],
        }],
    },
    Row {
        case: "a tag the send supplies by parameter decides a dimension a tag of the same \
               name in the header would otherwise",
        set: "tag-precedence-set",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[Dimension {
            name: "campaign",
            source: "MESSAGE_TAG",
            default: "none",
        }],
        email_tags: &[("campaign", "from-the-parameter")],
        // Both sources are message tags and AWS documents no order between them, so which
        // one decides the dimension is this emulator's resolution: the tags apply in the
        // order the send states them, and a dimension takes the first value its name
        // carries — which is the parameter's.
        headers: &[("X-SES-MESSAGE-TAGS", "campaign=from-the-header")],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[("campaign", "from-the-parameter")],
        }],
    },
    Row {
        case: "a header dimension resolves whatever case the send spelled the header in, \
               and an absent header carries its default",
        set: "header-case-set",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[
            Dimension {
                name: "X-Custom-Header",
                source: "EMAIL_HEADER",
                default: "none",
            },
            Dimension {
                name: "X-Absent-Header",
                source: "EMAIL_HEADER",
                default: "absent",
            },
        ],
        email_tags: &[],
        headers: &[("x-CUSTOM-header", "spelled-in-another-case")],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                ("X-Absent-Header", "absent"),
                ("X-Custom-Header", "spelled-in-another-case"),
            ],
        }],
    },
    Row {
        case: "a header dimension naming a header SES assembled for the message",
        set: "assembled-header-set",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[Dimension {
            name: "Subject",
            source: "EMAIL_HEADER",
            default: "no-subject",
        }],
        email_tags: &[],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[("Subject", SUBJECT)],
        }],
    },
    Row {
        case: "a LINK_TAG dimension carries its default, since no link of this send was \
               followed",
        set: "link-tag-set",
        enabled: true,
        matching: &["SEND"],
        dimensions: &[
            Dimension {
                name: CONFIGURATION_SET_TAG,
                source: "MESSAGE_TAG",
                default: "none",
            },
            Dimension {
                name: "link-id",
                source: "LINK_TAG",
                default: "no-link",
            },
        ],
        email_tags: &[],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                (CONFIGURATION_SET_TAG, "link-tag-set"),
                ("link-id", "no-link"),
            ],
        }],
    },
    Row {
        case: "a destination matching only DELIVERY counts the delivery and not the \
               acceptance",
        set: "delivery-only-set",
        enabled: true,
        matching: &["DELIVERY"],
        dimensions: &[Dimension {
            name: CONFIGURATION_SET_TAG,
            source: "MESSAGE_TAG",
            default: "none",
        }],
        email_tags: &[],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Delivery",
            dimensions: &[(CONFIGURATION_SET_TAG, "delivery-only-set")],
        }],
    },
    Row {
        case: "a destination matching every event type, for a recipient that rejects the \
               message",
        set: "every-type-bounce-set",
        enabled: true,
        matching: EVERY_EVENT_TYPE,
        dimensions: &[Dimension {
            name: "campaign",
            source: "MESSAGE_TAG",
            default: "none",
        }],
        email_tags: &[("campaign", "winter-tease")],
        headers: &[],
        recipient: "bounce@simulator.amazonses.com",
        expected: &[
            Series {
                metric: "Bounce",
                dimensions: &[("campaign", "winter-tease")],
            },
            Series {
                metric: "Send",
                dimensions: &[("campaign", "winter-tease")],
            },
        ],
    },
    Row {
        case: "a destination matching every event type, for a recipient that reports the \
               message",
        set: "every-type-complaint-set",
        enabled: true,
        matching: EVERY_EVENT_TYPE,
        dimensions: &[Dimension {
            name: CONFIGURATION_SET_TAG,
            source: "MESSAGE_TAG",
            default: "none",
        }],
        email_tags: &[],
        headers: &[],
        recipient: "complaint@simulator.amazonses.com",
        expected: &[
            Series {
                metric: "Complaint",
                dimensions: &[(CONFIGURATION_SET_TAG, "every-type-complaint-set")],
            },
            Series {
                metric: "Delivery",
                dimensions: &[(CONFIGURATION_SET_TAG, "every-type-complaint-set")],
            },
            Series {
                metric: "Send",
                dimensions: &[(CONFIGURATION_SET_TAG, "every-type-complaint-set")],
            },
        ],
    },
    Row {
        case: "a destination that is off counts nothing",
        set: "switched-off-set",
        enabled: false,
        matching: &["SEND", "DELIVERY"],
        dimensions: &[Dimension {
            name: CONFIGURATION_SET_TAG,
            source: "MESSAGE_TAG",
            default: "none",
        }],
        email_tags: &[],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[],
    },
    Row {
        case: "a destination matching an event type this send never produces counts \
               nothing",
        set: "unmatched-type-set",
        enabled: true,
        matching: &["COMPLAINT", "OPEN", "CLICK"],
        dimensions: &[Dimension {
            name: CONFIGURATION_SET_TAG,
            source: "MESSAGE_TAG",
            default: "none",
        }],
        email_tags: &[],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[],
    },
];

/// One answered request: its status and parsed body.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// A string at `pointer`, or `""` when the body carries nothing there.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: this service and CloudWatch on ONE engine, the shape
/// `core/router/src/lib.rs` mounts every service in.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// One call on this service's restJson1 surface.
    async fn ses(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://email.{REGION}.amazonaws.com{path}"))
            .header("host", format!("email.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIAEMULATOR0000000/20260101/{REGION}/ses/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    /// The same call, requiring the 200 the operation's output shape declares.
    async fn ses_ok(&self, case: &str, method: &str, path: &str, body: Option<Value>) -> Reply {
        let reply = self.ses(method, path, body).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: {method} {path} must be served, got {} {}",
            reply.status, reply.body
        );
        reply
    }

    /// One CloudWatch call on its `awsJson1_0` surface, requiring a 200.
    async fn cloudwatch(&self, case: &str, operation: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("monitoring.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.0")
            .header(
                "x-amz-target",
                format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
            )
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIAEMULATOR0000000/20260101/{REGION}/monitoring/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("serialize body"),
            ))
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed: Value = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap_or(Value::Null)
        };
        assert_eq!(
            status, 200,
            "case {case:?}: cloudwatch:{operation} must be served, got {status} {parsed}"
        );
        parsed
    }

    /// The account with a verified sending domain, so every send below is authorised.
    async fn given_verified_sender(&self, case: &str) {
        self.ses_ok(
            case,
            "POST",
            "/v2/email/identities",
            Some(json!({"EmailIdentity": SENDER_DOMAIN})),
        )
        .await;
        // Bring the verification transition the clock owes the identity into the past, so
        // a send does not race the span.
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let past = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .expect("an instant before now");
        for record in store.identities.values_mut() {
            record.verification_due_at = past;
        }
        store.settle(SystemTime::now());
    }

    /// A configuration set carrying one CloudWatch event destination.
    async fn given_destination(
        &self,
        case: &str,
        set: &str,
        enabled: bool,
        matching: &[&str],
        dimensions: &[Dimension],
    ) {
        self.ses_ok(
            case,
            "POST",
            "/v2/email/configuration-sets",
            Some(json!({"ConfigurationSetName": set})),
        )
        .await;
        let configurations: Vec<Value> = dimensions
            .iter()
            .map(|dimension| {
                json!({
                    "DimensionName": dimension.name,
                    "DimensionValueSource": dimension.source,
                    "DefaultDimensionValue": dimension.default,
                })
            })
            .collect();
        self.ses_ok(
            case,
            "POST",
            &format!("/v2/email/configuration-sets/{set}/event-destinations"),
            Some(json!({
                "EventDestinationName": "sim2real-cw",
                "EventDestination": {
                    "Enabled": enabled,
                    "MatchingEventTypes": matching,
                    "CloudWatchDestination": {"DimensionConfigurations": configurations},
                },
            })),
        )
        .await;
    }

    /// `SendEmail` through `set`, answering the `MessageId` it minted.
    async fn send(
        &self,
        case: &str,
        set: &str,
        recipient: &str,
        email_tags: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> String {
        let mut simple = json!({
            "Subject": {"Data": SUBJECT},
            "Body": {"Text": {"Data": "hello"}},
        });
        if !headers.is_empty() {
            simple["Headers"] = Value::Array(
                headers
                    .iter()
                    .map(|(name, value)| json!({"Name": name, "Value": value}))
                    .collect(),
            );
        }
        let request = json!({
            "FromEmailAddress": SENDER,
            "Destination": {"ToAddresses": [recipient]},
            "ConfigurationSetName": set,
            "Content": {"Simple": simple},
            "EmailTags": email_tags
                .iter()
                .map(|(name, value)| json!({"Name": name, "Value": value}))
                .collect::<Vec<Value>>(),
        });
        let reply = self
            .ses_ok(case, "POST", "/v2/email/outbound-emails", Some(request))
            .await;
        let message_id = reply.text("/MessageId");
        assert!(
            !message_id.is_empty(),
            "case {case:?}: an accepted send must answer a MessageId, got {}",
            reply.body
        );
        message_id
    }

    /// `SendEmail` of a RAW MIME document through `set`, answering the `MessageId` it
    /// minted.
    ///
    /// `Content$Raw` carries the document as a blob, which restJson1 spells base64, and a
    /// raw message writes its headers INSIDE that document rather than as members of the
    /// request — so a declared `EMAIL_HEADER` dimension of such a send resolves from the
    /// document's own header block.
    async fn send_raw(
        &self,
        case: &str,
        set: &str,
        recipient: &str,
        email_tags: &[(&str, &str)],
        document: &str,
    ) -> String {
        let request = json!({
            "FromEmailAddress": SENDER,
            "Destination": {"ToAddresses": [recipient]},
            "ConfigurationSetName": set,
            "Content": {"Raw": {"Data": ::aws_smithy_types::base64::encode(document)}},
            "EmailTags": email_tags
                .iter()
                .map(|(name, value)| json!({"Name": name, "Value": value}))
                .collect::<Vec<Value>>(),
        });
        let reply = self
            .ses_ok(case, "POST", "/v2/email/outbound-emails", Some(request))
            .await;
        let message_id = reply.text("/MessageId");
        assert!(
            !message_id.is_empty(),
            "case {case:?}: an accepted raw send must answer a MessageId, got {}",
            reply.body
        );
        message_id
    }

    /// `CreateEmailTemplate`: the one stored template a bulk send composes each of its
    /// messages from.
    async fn given_template(&self, case: &str) {
        self.ses_ok(
            case,
            "POST",
            "/v2/email/templates",
            Some(json!({
                "TemplateName": TEMPLATE,
                "TemplateContent": {"Subject": SUBJECT, "Text": "hello"},
            })),
        )
        .await;
    }

    /// `SendBulkEmail` through `set`, answering the `(Status, MessageId)` of every entry
    /// result in the order the response reported them.
    async fn send_bulk(
        &self,
        case: &str,
        set: &str,
        default_tags: &[(&str, &str)],
        entries: Vec<Value>,
    ) -> Vec<(String, String)> {
        let request = json!({
            "FromEmailAddress": SENDER,
            "ConfigurationSetName": set,
            "DefaultContent": {"Template": {
                "TemplateName": TEMPLATE,
                "TemplateData": "{}",
            }},
            "DefaultEmailTags": default_tags
                .iter()
                .map(|(name, value)| json!({"Name": name, "Value": value}))
                .collect::<Vec<Value>>(),
            "BulkEmailEntries": entries,
        });
        let reply = self
            .ses_ok(
                case,
                "POST",
                "/v2/email/outbound-bulk-emails",
                Some(request),
            )
            .await;
        reply
            .body
            .get("BulkEmailEntryResults")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|result| (text(result, "Status"), text(result, "MessageId")))
            .collect()
    }

    /// Every series the `AWS/SES` namespace holds, as `ListMetrics` reports them: each the
    /// metric name and its dimension set, both ordered so a comparison is stable.
    async fn listed_series(&self, case: &str) -> Vec<(String, Vec<(String, String)>)> {
        let answered = self
            .cloudwatch(case, "ListMetrics", &json!({"Namespace": NAMESPACE}))
            .await;
        let mut series: Vec<(String, Vec<(String, String)>)> = answered
            .get("Metrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|metric| (text(metric, "MetricName"), dimensions_of(metric)))
            .collect();
        series.sort();
        series
    }

    /// The datapoints `GetMetricStatistics` answers for the series `metric`/`dimensions`
    /// over the window around the send.
    async fn datapoints(
        &self,
        case: &str,
        metric: &str,
        dimensions: &[(&str, &str)],
    ) -> Vec<Value> {
        let now = now_secs();
        let answered = self
            .cloudwatch(
                case,
                "GetMetricStatistics",
                &json!({
                    "Namespace": NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": dimensions
                        .iter()
                        .map(|(name, value)| json!({"Name": name, "Value": value}))
                        .collect::<Vec<Value>>(),
                    "StartTime": now - WINDOW_SECS,
                    "EndTime": now + WINDOW_SECS,
                    "Period": PERIOD_SECS,
                    "Statistics": ["Sum"],
                }),
            )
            .await;
        answered
            .get("Datapoints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The `Sum` over every datapoint of one series' window: how many events it counted.
    async fn sum(&self, case: &str, metric: &str, dimensions: &[(&str, &str)]) -> f64 {
        self.datapoints(case, metric, dimensions)
            .await
            .iter()
            .filter_map(|point| point.get("Sum").and_then(Value::as_f64))
            .sum()
    }
}

/// A string member of a reported metric, or `""` when absent.
fn text(metric: &Value, member: &str) -> String {
    metric
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The dimension set of a reported metric, ordered by name.
fn dimensions_of(metric: &Value) -> Vec<(String, String)> {
    let mut dimensions: Vec<(String, String)> = metric
        .get("Dimensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|dimension| (text(dimension, "Name"), text(dimension, "Value")))
        .collect();
    dimensions.sort();
    dimensions
}

/// Wall-clock now as epoch seconds, the clock a metric window is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The series a row expects, in the order [`Emulator::listed_series`] reports them.
fn expected_series(row: &Row) -> Vec<(String, Vec<(String, String)>)> {
    expected_of(row.expected)
}

/// The same, for a set of series stated on its own.
fn expected_of(expected: &[Series]) -> Vec<(String, Vec<(String, String)>)> {
    let mut series: Vec<(String, Vec<(String, String)>)> = expected
        .iter()
        .map(|wanted| {
            let mut dimensions: Vec<(String, String)> = wanted
                .dimensions
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
            dimensions.sort();
            (wanted.metric.to_string(), dimensions)
        })
        .collect();
    series.sort();
    series
}

/// Read the namespace until the series `row` expects have all appeared, and then until
/// what it holds has settled — so a row that expects fewer series than the store ends up
/// holding fails on the comparison rather than on a race.
async fn settled_series(
    emulator: &Emulator,
    row: &Row,
) -> Vec<(String, Vec<(String, String)>)> {
    settled_count(emulator, row.case, row.expected.len()).await
}

/// The same, for a case that expects `wanted` series rather than a whole row.
async fn settled_count(
    emulator: &Emulator,
    case: &str,
    wanted: usize,
) -> Vec<(String, Vec<(String, String)>)> {
    let mut listed = emulator.listed_series(case).await;
    for _ in 0..READS_WHILE_PUBLISHING {
        if listed.len() >= wanted {
            break;
        }
        ::tokio::task::yield_now().await;
        listed = emulator.listed_series(case).await;
    }
    for _ in 0..SETTLED_READS {
        ::tokio::task::yield_now().await;
        let again = emulator.listed_series(case).await;
        assert_eq!(
            again, listed,
            "case {case:?}: the AWS/SES namespace kept changing: {listed:?} then {again:?}"
        );
    }
    listed
}

#[tokio::test]
async fn a_send_counts_one_datapoint_per_event_under_exactly_the_declared_dimensions() {
    for row in ROWS {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_verified_sender(case).await;
        emulator
            .given_destination(case, row.set, row.enabled, row.matching, row.dimensions)
            .await;

        // Nothing is in the namespace before the send: the destination alone publishes
        // nothing.
        assert!(
            emulator.listed_series(case).await.is_empty(),
            "case {case:?}: attaching a destination must publish no datapoint, got {:?}",
            emulator.listed_series(case).await
        );

        emulator
            .send(case, row.set, row.recipient, row.email_tags, row.headers)
            .await;

        let listed = settled_series(&emulator, row).await;
        assert_eq!(
            listed,
            expected_series(row),
            "case {case:?}: expected exactly the declared series in AWS/SES, got {listed:?}"
        );

        // Each of them counts the ONE event it is about.
        for wanted in row.expected {
            let points = emulator
                .datapoints(case, wanted.metric, wanted.dimensions)
                .await;
            assert_eq!(
                points.len(),
                1,
                "case {case:?}: expected one {:?} datapoint for {:?}, got {points:?}",
                wanted.metric,
                wanted.dimensions
            );
            let sum = emulator.sum(case, wanted.metric, wanted.dimensions).await;
            assert_eq!(
                sum, 1.0,
                "case {case:?}: expected Sum 1 for {:?} over {:?}, got {sum}",
                wanted.metric, wanted.dimensions
            );
        }
    }
}

#[tokio::test]
async fn a_dimension_the_destination_did_not_declare_has_no_series_at_all() {
    // The mirror image of the positive half: a destination that declares only its own
    // dimensions publishes a series for exactly those, and a read that adds a dimension it
    // never declared — the configuration set SES tags every send with, or a name nothing
    // configured — finds NO series rather than one defaulted to its `DefaultDimensionValue`.
    let case = "a destination declaring only its own dimensions";
    let set = "undeclared-probe-set";
    let declared: &[(&str, &str)] = &[("campaign", "probe-campaign")];
    let emulator = Emulator::new();
    emulator.given_verified_sender(case).await;
    emulator
        .given_destination(
            case,
            set,
            true,
            &["SEND"],
            &[Dimension {
                name: "campaign",
                source: "MESSAGE_TAG",
                default: "none",
            }],
        )
        .await;
    emulator
        .send(
            case,
            set,
            "success@simulator.amazonses.com",
            &[("campaign", "probe-campaign")],
            &[],
        )
        .await;

    let row = Row {
        case,
        set,
        enabled: true,
        matching: &["SEND"],
        dimensions: &[],
        email_tags: &[],
        headers: &[],
        recipient: ACCEPTING_RECIPIENT,
        expected: &[Series {
            metric: "Send",
            dimensions: &[("campaign", "probe-campaign")],
        }],
    };
    let listed = settled_series(&emulator, &row).await;
    assert_eq!(
        listed,
        expected_series(&row),
        "case {case:?}: expected exactly the declared series, got {listed:?}"
    );

    // The declared combination is there and counts the send.
    let sum = emulator.sum(case, "Send", declared).await;
    assert_eq!(
        sum, 1.0,
        "case {case:?}: expected Sum 1 for the declared dimensions, got {sum}"
    );

    // Every combination the destination did not declare has no datapoint at all, and an
    // absence stays an absence over further reads.
    let undeclared: [&[(&str, &str)]; 3] = [
        &[("campaign", "probe-campaign"), (CONFIGURATION_SET_TAG, set)],
        &[(CONFIGURATION_SET_TAG, set)],
        &[("campaign", "probe-campaign"), ("tier", "none")],
    ];
    for probe in undeclared {
        for _ in 0..SETTLED_READS {
            let points = emulator.datapoints(case, "Send", probe).await;
            assert!(
                points.is_empty(),
                "case {case:?}: {probe:?} is not a combination this destination declared, so \
                 it must name no series, got {points:?}"
            );
            ::tokio::task::yield_now().await;
        }
    }
}

#[tokio::test]
async fn each_send_counts_once_more_under_the_same_dimensions() {
    // One observation per EVENT: three sends through the same destination with the same
    // tags count three, in the one series their dimension set identifies.
    let case = "three sends through one destination";
    let set = "repeated-sends-set";
    let dimensions: &[(&str, &str)] = &[(CONFIGURATION_SET_TAG, "repeated-sends-set")];
    let sends = 3;
    let emulator = Emulator::new();
    emulator.given_verified_sender(case).await;
    emulator
        .given_destination(
            case,
            set,
            true,
            &["SEND"],
            &[Dimension {
                name: CONFIGURATION_SET_TAG,
                source: "MESSAGE_TAG",
                default: "none",
            }],
        )
        .await;
    for _ in 0..sends {
        emulator
            .send(case, set, "success@simulator.amazonses.com", &[], &[])
            .await;
    }

    let mut counted = 0.0;
    for _ in 0..READS_WHILE_PUBLISHING {
        counted = emulator.sum(case, "Send", dimensions).await;
        if counted >= f64::from(sends) {
            break;
        }
        ::tokio::task::yield_now().await;
    }
    assert_eq!(
        counted,
        f64::from(sends),
        "case {case:?}: expected the series to count every send, got {counted}"
    );

    // Reading the series does not change it.
    let read_again = emulator.sum(case, "Send", dimensions).await;
    assert_eq!(
        read_again, counted,
        "case {case:?}: a second read counted {read_again}, not the first read's {counted}"
    );
}

/// What every raw-content row's destination declares: the configuration set SES tags every
/// send through it with, a tag of the send, and a header of the message — the same three
/// kinds of dimension the recorded case declares, resolved for a send whose message is a
/// MIME document rather than members of the request.
const RAW_DIMENSIONS: &[Dimension] = &[
    Dimension {
        name: CONFIGURATION_SET_TAG,
        source: "MESSAGE_TAG",
        default: "none",
    },
    Dimension {
        name: "campaign",
        source: "MESSAGE_TAG",
        default: "none",
    },
    Dimension {
        name: "X-Custom-Header",
        source: "EMAIL_HEADER",
        default: "none",
    },
];

/// One raw-content row: the MIME document the send carries, the tags it applies, and
/// exactly the series it must leave in the namespace.
struct RawRow {
    case: &'static str,
    /// The configuration set the row creates and sends through.
    set: &'static str,
    /// `RawMessage$Data`: the document the send IS.
    document: &'static str,
    /// `SendEmailRequest$EmailTags`.
    email_tags: &'static [(&'static str, &'static str)],
    /// Exactly the series the send must leave in `AWS/SES`.
    expected: &'static [Series],
}

/// The rows differ along where the declared header dimension's value comes from in a raw
/// document: a header written as the dimension names it, one written in another case, one
/// folded over two lines the way RFC 5322 allows, and none at all.
const RAW_ROWS: &[RawRow] = &[
    RawRow {
        case: "a raw document whose header block carries the declared header",
        set: "raw-header-set",
        document: concat!(
            "From: sender@senders.example.com\r\n",
            "To: success@simulator.amazonses.com\r\n",
            "Subject: a raw message under test\r\n",
            "X-Custom-Header: header-of-this-document\r\n",
            "\r\n",
            "hello from a raw document\r\n",
        ),
        email_tags: &[("campaign", "raw-launch")],
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                (CONFIGURATION_SET_TAG, "raw-header-set"),
                ("campaign", "raw-launch"),
                ("X-Custom-Header", "header-of-this-document"),
            ],
        }],
    },
    RawRow {
        case: "a raw document spelling the declared header in another case",
        set: "raw-header-case-set",
        document: concat!(
            "From: sender@senders.example.com\r\n",
            "To: success@simulator.amazonses.com\r\n",
            "Subject: a raw message under test\r\n",
            "x-CUSTOM-header: spelled-in-another-case\r\n",
            "\r\n",
            "hello from a raw document\r\n",
        ),
        email_tags: &[("campaign", "raw-case")],
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                (CONFIGURATION_SET_TAG, "raw-header-case-set"),
                ("campaign", "raw-case"),
                ("X-Custom-Header", "spelled-in-another-case"),
            ],
        }],
    },
    RawRow {
        case: "a raw document folding the declared header over two lines",
        set: "raw-folded-header-set",
        document: concat!(
            "From: sender@senders.example.com\r\n",
            "To: success@simulator.amazonses.com\r\n",
            "Subject: a raw message under test\r\n",
            "X-Custom-Header: folded\r\n",
            " over-two-lines\r\n",
            "\r\n",
            "hello from a raw document\r\n",
        ),
        email_tags: &[("campaign", "raw-folded")],
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                (CONFIGURATION_SET_TAG, "raw-folded-header-set"),
                ("campaign", "raw-folded"),
                ("X-Custom-Header", "folded over-two-lines"),
            ],
        }],
    },
    RawRow {
        case: "a raw document declaring its message tags in its own X-SES-MESSAGE-TAGS \
               header",
        set: "raw-tag-header-set",
        document: concat!(
            "From: sender@senders.example.com\r\n",
            "To: success@simulator.amazonses.com\r\n",
            "Subject: a raw message under test\r\n",
            "X-SES-MESSAGE-TAGS: campaign=declared-in-the-document\r\n",
            "X-Custom-Header: header-of-this-document\r\n",
            "\r\n",
            "hello from a raw document\r\n",
        ),
        email_tags: &[],
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                (CONFIGURATION_SET_TAG, "raw-tag-header-set"),
                ("campaign", "declared-in-the-document"),
                ("X-Custom-Header", "header-of-this-document"),
            ],
        }],
    },
    RawRow {
        case: "a raw document carrying no such header, so the dimension carries its default",
        set: "raw-absent-header-set",
        document: concat!(
            "From: sender@senders.example.com\r\n",
            "To: success@simulator.amazonses.com\r\n",
            "Subject: a raw message under test\r\n",
            "\r\n",
            "hello from a raw document\r\n",
        ),
        email_tags: &[("campaign", "raw-absent")],
        expected: &[Series {
            metric: "Send",
            dimensions: &[
                (CONFIGURATION_SET_TAG, "raw-absent-header-set"),
                ("campaign", "raw-absent"),
                ("X-Custom-Header", "none"),
            ],
        }],
    },
];

#[tokio::test]
async fn a_raw_sends_header_dimension_resolves_from_the_documents_own_header_block() {
    for raw in RAW_ROWS {
        let case = raw.case;
        let emulator = Emulator::new();
        emulator.given_verified_sender(case).await;
        emulator
            .given_destination(case, raw.set, true, &["SEND"], RAW_DIMENSIONS)
            .await;
        emulator
            .send_raw(
                case,
                raw.set,
                ACCEPTING_RECIPIENT,
                raw.email_tags,
                raw.document,
            )
            .await;

        let listed = settled_count(&emulator, case, raw.expected.len()).await;
        assert_eq!(
            listed,
            expected_of(raw.expected),
            "case {case:?}: expected exactly the declared series in AWS/SES, got {listed:?}"
        );
        for wanted in raw.expected {
            let sum = emulator.sum(case, wanted.metric, wanted.dimensions).await;
            assert_eq!(
                sum, 1.0,
                "case {case:?}: expected Sum 1 for {:?} over {:?}, got {sum}",
                wanted.metric, wanted.dimensions
            );
        }
    }
}

/// What the bulk-send destination declares: a tag of each message and a header of it.
const BULK_DIMENSIONS: &[Dimension] = &[
    Dimension {
        name: "campaign",
        source: "MESSAGE_TAG",
        default: "none",
    },
    Dimension {
        name: "X-Custom-Header",
        source: "EMAIL_HEADER",
        default: "no-header",
    },
];

/// Exactly the series a bulk send of the three entries below must leave: one per MESSAGE,
/// resolved from what is that entry's own and falling back to the request's defaults and
/// then to each configuration's own default.
const BULK_SERIES: &[Series] = &[
    Series {
        metric: "Send",
        dimensions: &[
            ("campaign", "bulk-first"),
            ("X-Custom-Header", "first-header"),
        ],
    },
    Series {
        metric: "Send",
        dimensions: &[("campaign", "bulk-second"), ("X-Custom-Header", "no-header")],
    },
    Series {
        metric: "Send",
        dimensions: &[
            ("campaign", "bulk-default"),
            ("X-Custom-Header", "no-header"),
        ],
    },
];

#[tokio::test]
async fn a_bulk_send_counts_one_event_per_entry_under_that_entrys_own_dimensions() {
    // A bulk send composes one MESSAGE per entry, so each entry's own event is counted
    // under the dimensions resolved from that message: the entry's `ReplacementTags` and
    // `ReplacementHeaders` where it states them, the request's `DefaultEmailTags` where it
    // does not, and the configuration's `DefaultDimensionValue` where neither supplies the
    // dimension at all.
    let case = "three bulk entries through one destination";
    let set = "bulk-entries-set";
    let emulator = Emulator::new();
    emulator.given_verified_sender(case).await;
    emulator.given_template(case).await;
    emulator
        .given_destination(case, set, true, &["SEND"], BULK_DIMENSIONS)
        .await;

    let results = emulator
        .send_bulk(
            case,
            set,
            &[("campaign", "bulk-default")],
            vec![
                json!({
                    "Destination": {"ToAddresses": [ACCEPTING_RECIPIENT]},
                    "ReplacementTags": [{"Name": "campaign", "Value": "bulk-first"}],
                    "ReplacementHeaders": [
                        {"Name": "X-Custom-Header", "Value": "first-header"},
                    ],
                }),
                json!({
                    "Destination": {"ToAddresses": ["second@example.net"]},
                    "ReplacementTags": [{"Name": "campaign", "Value": "bulk-second"}],
                }),
                json!({"Destination": {"ToAddresses": ["third@example.net"]}}),
            ],
        )
        .await;
    assert_eq!(
        results.len(),
        BULK_SERIES.len(),
        "case {case:?}: expected one result per entry, got {results:?}"
    );
    for (status, message_id) in &results {
        assert_eq!(
            status, "SUCCESS",
            "case {case:?}: every entry must be accepted, got {results:?}"
        );
        assert!(
            !message_id.is_empty(),
            "case {case:?}: an accepted entry must answer a MessageId, got {results:?}"
        );
    }

    let listed = settled_count(&emulator, case, BULK_SERIES.len()).await;
    assert_eq!(
        listed,
        expected_of(BULK_SERIES),
        "case {case:?}: expected exactly one series per entry in AWS/SES, got {listed:?}"
    );
    for wanted in BULK_SERIES {
        let sum = emulator.sum(case, wanted.metric, wanted.dimensions).await;
        assert_eq!(
            sum, 1.0,
            "case {case:?}: expected Sum 1 for {:?} over {:?}, got {sum}",
            wanted.metric, wanted.dimensions
        );
    }
}

/// The default a declared dimension carries where the send supplies no value for it, in
/// the header-form cases below.
const TAG_HEADER_DEFAULT: &str = "none";

/// How long a message tag's name or value may be: it "can contain no more than 256
/// characters" (`smithy_query(query="doc",shape="MessageTag",member="Name")`, and the same
/// for `Value`). Stated here from the model rather than read from the implementation, so a
/// case drives the published bound and not whatever the code happens to hold.
const MAX_MESSAGE_TAG_CHARS: usize = 256;

/// How many `name=value` pairs this emulator reads from the `X-SES-MESSAGE-TAGS` headers of
/// one message. Mirrors the emulator's own ceiling rather than any AWS bound, so a case can
/// drive a pair written past it.
const EMULATOR_HEADER_TAG_PAIRS: usize = 64;

/// The forms an `X-SES-MESSAGE-TAGS` header value can take, and the value a declared
/// `campaign` dimension resolves to for each: the header writes message tags as
/// `name=value` pairs separated by commas, and anything that is no message tag applies
/// none — so the dimension carries its own default instead.
///
/// `(case, header value, the value the dimension resolves to)`.
fn message_tag_header_rows() -> Vec<(String, String, String)> {
    let longest = "v".repeat(MAX_MESSAGE_TAG_CHARS);
    let past_longest = "v".repeat(MAX_MESSAGE_TAG_CHARS + 1);
    let rows: Vec<(&str, String, String)> = vec![
        (
            "one pair naming the declared dimension",
            "campaign=header-value".to_string(),
            "header-value".to_string(),
        ),
        (
            "the pair among others",
            "tier=gold,campaign=header-value,region=eu-west-1".to_string(),
            "header-value".to_string(),
        ),
        (
            "spaces around the pairs",
            " campaign = header-value , tier = gold ".to_string(),
            "header-value".to_string(),
        ),
        (
            "empty pairs beside the declared one",
            "campaign=header-value,,".to_string(),
            "header-value".to_string(),
        ),
        (
            "a pair with no value at all",
            "campaign".to_string(),
            TAG_HEADER_DEFAULT.to_string(),
        ),
        (
            "nothing before the =",
            "=header-value".to_string(),
            TAG_HEADER_DEFAULT.to_string(),
        ),
        (
            "a value outside a message tag's character set",
            "campaign=not a tag value".to_string(),
            TAG_HEADER_DEFAULT.to_string(),
        ),
        (
            "a name outside a message tag's character set",
            "camp:aign=header-value".to_string(),
            TAG_HEADER_DEFAULT.to_string(),
        ),
        (
            "text that writes no pair at all",
            "garbage".to_string(),
            TAG_HEADER_DEFAULT.to_string(),
        ),
        (
            // The bound is the message TAG's, which is what resolving the dimension
            // applies; CloudWatch's own `DimensionValue` tops out one character lower, and
            // this series is appended to its store rather than sent through
            // `PutMetricData`, which is where that constraint is applied.
            "a value of exactly the longest a tag may be",
            format!("campaign={longest}"),
            longest.clone(),
        ),
        (
            "a value one character longer than a tag may be",
            format!("campaign={past_longest}"),
            TAG_HEADER_DEFAULT.to_string(),
        ),
        (
            // The ceiling on how many pairs a message's headers apply is the emulator's,
            // not an AWS quota: a header's value is caller-supplied text of no modelled
            // length, so only the first EMULATOR_HEADER_TAG_PAIRS pairs are read and a
            // pair past them applies nothing.
            "the declared pair past the pairs one message applies",
            format!(
                "{}campaign=header-value",
                "x=y,".repeat(EMULATOR_HEADER_TAG_PAIRS)
            ),
            TAG_HEADER_DEFAULT.to_string(),
        ),
    ];
    rows.into_iter()
        .map(|(case, header, resolved)| (case.to_string(), header, resolved))
        .collect()
}

#[tokio::test]
async fn a_message_tags_header_applies_only_the_tags_it_really_writes() {
    for (case, header, resolved) in message_tag_header_rows() {
        let case = case.as_str();
        let set = "tag-header-form-set";
        let emulator = Emulator::new();
        emulator.given_verified_sender(case).await;
        emulator
            .given_destination(
                case,
                set,
                true,
                &["SEND"],
                &[Dimension {
                    name: "campaign",
                    source: "MESSAGE_TAG",
                    default: TAG_HEADER_DEFAULT,
                }],
            )
            .await;
        emulator
            .send(
                case,
                set,
                ACCEPTING_RECIPIENT,
                &[],
                &[("X-SES-MESSAGE-TAGS", header.as_str())],
            )
            .await;

        let listed = settled_count(&emulator, case, 1).await;
        let expected = vec![(
            "Send".to_string(),
            vec![("campaign".to_string(), resolved.clone())],
        )];
        assert_eq!(
            listed, expected,
            "case {case:?}: the header {header:?} must leave exactly the series \
             {expected:?}, got {listed:?}"
        );
        let sum = emulator
            .sum(case, "Send", &[("campaign", resolved.as_str())])
            .await;
        assert_eq!(
            sum, 1.0,
            "case {case:?}: expected Sum 1 for campaign={resolved:?}, got {sum}"
        );
    }
}
