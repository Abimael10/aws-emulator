// SPDX-License-Identifier: CC0-1.0

//! `PutBucketNotificationConfiguration` against a destination that ENCRYPTS its messages
//! under a customer managed KMS key.
//!
//! The rule under test: a destination carrying a `KmsMasterKeyId` has every message written
//! to it encrypted under that key, and a notification is written by Amazon S3 as its own
//! service principal on behalf of the publishing bucket — so that key's own policy decides
//! whether the publication can be encrypted at all. A configuration naming a destination
//! whose key will not seal a notification of this bucket names one S3 cannot publish to, and
//! the whole call is refused HTTP 400 `InvalidArgument` with the fixed message
//! "Unable to validate the following destination configurations" and the numbered
//! `<ArgumentName{n}>`/`<ArgumentValue{n}>` pair naming the destination's ARN and the reason;
//! the bucket keeps exactly the configuration it already carried, and the destination is sent
//! nothing. Granting the Amazon S3 service principal the operations that seal a message
//! (`kms:GenerateDataKey` and `kms:Decrypt`, the pair AWS documents for a key that encrypts a
//! notification destination) for this bucket makes the identical call succeed at once, and the
//! `s3:TestEvent` then arrives on the queue.
//!
//! The rows span what the rule generalises over rather than one recorded configuration:
//!
//! * the destination's ENCRYPTION — none at all (a queue on the Amazon SQS owned key), and a
//!   customer managed key;
//! * WHICH authority the key policy states — nothing beyond the default root statement, the
//!   sealing pair for this bucket, the same pair for every source, the same pair for ANOTHER
//!   bucket, the pair granted to another service principal, and only one of the two
//!   operations;
//! * the CONDITION a statement is scoped by — `aws:SourceArn` compared with `StringEquals` and
//!   with `ArnLike`, and `kms:ViaService` naming the destination's own service and another
//!   service's;
//! * the key's own STATE — enabled, and disabled, which seals nothing for anybody;
//! * HOW MANY destinations one container names and which of them is refused — one, and an
//!   unencrypted queue beside an encrypted one;
//! * and whether `SkipDestinationValidation` was asked for, which skips the question entirely.
//!
//! Every assertion drives real HTTP requests through a router carrying S3 beside SQS (which
//! owns the destination) and KMS (which owns the key and its policy), so each key, key policy
//! and queue is made with those services' own operations: nothing here reaches into a store to
//! plant a key, a policy or a decision.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{elements_of, Answer};

/// The Region every request below is signed for: a bucket, the queue it notifies and the key
/// that queue is encrypted under all live at the same endpoint.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The error code S3 answers a container it could not validate with, and the status it
/// carries ("InvalidArgument - Invalid Argument", HTTP 400, from Amazon S3's list of error
/// codes).
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;

/// The `<Message>` that refusal carries, as S3 words it — the same sentence however many
/// destinations failed.
const UNABLE_TO_VALIDATE: &str = "Unable to validate the following destination configurations";

/// The per-destination detail S3 carries for a QUEUE destination whose encryption key will
/// not seal the notification.
const QUEUE_UNENCRYPTABLE: &str = "The message to the SSE queue could not be encrypted using KMS";

/// The `Event` of the test notification a validated destination receives.
const TEST_EVENT: &str = "s3:TestEvent";

/// The event every row's own configuration is notified about — immaterial to the rule, which
/// is about the DESTINATION.
const CONFIGURED_EVENT: &str = "s3:ObjectCreated:*";

/// The event the baseline configuration each bucket starts with is notified about, which is
/// what tells the baseline apart from a row's own container in a read-back.
const BASELINE_EVENT: &str = "s3:ObjectRemoved:*";

/// The bucket a grant scoped to ANOTHER source names, which is never the bucket the row
/// configures.
const ANOTHER_BUCKET: &str = "some-other-bucket";

/// How long a wait for the test notification lasts before giving up. The publication is made
/// inside the put, so this is many deliveries' worth of slack and not a span the behaviour
/// depends on.
const DELIVERY_BUDGET: Duration = Duration::from_secs(10);

