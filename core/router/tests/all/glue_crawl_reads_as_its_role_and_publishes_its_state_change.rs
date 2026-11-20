// SPDX-License-Identifier: CC0-1.0

//! What a STARTED crawl DOES on the assembled emulator: which of its targets it reads as the
//! crawler's role, which tables that leaves in the Data Catalog, and the state change it
//! publishes when the crawl comes to rest.
//!
//! The RULES under test:
//!
//! 1. **A crawl reads the data behind each Amazon S3 target AS the crawler's `Role`, so the
//!    documents the account holds on that role decide every call the walk makes: a target whose
//!    bucket listing and whose object read those documents admit is walked and the dataset
//!    behind it is published as a table in the crawler's `DatabaseName`, while a target either
//!    of those two calls is withheld on is not read at all, so no table is published for it.
//!    The crawl itself is carried out either way — it reached every target it was permitted —
//!    so it comes to rest with the crawler back in `READY` and `LastCrawl.Status` `SUCCEEDED`,
//!    and granting the missing read and starting the SAME crawler again then publishes the
//!    table.** Sources: `CreateCrawler$Role` ("the IAM role or Amazon Resource Name (ARN) of an
//!    IAM role used by the new crawler to access customer resources"), `CreateCrawler$Targets`,
//!    `StartCrawler` ("starts a crawl using the specified crawler, regardless of what is
//!    scheduled"), `LastCrawlInfo$Status` and the Amazon S3 permissions the two calls a walk
//!    makes require (`s3:ListBucket` for a listing, `s3:GetObject` for an object read).
//!
//! 2. **A crawl that comes to rest is published onto the account's DEFAULT EventBridge bus as
//!    `source` `aws.glue` / `detail-type` `Glue Crawler State Change`, carrying the crawler's
//!    own name and the state the crawl came to rest in inside the event `detail`; every rule
//!    whose `EventPattern` matches that envelope hands it to its targets, and a rule whose
//!    pattern selects some other state receives nothing — with NO further Glue call of any
//!    kind, so the publication is the service's own rather than a side effect of being read.**
//!    Sources: `PutRule$EventPattern` and `PutTargets` ("for Lambda functions, Amazon SNS
//!    topics and Amazon SQS queues, EventBridge relies on resource-based policies").
//!
//! WHY the suite lives here: four crates have to agree for either rule to hold — IAM answers
//! for the crawl role, Amazon S3 holds the data the walk reads, AWS Glue serves the crawl and
//! the catalog, and EventBridge and Amazon SQS carry the state change to a client that makes no
//! further Glue call. A crate-local test can only ever see one side of that. The crawl of a
//! stack-deployed crawler, the shape of the tables a permitted walk publishes and the two
//! modelled refusals `StartCrawler` answers are pinned beside this by
//! [`crate::cloudformation_deployed_crawler_crawls_its_s3_target`]; what that suite cannot
//! reach is the axis both rules here are about — the role's own documents deciding what the
//! walk reads, and the event the finished crawl publishes.
//!
//! The rows of each table differ along what its rule generalises over: WHICH of the two calls
//! the role's documents withhold and HOW they withhold it (no statement covering the target, an
//! explicit `Deny` of either call, a grant narrowed to another prefix) for rule 1, and which
//! dimension of the envelope the rule's pattern filters on for rule 2. Every row is otherwise
//! identical — the same bucket, the same data, the same trust document — so the only thing a
//! row's outcome can follow from is the one thing it varies.
//!
//! Nothing here sleeps for a span or asserts on the wall clock: every wait is for the CONDITION
//! it is about — the crawler's state, the message on the queue — looked at again until it holds
//! or a generous deadline passes. The spans the clock owes are the emulator's own.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so each test builds it on a
//! sized thread of its own with a multi-thread runtime: a crawl's own clock pump publishes the
//! state change from a task of its own.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is addressed to, and so the Region the crawl reads its
/// objects through and publishes its state change in.
const REGION: &str = "us-west-2";

/// The account every ARN this emulator mints carries, which the envelope names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The XML namespace Amazon S3's own model declares, which a client's payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal a crawler's role has to admit for Glue to act as it, written out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass. The rest of the vocabulary below is written out for the same
/// reason.
const GLUE_PRINCIPAL: &str = "glue.amazonaws.com";