/// S3 beside the services a decision about an encrypted destination reads from: SQS (the
/// queue, its access policy and its `KmsMasterKeyId`) and KMS (the key, its policy and its
/// state).
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_kms_trentservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Dispatch one request, signed for `signing_name` so the router resolves the service
    /// the way it does in production.
    async fn send(
        &self,
        signing_name: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=not-verified"
        );
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("authorization", authorization);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body))
            .expect("the test request must build");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        Answer {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body,
        }
    }

    /// One S3 request carrying an XML (or empty) payload.
    async fn s3(&self, method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> Answer {
        let mut all: Vec<(&str, &str)> = vec![("content-type", "application/xml")];
        all.extend_from_slice(headers);
        self.send("s3", method, uri, &all, body.as_bytes().to_vec())
            .await
    }

    /// One RPC call that must answer 200, with its payload parsed.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let content_type = format!("application/x-amz-json-{json_version}");
        let target = format!("{target_prefix}.{operation}");
        let answer = self
            .send(
                signing_name,
                "POST",
                "/",
                &[
                    ("content-type", content_type.as_str()),
                    ("x-amz-target", target.as_str()),
                ],
                body.to_string().into_bytes(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "{operation} {body} must answer HTTP 200, got {} with {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        if answer.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&answer.body).unwrap_or(Value::Null)
    }

    /// An Amazon SQS call (`awsJson1_0`).
    async fn sqs(&self, operation: &str, body: &Value) -> Value {
        self.rpc("sqs", "AmazonSQS", "1.0", operation, body).await
    }

    /// A KMS call (`awsJson1_1`, reached by `X-Amz-Target: TrentService.<Operation>`).
    async fn kms(&self, operation: &str, body: &Value) -> Value {
        self.rpc("kms", "TrentService", "1.1", operation, body).await
    }

    /// `PUT /<bucket>` — `CreateBucket`.
    async fn create_bucket(&self, bucket: &str) {
        let answer = self.s3("PUT", &format!("/{bucket}"), &[], "").await;
        assert_eq!(
            answer.status,
            200,
            "creating {bucket:?} must succeed, got {} with {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
    }

    /// Create a customer managed KMS key carrying `policy`, and answer its ARN.
    async fn create_key(&self, case: &str, policy: &str) -> String {
        let created = self
            .kms("CreateKey", &json!({ "Policy": policy, "Description": case }))
            .await;
        created["KeyMetadata"]["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateKey must answer an Arn, got {created}"))
            .to_string()
    }

    /// Create the queue named `name`, grant Amazon S3 the send from `bucket` in its own access
    /// policy, and put it on the KMS key `master_key_id` when one is given. Answers the
    /// queue's url beside the ARN Amazon SQS keyed it by.
    ///
    /// The access policy is the grant S3's destination validation already asks for, so what a
    /// row varies is the KEY's policy and not this one.
    async fn create_queue(
        &self,
        name: &str,
        bucket: &str,
        master_key_id: Option<&str>,
    ) -> (String, String) {
        let created = self.sqs("CreateQueue", &json!({ "QueueName": name })).await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateQueue must answer a QueueUrl, got {created}"))
            .to_string();
        let attributes = self
            .sqs(
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("GetQueueAttributes must answer a QueueArn, got {attributes}")
            })
            .to_string();
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "s3-publish",
                "Effect": "Allow",
                "Principal": {"Service": "s3.amazonaws.com"},
                "Action": "sqs:SendMessage",
                "Resource": arn,
                "Condition": {"ArnLike": {"aws:SourceArn": bucket_arn(bucket)}},
            }],
        })
        .to_string();
        let mut set = json!({ "QueueUrl": url, "Attributes": { "Policy": document } });
        if let Some(key) = master_key_id {
            set["Attributes"]["KmsMasterKeyId"] = json!(key);
        }
        self.sqs("SetQueueAttributes", &set).await;
        (url, arn)
    }

    /// `PUT /<bucket>?notification` carrying `body`, and `x-amz-skip-destination-validation`
    /// when the row asks for it (the header
    /// `PutBucketNotificationConfigurationRequest$SkipDestinationValidation` binds to).
    async fn put_notification(&self, bucket: &str, body: &str, skip_validation: bool) -> Answer {
        let headers: &[(&str, &str)] = if skip_validation {
            &[("x-amz-skip-destination-validation", "true")]
        } else {
            &[]
        };
        self.s3("PUT", &format!("/{bucket}?notification"), headers, body)
            .await
    }

    /// `GET /<bucket>?notification` — the configuration the bucket really carries.
    async fn read_notification(&self, case: &str, bucket: &str) -> String {
        let read = self.s3("GET", &format!("/{bucket}?notification"), &[], "").await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: reading the configuration back must succeed, got {} with {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        String::from_utf8_lossy(&read.body).to_string()
    }

    /// Every message on the queue right now, each parsed as the JSON it carries.
    async fn drain(&self, queue_url: &str) -> Vec<Value> {
        let received = self
            .sqs(
                "ReceiveMessage",
                &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        received["Messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|message| {
                let body = message["Body"].as_str().unwrap_or_default();
                serde_json::from_str(body).unwrap_or(Value::Null)
            })
            .collect()
    }

    /// The first message the queue receives, waiting until it arrives or [`DELIVERY_BUDGET`]
    /// passes — the CONDITION rather than a fixed span.
    async fn delivered(&self, queue_url: &str) -> Option<Value> {
        let deadline = Instant::now() + DELIVERY_BUDGET;
        loop {
            let mut messages = self.drain(queue_url).await;
            if !messages.is_empty() {
                return Some(messages.remove(0));
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// An S3 bucket's ARN — `arn:aws:s3:::<bucket>`, which carries no Region or account "because
/// a bucket name is globally unique".
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// Which authority a row's KMS key policy states for the publication, beside the default root
/// statement every document below carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyGrant {
    /// Nothing at all: the key carries only the statement giving the owning account its own
    /// access, which grants a service principal nothing.
    None,
    /// Both sealing operations to `s3.amazonaws.com`, scoped to THIS bucket by
    /// `aws:SourceArn` under `StringEquals`.
    ThisBucket,
    /// The same pair, scoped to THIS bucket by `aws:SourceArn` under `ArnLike` — the other
    /// operator a grant to a service principal is written with.
    ThisBucketArnLike,
    /// The same pair with no condition at all, so every source is granted.
    AnySource,
    /// The same pair, scoped to ANOTHER bucket's ARN.
    OtherBucket,
    /// The same pair, granted to another service principal.
    OtherPrincipal,
    /// Only ONE of the two sealing operations, which is not the pair a destination's
    /// encryption needs.
    DataKeyOnly,
    /// The pair for this bucket, scoped by `kms:ViaService` to the service that OWNS the
    /// destination — the service the use really reaches KMS through.
    ViaDestinationService,
    /// The pair for this bucket, scoped by `kms:ViaService` to another service.
    ViaOtherService,
}

/// The operations sealing one message under a customer managed key performs, which is the
/// pair AWS documents for a key that encrypts a notification destination
/// (`kms:GenerateDataKey` and `kms:Decrypt`).
const SEALING_ACTIONS: [&str; 2] = ["kms:GenerateDataKey", "kms:Decrypt"];

/// The key policy for `grant`, as a document `CreateKey` accepts.
///
/// Every document opens with the statement the default key policy carries — the owning
/// account's own access, which is what keeps the key manageable — so what a row varies is the
/// statement about the publication alone.
fn key_policy(grant: KeyGrant, bucket: &str) -> String {
    let account = engine::emulator_account_id();
    let root = json!({
        "Sid": "Enable IAM User Permissions",
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
        "Action": "kms:*",
        "Resource": "*",
    });
    let publication = match grant {
        KeyGrant::None => None,
        KeyGrant::ThisBucket => Some(json!({
            "Sid": "s3-seal",
            "Effect": "Allow",
            "Principal": {"Service": "s3.amazonaws.com"},
            "Action": SEALING_ACTIONS,
            "Resource": "*",
            "Condition": {"StringEquals": {"aws:SourceArn": bucket_arn(bucket)}},
        })),
        KeyGrant::ThisBucketArnLike => Some(json!({
            "Sid": "s3-seal-arnlike",
            "Effect": "Allow",
            "Principal": {"Service": "s3.amazonaws.com"},
            "Action": SEALING_ACTIONS,
            "Resource": "*",
            "Condition": {"ArnLike": {"aws:SourceArn": bucket_arn(bucket)}},
        })),
        KeyGrant::AnySource => Some(json!({
            "Sid": "s3-seal-any",
            "Effect": "Allow",
            "Principal": {"Service": "s3.amazonaws.com"},
            "Action": SEALING_ACTIONS,
            "Resource": "*",
        })),
        KeyGrant::OtherBucket => Some(json!({
            "Sid": "s3-seal-other-bucket",
            "Effect": "Allow",
            "Principal": {"Service": "s3.amazonaws.com"},
            "Action": SEALING_ACTIONS,
            "Resource": "*",
            "Condition": {"StringEquals": {"aws:SourceArn": bucket_arn(ANOTHER_BUCKET)}},
        })),
        KeyGrant::OtherPrincipal => Some(json!({
            "Sid": "sns-seal",
            "Effect": "Allow",
            "Principal": {"Service": "sns.amazonaws.com"},
            "Action": SEALING_ACTIONS,
            "Resource": "*",
        })),
        KeyGrant::DataKeyOnly => Some(json!({
            "Sid": "s3-seal-data-key-only",
            "Effect": "Allow",
            "Principal": {"Service": "s3.amazonaws.com"},
            "Action": SEALING_ACTIONS[0],
            "Resource": "*",
        })),
        KeyGrant::ViaDestinationService => Some(json!({
            "Sid": "s3-seal-via-sqs",
            "Effect": "Allow",
            "Principal": {"Service": "s3.amazonaws.com"},
            "Action": SEALING_ACTIONS,
            "Resource": "*",
            "Condition": {"StringEquals": {"kms:ViaService": format!("sqs.{REGION}.amazonaws.com")}},
        })),
        KeyGrant::ViaOtherService => Some(json!({
            "Sid": "s3-seal-via-other",
            "Effect": "Allow",
            "Principal": {"Service": "s3.amazonaws.com"},
            "Action": SEALING_ACTIONS,
            "Resource": "*",
            "Condition": {"StringEquals": {"kms:ViaService": format!("dynamodb.{REGION}.amazonaws.com")}},
        })),
    };
    let mut statements = vec![root];
    statements.extend(publication);
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// How a row's queue destination is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encryption {
    /// On the Amazon SQS owned key: no KMS key of this account is involved, so nothing is
    /// asked of KMS at all.
    SqsOwned,
    /// On a customer managed key whose policy states `grant`, enabled when `enabled`.
    CustomerKey { grant: KeyGrant, enabled: bool },
}

/// One destination a row's container names.
#[derive(Debug, Clone, Copy)]
struct Named {
    name: &'static str,
    encryption: Encryption,
    /// `None` for a destination that validates.
    rejected: Option<&'static str>,
}

/// One row of the table.
struct Row {
    case: &'static str,
    destinations: &'static [Named],
    /// Whether the request carries `SkipDestinationValidation`.
    skip_validation: bool,
    /// The status the put must answer.
    expected_status: u16,
}

/// The rule, over the class it generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a queue on the Amazon SQS owned key, which asks KMS nothing",
            destinations: &[Named {
                name: "sqs-owned-queue",
                encryption: Encryption::SqsOwned,
                rejected: None,
            }],
            skip_validation: false,
            expected_status: 200,
        },
        Row {
            case: "a queue on a key granting the sealing pair for this bucket",
            destinations: &[Named {
                name: "granted-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::ThisBucket,
                    enabled: true,
                },
                rejected: None,
            }],
            skip_validation: false,
            expected_status: 200,
        },
        Row {
            case: "a queue on a key granting the pair for this bucket under ArnLike",
            destinations: &[Named {
                name: "granted-arnlike-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::ThisBucketArnLike,
                    enabled: true,
                },
                rejected: None,
            }],
            skip_validation: false,
            expected_status: 200,
        },
        Row {
            case: "a queue on a key granting the pair for every source",
            destinations: &[Named {
                name: "any-source-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::AnySource,
                    enabled: true,
                },
                rejected: None,
            }],
            skip_validation: false,
            expected_status: 200,
        },
        Row {
            case: "a queue on a key granting the pair through the destination's own service",
            destinations: &[Named {
                name: "via-sqs-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::ViaDestinationService,
                    enabled: true,
                },
                rejected: None,
            }],
            skip_validation: false,
            expected_status: 200,
        },
        Row {
            case: "a queue on a key whose policy grants Amazon S3 nothing",
            destinations: &[Named {
                name: "ungranted-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::None,
                    enabled: true,
                },
                rejected: Some(QUEUE_UNENCRYPTABLE),
            }],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            case: "a queue on a key whose grant names another bucket",
            destinations: &[Named {
                name: "other-bucket-key-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::OtherBucket,
                    enabled: true,
                },
                rejected: Some(QUEUE_UNENCRYPTABLE),
            }],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            case: "a queue on a key whose grant names another service principal",
            destinations: &[Named {
                name: "other-principal-key-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::OtherPrincipal,
                    enabled: true,
                },
                rejected: Some(QUEUE_UNENCRYPTABLE),
            }],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            case: "a queue on a key granting only one of the two sealing operations",
            destinations: &[Named {
                name: "data-key-only-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::DataKeyOnly,
                    enabled: true,
                },
                rejected: Some(QUEUE_UNENCRYPTABLE),
            }],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            case: "a queue on a key whose grant is scoped to another service's path",
            destinations: &[Named {
                name: "via-other-service-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::ViaOtherService,
                    enabled: true,
                },
                rejected: Some(QUEUE_UNENCRYPTABLE),
            }],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            case: "a queue on a DISABLED key that grants the pair for this bucket",
            destinations: &[Named {
                name: "disabled-key-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::ThisBucket,
                    enabled: false,
                },
                rejected: Some(QUEUE_UNENCRYPTABLE),
            }],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            // The recorded shape of this defect: the unencrypted control queue validates and
            // the encrypted one does not, so the container is refused WHOLE and the refusal
            // names only the queue that failed.
            case: "an unencrypted queue beside one on a key that grants Amazon S3 nothing",
            destinations: &[
                Named {
                    name: "control-queue",
                    encryption: Encryption::SqsOwned,
                    rejected: None,
                },
                Named {
                    name: "kms-subject-queue",
                    encryption: Encryption::CustomerKey {
                        grant: KeyGrant::None,
                        enabled: true,
                    },
                    rejected: Some(QUEUE_UNENCRYPTABLE),
                },
            ],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            case: "two queues on ungranted keys, both refused",
            destinations: &[
                Named {
                    name: "two-keys-first-queue",
                    encryption: Encryption::CustomerKey {
                        grant: KeyGrant::None,
                        enabled: true,
                    },
                    rejected: Some(QUEUE_UNENCRYPTABLE),
                },
                Named {
                    name: "two-keys-second-queue",
                    encryption: Encryption::CustomerKey {
                        grant: KeyGrant::OtherBucket,
                        enabled: true,
                    },
                    rejected: Some(QUEUE_UNENCRYPTABLE),
                },
            ],
            skip_validation: false,
            expected_status: INVALID_ARGUMENT_STATUS,
        },
        Row {
            case: "a queue on an ungranted key, with validation skipped",
            destinations: &[Named {
                name: "skipped-ungranted-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::None,
                    enabled: true,
                },
                rejected: None,
            }],
            skip_validation: true,
            expected_status: 200,
        },
    ]
}