/// The action a trust document names for that principal.
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The service principal EventBridge delivers to a queue target as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The two Amazon S3 calls one target's walk makes: the listing of the bucket under the
/// target's prefix, and the read of the object a schema is inferred from.
const LIST_ACTION: &str = "s3:ListBucket";
const READ_ACTION: &str = "s3:GetObject";

/// The `source` and `detail-type` a Glue crawl's state change is published under.
const GLUE_EVENT_SOURCE: &str = "aws.glue";
const CRAWLER_STATE_CHANGE: &str = "Glue Crawler State Change";

/// The bus an AWS service's own events go onto, which is the bus a rule carrying no
/// `EventBusName` is created on.
const DEFAULT_BUS: &str = "default";

/// The `CrawlerState` a crawler with no run in it reports, and the `LastCrawlStatus` a crawl
/// that read what it was permitted comes to rest in.
const CRAWLER_READY: &str = "READY";
const LAST_CRAWL_SUCCEEDED: &str = "SUCCEEDED";

/// The `detail.state` the EVENT words that same outcome as: this event class carries its own
/// capitalised vocabulary rather than the control plane's upper-case one.
const EVENT_STATE_SUCCEEDED: &str = "Succeeded";

/// `detail.state` of a crawl that could not be carried out, which the negative control's rule
/// selects on and which no row here produces.
const EVENT_STATE_FAILED: &str = "Failed";

/// How long one condition is waited for, and how often it is looked at. Generous on purpose:
/// what is waited for is the condition rather than a span, so a loaded or a fast host reaches
/// the same verdict.
const CRAWL_DEADLINE: Duration = Duration::from_secs(180);
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The data every target below holds: comma-separated rows with a header.
const ROWS_CSV: &str = "id,name\n1,alice\n2,bob\n";

/// The columns that data carries, as the catalog holds them — the header's names, with the
/// types read off the first row.
const ROWS_COLUMNS: [(&str, &str); 2] = [("id", "bigint"), ("name", "string")];

/// The key of the object each row's target holds, under that row's own folder.
const DATA_OBJECT: &str = "rows.csv";