/// The `NotificationConfiguration` payload naming `destinations` by ARN, each notified about
/// `event`.
///
/// Each configuration filters on a key prefix of its own, which is what lets one container
/// name SEVERAL destinations for one event.
fn payload(destinations: &[(&Named, String)], event: &str) -> String {
    let mut body = format!("<NotificationConfiguration xmlns=\"{XMLNS}\">");
    for (named, arn) in destinations {
        body.push_str(&format!(
            "<QueueConfiguration><Queue>{arn}</Queue><Event>{event}</Event>\
             <Filter><S3Key><FilterRule><Name>prefix</Name><Value>{prefix}/</Value>\
             </FilterRule></S3Key></Filter></QueueConfiguration>",
            prefix = named.name
        ));
    }
    body.push_str("</NotificationConfiguration>");
    body
}

/// Stand up the destinations one row names: a KMS key where its encryption asks for one, the
/// queue itself, and the key's state. Answers each destination beside the ARN and url it was
/// made with.
async fn destinations_of<'row>(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    row: &'row Row,
) -> Vec<(&'row Named, String, String)> {
    let mut made = Vec::new();
    for named in row.destinations {
        let key = match named.encryption {
            Encryption::SqsOwned => None,
            Encryption::CustomerKey { grant, enabled } => {
                let key_arn = emulator.create_key(case, &key_policy(grant, bucket)).await;
                if !enabled {
                    emulator
                        .kms("DisableKey", &json!({ "KeyId": key_arn }))
                        .await;
                }
                Some(key_arn)
            }
        };
        let (url, arn) = emulator
            .create_queue(named.name, bucket, key.as_deref())
            .await;
        made.push((named, arn, url));
    }
    made
}

#[tokio::test]
async fn a_configuration_naming_a_destination_s3_cannot_encrypt_for_is_refused_whole() {
    let emulator = Emulator::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let bucket = format!("destination-encryption-{index}");
        emulator.create_bucket(&bucket).await;

        // The configuration the bucket carries BEFORE the row's put, so "nothing was
        // recorded" is a read that reports this one rather than one empty container compared
        // against another. It is installed with validation skipped, which is the one way to
        // configure a destination without publishing a test notification.
        let baseline = format!("baseline-{index}");
        let (baseline_url, baseline_arn) = emulator.create_queue(&baseline, &bucket, None).await;
        let baseline_body = format!(
            "<NotificationConfiguration xmlns=\"{XMLNS}\"><QueueConfiguration><Queue>\
             {baseline_arn}</Queue><Event>{BASELINE_EVENT}</Event></QueueConfiguration>\
             </NotificationConfiguration>"
        );
        let seeded = emulator
            .put_notification(&bucket, &baseline_body, true)
            .await;
        assert_eq!(
            seeded.status,
            200,
            "case {case:?}: seeding the baseline configuration must succeed, got {} with {}",
            seeded.status,
            String::from_utf8_lossy(&seeded.body)
        );

        let made = destinations_of(&emulator, case, &bucket, row).await;
        let named_arns: Vec<(&Named, String)> = made
            .iter()
            .map(|(named, arn, _)| (*named, arn.clone()))
            .collect();
        let body = payload(&named_arns, CONFIGURED_EVENT);
        let answer = emulator
            .put_notification(&bucket, &body, row.skip_validation)
            .await;
        let document = String::from_utf8_lossy(&answer.body).to_string();
        assert_eq!(
            answer.status, row.expected_status,
            "case {case:?}: expected HTTP {}, got {} with {document}",
            row.expected_status, answer.status
        );

        let reported = emulator.read_notification(case, &bucket).await;

        if row.expected_status == 200 {
            let expected: Vec<String> = made.iter().map(|(_, arn, _)| arn.clone()).collect();
            let stored = elements_of(&reported, "Queue");
            assert_eq!(
                stored, expected,
                "case {case:?}: expected the stored destinations {expected:?}, got {stored:?} \
                 in {reported}"
            );
            if row.skip_validation {
                continue;
            }
            // Validated: each queue carries the test notification naming this bucket.
            for (named, _, url) in &made {
                let test_event = emulator.delivered(url).await.unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the queue {:?} validated, so it must carry the test \
                         notification",
                        named.name
                    )
                });
                assert_eq!(
                    test_event["Event"].as_str(),
                    Some(TEST_EVENT),
                    "case {case:?}: expected the test notification's Event {TEST_EVENT:?}, got \
                     {test_event}"
                );
                assert_eq!(
                    test_event["Bucket"].as_str(),
                    Some(bucket.as_str()),
                    "case {case:?}: expected the test notification to name {bucket:?}, got \
                     {test_event}"
                );
            }
            continue;
        }

        // Refused: the error document carries the code and the fixed message S3 sends.
        assert!(
            elements_of(&document, "Code").contains(&INVALID_ARGUMENT.to_string()),
            "case {case:?}: expected the refusal's Code to be {INVALID_ARGUMENT:?}, got \
             {document}"
        );
        assert!(
            elements_of(&document, "Message").contains(&UNABLE_TO_VALIDATE.to_string()),
            "case {case:?}: expected the refusal's Message to be {UNABLE_TO_VALIDATE:?}, got \
             {document}"
        );

        // Beside it, the NUMBERED pair naming each destination that failed and the reason it
        // failed, in the order the container named them — and nothing past them.
        let expected: Vec<(String, String)> = made
            .iter()
            .filter_map(|(named, arn, _)| {
                named.rejected.map(|detail| (arn.clone(), detail.to_string()))
            })
            .collect();
        let refused: Vec<(String, String)> = (1..=expected.len())
            .map(|position| {
                let name = elements_of(&document, &format!("ArgumentName{position}"));
                let value = elements_of(&document, &format!("ArgumentValue{position}"));
                (
                    name.first().cloned().unwrap_or_default(),
                    value.first().cloned().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            refused, expected,
            "case {case:?}: expected the refusal to name each failed destination and its \
             reason as {expected:?}, got {refused:?} in {document}"
        );
        assert!(
            elements_of(&document, &format!("ArgumentName{}", expected.len() + 1)).is_empty(),
            "case {case:?}: the refusal must name no destination past the ones that failed, \
             got {document}"
        );

        // The rejection is atomic: the bucket keeps exactly the configuration it carried.
        let stored = elements_of(&reported, "Queue");
        assert_eq!(
            stored,
            vec![baseline_arn.clone()],
            "case {case:?}: a refused put records nothing, so the bucket must still carry the \
             baseline configuration, got {reported}"
        );
        assert_eq!(
            elements_of(&reported, "Event"),
            vec![BASELINE_EVENT.to_string()],
            "case {case:?}: the baseline configuration must be untouched, got {reported}"
        );

        // And no destination the refusal NAMED was published to: the question the key
        // answers is asked before anything is sent, so a queue S3 could not have encrypted
        // for receives nothing at all. A destination that validated before another failed was
        // already sent its test notification, which is what makes the atomicity contract
        // about the STORED configuration above.
        for (named, _, url) in made.iter().filter(|(named, _, _)| named.rejected.is_some()) {
            let drained = emulator.drain(url).await;
            assert!(
                drained.is_empty(),
                "case {case:?}: a destination the refusal named is published nothing, so the \
                 queue {:?} must be empty, got {drained:?}",
                named.name
            );
        }
        let on_baseline = emulator.drain(&baseline_url).await;
        assert!(
            on_baseline.is_empty(),
            "case {case:?}: the baseline queue was configured with validation skipped and is \
             named by no later container, so it must stay empty, got {on_baseline:?}"
        );
    }
}