/// What one request answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The body as JSON, or `Value::Null` for one that is not JSON.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator this suite drives: the whole mounted service set over ONE engine, exactly as
/// `core/router/src/lib.rs` assembles it for the binary.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// Build it. Request authentication is off, as in the suites beside this one: each call
    /// carries a credential scope that names the service it is for and no computed signature.
    fn start() -> Self {
        let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
        router.set_request_authentication(false);
        Self { router }
    }

    /// Dispatch one request as it arrives on the wire.
    async fn send(
        &self,
        method: &str,
        host: &str,
        signing_name: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDCRAWLBOUNDARY/20260101/{REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=not-verified"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::from(body.to_owned()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        Answer {
            status: response.status().as_u16(),
            body: String::from_utf8_lossy(response.body()).into_owned(),
        }
    }

    /// One Amazon S3 REST call, path-style as the emulator serves it — the endpoint a crawl
    /// reads its targets through.
    async fn s3(&self, method: &str, path: &str, body: &str) -> Answer {
        let host = format!("s3.{REGION}.amazonaws.com");
        self.send(
            method,
            &host,
            "s3",
            path,
            &[("content-type", "application/octet-stream")],
            body,
        )
        .await
    }

    /// One IAM `awsQuery` action, which must be served.
    async fn iam(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in members {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answered = self
            .send(
                "POST",
                "iam.amazonaws.com",
                "iam",
                "/",
                &[("content-type", "application/x-www-form-urlencoded")],
                &body,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: IAM {action} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.body
    }

    /// One `X-Amz-Target` RPC call, and what it answered.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> Answer {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        self.send(
            "POST",
            &host,
            signing_name,
            "/",
            &[
                (
                    "content-type",
                    &format!("application/x-amz-json-{json_version}"),
                ),
                ("x-amz-target", &format!("{target_prefix}.{operation}")),
            ],
            &body.to_string(),
        )
        .await
    }

    /// One AWS Glue call (`awsJson1_1`, `AWSGlue.<Operation>`), and what it answered.
    async fn glue(&self, operation: &str, body: &Value) -> Answer {
        self.rpc("glue", "AWSGlue", "1.1", operation, body).await
    }

    /// One AWS Glue call that must be served, and the payload it answered with.
    async fn glue_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answered = self.glue(operation, body).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: Glue {operation} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must be served.
    async fn events(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answered = self.rpc("events", "AWSEvents", "1.1", operation, body).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: EventBridge {operation} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must be served.
    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answered = self.rpc("sqs", "AmazonSQS", "1.0", operation, body).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: Amazon SQS {operation} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// Create `bucket` through Amazon S3's own `CreateBucket`.
    async fn create_bucket(&self, case: &str, bucket: &str) {
        let created = self
            .s3(
                "PUT",
                &format!("/{bucket}"),
                &format!(
                    "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
                     <LocationConstraint>{REGION}</LocationConstraint>\
                     </CreateBucketConfiguration>"
                ),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket {bucket} must be created, got {} with {}",
            created.status, created.body
        );
    }

    /// Write `body` at `key` in `bucket`.
    async fn put_object(&self, case: &str, bucket: &str, key: &str, body: &str) {
        let written = self.s3("PUT", &format!("/{bucket}/{key}"), body).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: {key} must be written, got {} with {}",
            written.status, written.body
        );
    }

    /// Create `role_name` trusting Glue with the identity document `grants` as its one inline
    /// policy, and answer the ARN IAM minted for it.
    async fn create_role(&self, case: &str, role_name: &str, grants: &Value) -> String {
        let created = self
            .iam(
                case,
                "CreateRole",
                &[
                    ("RoleName", role_name),
                    ("AssumeRolePolicyDocument", &admits_glue().to_string()),
                ],
            )
            .await;
        let arn = element(&created, "Arn");
        assert!(
            !arn.is_empty(),
            "case {case:?}: CreateRole must answer the role's ARN, got {created}"
        );
        self.put_role_policy(case, role_name, grants).await;
        arn
    }

    /// Write the role's one inline policy, which is what every row varies.
    async fn put_role_policy(&self, case: &str, role_name: &str, grants: &Value) {
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", "crawl-target-access"),
                ("PolicyDocument", &grants.to_string()),
            ],
        )
        .await;
    }

    /// Create the database a crawl publishes into.
    async fn create_database(&self, case: &str, database: &str) {
        self.glue_ok(
            case,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": database } }),
        )
        .await;
    }

    /// Create a crawler over one Amazon S3 folder, running as `role`.
    async fn create_crawler(
        &self,
        case: &str,
        crawler: &str,
        role: &str,
        database: &str,
        target: &str,
    ) {
        self.glue_ok(
            case,
            "CreateCrawler",
            &json!({
                "Name": crawler,
                "Role": role,
                "DatabaseName": database,
                "Targets": { "S3Targets": [ { "Path": target } ] },
            }),
        )
        .await;
    }

    /// `StartCrawler`, which must be accepted with the operation's EMPTY output.
    async fn start_crawler(&self, case: &str, crawler: &str) {
        let answered = self.glue("StartCrawler", &json!({ "Name": crawler })).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from StartCrawler({crawler:?}), got {} with {}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.json(),
            json!({}),
            "case {case:?}: StartCrawler reports no member of its own, got {}",
            answered.body
        );
    }

    /// The `Crawler` once the finish the clock owed its crawl has been materialised: polled for
    /// the CONDITION — the crawler's own state — rather than for any elapsed time.
    async fn crawler_at_rest(&self, case: &str, crawler: &str) -> Value {
        let deadline = Instant::now() + CRAWL_DEADLINE;
        loop {
            let reported = self
                .glue_ok(case, "GetCrawler", &json!({ "Name": crawler }))
                .await;
            let reported = reported["Crawler"].clone();
            if reported["State"].as_str() == Some(CRAWLER_READY)
                && reported.pointer("/LastCrawl/Status").is_some()
            {
                return reported;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the crawl of {crawler:?} never came to rest; the crawler \
                 reports {reported}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }

    /// The tables `GetTables` reports for `database`, in name order.
    async fn tables_of(&self, case: &str, database: &str) -> Vec<Value> {
        let reported = self
            .glue_ok(case, "GetTables", &json!({ "DatabaseName": database }))
            .await;
        let mut tables: Vec<Value> = reported["TableList"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        tables.sort_by_key(|table| table["Name"].as_str().unwrap_or_default().to_string());
        tables
    }

    /// Create a queue called `name` and grant EventBridge the delivery onto it; answers its url
    /// and the ARN a rule names it by.
    async fn delivery_queue(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({ "QueueName": name }))
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateQueue must answer a url, got {created}"))
            .to_string();
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the queue must report its ARN, got {attributes}")
            })
            .to_string();
        // Every queue here grants the delivery, so what a queue holds follows from the rule's
        // pattern alone and never from its own policy.
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowEventBridge",
                "Effect": "Allow",
                "Principal": { "Service": EVENTS_PRINCIPAL },
                "Action": "sqs:SendMessage",
                "Resource": &arn,
            }],
        })
        .to_string();
        self.sqs(
            case,
            "SetQueueAttributes",
            &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
        )
        .await;
        (url, arn)
    }

    /// A rule on the default bus carrying `pattern`, delivering to `target_arn`.
    async fn rule_to(&self, case: &str, rule: &str, pattern: &Value, target_arn: &str) {
        self.events(
            case,
            "PutRule",
            &json!({
                "Name": rule,
                "EventBusName": DEFAULT_BUS,
                "EventPattern": pattern.to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let targeted = self
            .events(
                case,
                "PutTargets",
                &json!({
                    "Rule": rule,
                    "EventBusName": DEFAULT_BUS,
                    "Targets": [ { "Id": "queue", "Arn": target_arn } ],
                }),
            )
            .await;
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: the rule's target must be accepted, got {targeted}"
        );
    }

    /// Every message now waiting on the queue at `url`, as the envelopes their bodies carry.
    async fn drain(&self, case: &str, url: &str) -> Vec<Value> {
        let received = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        received["Messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|message| {
                serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                    .unwrap_or(Value::Null)
            })
            .collect()
    }

    /// The envelope the queue at `url` receives, waited for by polling the queue itself.
    async fn delivered_envelope(&self, case: &str, url: &str) -> Value {
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            if let Some(envelope) = self.drain(case, url).await.into_iter().next() {
                return envelope;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the queue received no delivery of the crawl's state change"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
}

/// The trust document every role here carries: Glue has to be able to assume the role a crawl
/// runs as, so this is constant across the rows — what they vary is the IDENTITY document.
fn admits_glue() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": GLUE_PRINCIPAL },
            "Action": ASSUME_ROLE,
        }],
    })
}