#[tokio::test]
async fn granting_the_key_makes_the_identical_configuration_succeed_at_once() {
    let emulator = Emulator::new();
    let case = "the key policy gains the grant between two identical puts";
    let bucket = "destination-encryption-grant-added";
    emulator.create_bucket(bucket).await;

    let key_arn = emulator
        .create_key(case, &key_policy(KeyGrant::None, bucket))
        .await;
    let (url, arn) = emulator
        .create_queue("grant-added-queue", bucket, Some(&key_arn))
        .await;
    let body = payload(
        &[(
            &Named {
                name: "grant-added-queue",
                encryption: Encryption::CustomerKey {
                    grant: KeyGrant::None,
                    enabled: true,
                },
                rejected: Some(QUEUE_UNENCRYPTABLE),
            },
            arn.clone(),
        )],
        CONFIGURED_EVENT,
    );

    let refused = emulator.put_notification(bucket, &body, false).await;
    let document = String::from_utf8_lossy(&refused.body).to_string();
    assert_eq!(
        refused.status, INVALID_ARGUMENT_STATUS,
        "case {case:?}: the put must be refused while the key grants Amazon S3 nothing, got \
         {} with {document}",
        refused.status
    );
    assert_eq!(
        elements_of(&document, "ArgumentValue1"),
        vec![QUEUE_UNENCRYPTABLE.to_string()],
        "case {case:?}: expected the reason {QUEUE_UNENCRYPTABLE:?}, got {document}"
    );

    // The one thing that changes: the key's own policy now grants the sealing pair for this
    // bucket. Nothing about the bucket, the container or the queue is touched.
    emulator
        .kms(
            "PutKeyPolicy",
            &json!({
                "KeyId": key_arn,
                "PolicyName": "default",
                "Policy": key_policy(KeyGrant::ThisBucket, bucket),
            }),
        )
        .await;

    let accepted = emulator.put_notification(bucket, &body, false).await;
    assert_eq!(
        accepted.status,
        200,
        "case {case:?}: the identical put must succeed once the key policy grants it, got {} \
         with {}",
        accepted.status,
        String::from_utf8_lossy(&accepted.body)
    );
    let reported = emulator.read_notification(case, bucket).await;
    assert_eq!(
        elements_of(&reported, "Queue"),
        vec![arn.clone()],
        "case {case:?}: the accepted configuration must read back, got {reported}"
    );
    let test_event = emulator
        .delivered(&url)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the validated queue must carry the test event"));
    assert_eq!(
        test_event["Event"].as_str(),
        Some(TEST_EVENT),
        "case {case:?}: expected the test notification's Event {TEST_EVENT:?}, got {test_event}"
    );
}