/// The ARN of a BUCKET, which is the resource a listing is authorized against.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// The ARN of the OBJECTS in a bucket, which is the resource an object read is authorized
/// against.
fn objects_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}/*")
}

/// One statement: `effect` the `action` list on `resource`, optionally under `condition`.
fn statement(effect: &str, action: Value, resource: Value, condition: Option<Value>) -> Value {
    let mut written = json!({ "Effect": effect, "Action": action, "Resource": resource });
    if let Some(condition) = condition {
        written["Condition"] = condition;
    }
    written
}

/// An identity document carrying `statements`.
fn document(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The two statements that admit the whole walk of `bucket`: the listing of the bucket and the
/// read of its objects.
fn walk_statements(bucket: &str) -> Vec<Value> {
    vec![
        statement(
            "Allow",
            json!(LIST_ACTION),
            json!(bucket_arn(bucket)),
            None,
        ),
        statement(
            "Allow",
            json!(READ_ACTION),
            json!(objects_arn(bucket)),
            None,
        ),
    ]
}

/// The document that admits the whole walk of `bucket`.
fn admits_the_walk(bucket: &str) -> Value {
    document(walk_statements(bucket))
}

/// One row of the authorization table: the identity document the crawl role carries, and
/// whether the walk of the row's own target is therefore permitted.
struct TargetRow {
    case: &'static str,
    /// The label the row's folder, role, database and crawler are named after, which is also
    /// the name the catalog holds the dataset behind that folder under (the last folder level
    /// of the target's path).
    label: &'static str,
    /// The row's identity document, as a function of the bucket the target sits in and the
    /// folder the target names.
    grants: fn(&str, &str) -> Value,
    /// Whether the documents admit BOTH calls the walk of that target makes.
    walks: bool,
}

/// The rows: which of the two calls the documents withhold, and how they withhold it.
fn target_rows() -> Vec<TargetRow> {
    vec![
        TargetRow {
            case: "documents admitting both the listing and the object read",
            label: "admitted",
            grants: |bucket, _folder| admits_the_walk(bucket),
            walks: true,
        },
        TargetRow {
            case: "documents covering another bucket only",
            label: "elsewhere",
            grants: |_bucket, _folder| admits_the_walk("glue-crawl-boundary-other"),
            walks: false,
        },
        TargetRow {
            case: "documents denying the object read explicitly",
            label: "read_denied",
            grants: |bucket, _folder| {
                let mut statements = walk_statements(bucket);
                statements.push(statement(
                    "Deny",
                    json!(READ_ACTION),
                    json!(objects_arn(bucket)),
                    None,
                ));
                document(statements)
            },
            walks: false,
        },
        TargetRow {
            case: "documents denying the listing explicitly",
            label: "list_denied",
            grants: |bucket, _folder| {
                let mut statements = walk_statements(bucket);
                statements.push(statement(
                    "Deny",
                    json!(LIST_ACTION),
                    json!(bucket_arn(bucket)),
                    None,
                ));
                document(statements)
            },
            walks: false,
        },
        TargetRow {
            case: "a listing grant narrowed to another prefix",
            label: "narrowed",
            grants: |bucket, folder| {
                document(vec![
                    // The listing a walk makes carries the target's own prefix as `s3:prefix`,
                    // so a grant written for some other prefix decides as narrowly as it is
                    // written.
                    statement(
                        "Allow",
                        json!(LIST_ACTION),
                        json!(bucket_arn(bucket)),
                        Some(json!({
                            "StringEquals": { "s3:prefix": format!("not-{folder}/") }
                        })),
                    ),
                    statement(
                        "Allow",
                        json!(READ_ACTION),
                        json!(objects_arn(bucket)),
                        None,
                    ),
                ])
            },
            walks: false,
        },
    ]
}

/// The `(Name, Type)` pairs of one table's `Columns`, in the order the catalog holds them.
fn named_types(table: &Value) -> Vec<(String, String)> {
    table
        .pointer("/StorageDescriptor/Columns")
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .map(|column| {
                    (
                        column["Name"].as_str().unwrap_or_default().to_string(),
                        column["Type"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Run `body` on a thread sized for the router assembly, and carry a row's failure out of it.
fn on_the_emulator<Body>(name: &'static str, body: Body)
where
    Body: FnOnce(&Emulator, &tokio::runtime::Runtime) + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let emulator = Emulator::start();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(4)
                .build()
                .expect("a multi-thread runtime");
            body(&emulator, &runtime);
        })
        .expect("spawn the suite's thread")
        .join()
        .unwrap_or_else(|_| panic!("{name}"));
}

/// RULE 1: the documents the account holds on the crawl role decide what the walk reads, so a
/// table is published exactly for a target both of the walk's calls are admitted on — and the
/// crawl comes to rest `SUCCEEDED` either way.
#[test]
fn a_crawl_publishes_a_table_only_for_a_target_its_role_may_read() {
    on_the_emulator("xsvc-crawl-boundary", |emulator, runtime| {
        runtime.block_on(async {
            let bucket = "glue-crawl-boundary";
            emulator.create_bucket("setup", bucket).await;

            for row in target_rows() {
                let case = row.case;
                let label = row.label;
                let key = format!("{label}/{DATA_OBJECT}");
                emulator.put_object(case, bucket, &key, ROWS_CSV).await;

                let role_name = format!("glue-crawl-{label}");
                let role = emulator
                    .create_role(case, &role_name, &(row.grants)(bucket, label))
                    .await;
                let database = format!("glue_crawl_{label}_db");
                let crawler = format!("glue-crawl-{label}");
                let target = format!("s3://{bucket}/{label}/");
                emulator.create_database(case, &database).await;
                emulator
                    .create_crawler(case, &crawler, &role, &database, &target)
                    .await;
                let before = emulator.tables_of(case, &database).await;
                assert!(
                    before.is_empty(),
                    "case {case:?}: the create published no table of its own, got {before:?}"
                );

                emulator.start_crawler(case, &crawler).await;
                let at_rest = emulator.crawler_at_rest(case, &crawler).await;
                // The crawl was CARRIED OUT whatever the documents withheld: it reached every
                // target it was permitted, and a target it was not holds no dataset it could
                // have described.
                assert_eq!(
                    at_rest.pointer("/LastCrawl/Status").and_then(Value::as_str),
                    Some(LAST_CRAWL_SUCCEEDED),
                    "case {case:?}: expected the crawl to come to rest {LAST_CRAWL_SUCCEEDED}, \
                     got {at_rest}"
                );

                let published = emulator.tables_of(case, &database).await;
                let names: Vec<&str> = published
                    .iter()
                    .map(|table| table["Name"].as_str().unwrap_or_default())
                    .collect();
                if !row.walks {
                    assert!(
                        published.is_empty(),
                        "case {case:?}: a target the role's documents withhold publishes no \
                         table, got {names:?}"
                    );
                    continue;
                }
                assert_eq!(
                    names,
                    vec![label],
                    "case {case:?}: expected the dataset behind {target:?} to be published as \
                     {label:?}, got {names:?}"
                );
                let table = &published[0];
                assert_eq!(
                    table.pointer("/StorageDescriptor/Location").and_then(Value::as_str),
                    Some(target.as_str()),
                    "case {case:?}: expected the table located under its target, got {table}"
                );
                let expected: Vec<(String, String)> = ROWS_COLUMNS
                    .iter()
                    .map(|(name, kind)| ((*name).to_string(), (*kind).to_string()))
                    .collect();
                let actual = named_types(table);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected the columns {expected:?} read off the data, got \
                     {actual:?}"
                );
            }
        });
    });
}

/// RULE 1, its second half: granting the read the documents withheld and starting the SAME
/// crawler again publishes the table the first crawl could not.
#[test]
fn granting_the_withheld_read_lets_the_same_crawler_publish_its_table() {
    on_the_emulator("xsvc-crawl-regrant", |emulator, runtime| {
        runtime.block_on(async {
            let case = "a role granted the read between two crawls of one crawler";
            let bucket = "glue-crawl-regrant";
            let label = "regranted";
            emulator.create_bucket(case, bucket).await;
            emulator
                .put_object(case, bucket, &format!("{label}/{DATA_OBJECT}"), ROWS_CSV)
                .await;

            // The role starts out with documents that cover another bucket only, so nothing is
            // in force that admits either call this crawl makes.
            let role_name = "glue-crawl-regranted";
            let role = emulator
                .create_role(
                    case,
                    role_name,
                    &admits_the_walk("glue-crawl-regrant-other"),
                )
                .await;
            let database = "glue_crawl_regranted_db";
            let crawler = "glue-crawl-regranted";
            let target = format!("s3://{bucket}/{label}/");
            emulator.create_database(case, database).await;
            emulator
                .create_crawler(case, crawler, &role, database, &target)
                .await;

            emulator.start_crawler(case, crawler).await;
            let withheld = emulator.crawler_at_rest(case, crawler).await;
            assert_eq!(
                withheld.pointer("/LastCrawl/Status").and_then(Value::as_str),
                Some(LAST_CRAWL_SUCCEEDED),
                "case {case:?}: the first crawl was carried out, got {withheld}"
            );
            let nothing = emulator.tables_of(case, database).await;
            assert!(
                nothing.is_empty(),
                "case {case:?}: the first crawl read nothing, so it published nothing, got \
                 {nothing:?}"
            );

            // The ONE thing that changes: the role's own document now admits the walk.
            emulator
                .put_role_policy(case, role_name, &admits_the_walk(bucket))
                .await;
            emulator.start_crawler(case, crawler).await;
            let granted = emulator.crawler_at_rest(case, crawler).await;
            assert_eq!(
                granted.pointer("/LastCrawl/Status").and_then(Value::as_str),
                Some(LAST_CRAWL_SUCCEEDED),
                "case {case:?}: the second crawl was carried out, got {granted}"
            );
            let published = emulator.tables_of(case, database).await;
            let names: Vec<&str> = published
                .iter()
                .map(|table| table["Name"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                names,
                vec![label],
                "case {case:?}: expected the re-run of the same crawler to publish {label:?}, \
                 got {names:?}"
            );
        });
    });
}

/// One row of the delivery table: the dimension the rule's pattern filters the envelope on, and
/// whether an envelope of a SUCCEEDED crawl matches it.
struct DeliveryRow {
    case: &'static str,
    label: &'static str,
    /// The `detail` clause the pattern carries beyond the source and the detail type, as a
    /// function of the crawler's own name. `None` filters on the source and detail type alone.
    detail: Option<fn(&str) -> Value>,
    /// Whether the crawl's own state change matches that pattern.
    matches: bool,
}

/// RULE 2: the state change a finished crawl publishes reaches the targets of every rule whose
/// pattern matches it — and no target of a rule whose pattern selects another state.
#[test]
fn a_finished_crawl_reaches_the_targets_of_every_rule_matching_its_state_change() {
    on_the_emulator("xsvc-crawl-event", |emulator, runtime| {
        runtime.block_on(async {
            let case = "a crawl that read its target";
            let bucket = "glue-crawl-events";
            let label = "catalogued";
            emulator.create_bucket(case, bucket).await;
            emulator
                .put_object(case, bucket, &format!("{label}/{DATA_OBJECT}"), ROWS_CSV)
                .await;
            let role = emulator
                .create_role(case, "glue-crawl-events", &admits_the_walk(bucket))
                .await;
            let database = "glue_crawl_events_db";
            let crawler = "glue-crawl-events";
            emulator.create_database(case, database).await;
            emulator
                .create_crawler(
                    case,
                    crawler,
                    &role,
                    database,
                    &format!("s3://{bucket}/{label}/"),
                )
                .await;

            let rows = [
                DeliveryRow {
                    case: "a pattern naming the crawler and the state inside the detail",
                    label: "named",
                    detail: Some(|crawler| {
                        json!({ "crawlerName": [crawler], "state": [EVENT_STATE_SUCCEEDED] })
                    }),
                    matches: true,
                },
                DeliveryRow {
                    case: "a pattern filtering on the source and the detail type alone",
                    label: "broad",
                    detail: None,
                    matches: true,
                },
                DeliveryRow {
                    case: "a pattern selecting a crawl that failed",
                    label: "failed",
                    detail: Some(|_crawler| json!({ "state": [EVENT_STATE_FAILED] })),
                    matches: false,
                },
            ];

            // Every rule is in place BEFORE the crawl is started, each with its own queue, so
            // one publication is fanned out to all of them.
            let mut queues: Vec<(&DeliveryRow, String)> = Vec::new();
            for row in &rows {
                let (url, arn) = emulator
                    .delivery_queue(row.case, &format!("glue-crawl-event-{}", row.label))
                    .await;
                let mut pattern = json!({
                    "source": [GLUE_EVENT_SOURCE],
                    "detail-type": [CRAWLER_STATE_CHANGE],
                });
                if let Some(clause) = row.detail {
                    pattern["detail"] = clause(crawler);
                }
                emulator
                    .rule_to(
                        row.case,
                        &format!("glue-crawl-event-rule-{}", row.label),
                        &pattern,
                        &arn,
                    )
                    .await;
                queues.push((row, url));
            }

            emulator.start_crawler(case, crawler).await;

            // The matching rules' queues receive the envelope with NO Glue call in between: the
            // publication is the service's own clock, not a side effect of a read.
            for (row, url) in queues.iter().filter(|(row, _)| row.matches) {
                let case = row.case;
                let envelope = emulator.delivered_envelope(case, url).await;
                for (pointer, expected) in [
                    ("/source", json!(GLUE_EVENT_SOURCE)),
                    ("/detail-type", json!(CRAWLER_STATE_CHANGE)),
                    ("/account", json!(ACCOUNT)),
                    ("/region", json!(REGION)),
                    ("/detail/crawlerName", json!(crawler)),
                    ("/detail/state", json!(EVENT_STATE_SUCCEEDED)),
                ] {
                    let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                    assert_eq!(
                        actual, expected,
                        "case {case:?}: expected {pointer} to be {expected}, got {actual} in \
                         {envelope}"
                    );
                }
            }

            // The non-matching rule's queue is read only AFTER every matching one has its own
            // copy of the same envelope, so what it holds is the pattern's doing rather than a
            // look taken too early.
            for (row, url) in queues.iter().filter(|(row, _)| !row.matches) {
                let case = row.case;
                let held = emulator.drain(case, url).await;
                assert!(
                    held.is_empty(),
                    "case {case:?}: a rule whose pattern selects another state delivers \
                     nothing, got {held:?}"
                );
            }

            // The crawl the event was about is the one a read of the crawler reports, and the
            // catalog holds what it published: the event is not a second, invented account of
            // the crawl.
            let at_rest = emulator.crawler_at_rest(case, crawler).await;
            assert_eq!(
                at_rest.pointer("/LastCrawl/Status").and_then(Value::as_str),
                Some(LAST_CRAWL_SUCCEEDED),
                "case {case:?}: expected the crawl the event reported, got {at_rest}"
            );
            let published = emulator.tables_of(case, database).await;
            let names: Vec<&str> = published
                .iter()
                .map(|table| table["Name"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                names,
                vec![label],
                "case {case:?}: expected the crawl to publish {label:?}, got {names:?}"
            );
        });
    });
}
