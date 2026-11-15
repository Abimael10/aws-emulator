// SPDX-License-Identifier: CC0-1.0

//! The references an environment's configuration carries, resolved against the services that
//! OWN the resources they name — end to end over HTTP, with those services mounted on the
//! same engine.
//!
//! RULE: a configuration member that NAMES a resource of another service (`ExecutionRoleArn`
//! in IAM, `SourceBucketArn` in Amazon S3, `NetworkConfiguration.SecurityGroupIds` in Amazon
//! EC2) is resolved against that service before a create or an update stores it: a reference
//! the owning service ANSWERS is unusable is refused with HTTP 400 `ValidationException`,
//! naming the member at fault, and leaves the stored record exactly as it was; a reference it
//! resolves is stored and reads back. A reference nothing answered about — no such service
//! mounted to ask, or an execution role this account's IAM holds nothing under — is refused
//! for nothing: a refusal has to be grounded in what the owning service said.
//!
//! The rows differ along what the rule generalises over: WHICH member carries the reference,
//! which owning service answers for it, what that service answered (a resource it holds, one
//! it holds nothing under, a role whose trust policy names another service only, a bucket it
//! holds in another Region), how many references one member carries, and whether the request
//! is a create or an update. Every resource a row resolves against is created through the
//! owning service's OWN operation on the same engine, so what MWAA resolves is what that
//! service holds.
//!
//! The last table pins the opposite of a refusal: `AirflowVersion` moves in EITHER direction.
//! "To upgrade your environment, specify a newer version of Apache Airflow supported by
//! Amazon MWAA. To downgrade your environment, specify an older version of Apache Airflow
//! supported by Amazon MWAA"
//! (`smithy_query(query="doc", shape="UpdateEnvironmentInput", member="AirflowVersion")`), so
//! a patch that moves an environment BACK to an earlier supported release is served.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The canonical emulator account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The IAM API version an awsQuery body carries for that service.
const IAM_VERSION: &str = "2010-05-08";

/// The Amazon EC2 API version an ec2Query body carries.
const EC2_VERSION: &str = "2016-11-15";

/// A trust policy admitting the service principal an Amazon MWAA environment's execution
/// role is assumed under.
const TRUST_ADMITTING_MWAA: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"airflow-env.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// A trust policy admitting ANOTHER service's principal and nothing else: a role of this
/// account that exists and cannot be assumed to run an environment.
const TRUST_ADMITTING_ANOTHER_SERVICE: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// How long a poll waits for a status the clock owes before asking again, and the deadline it
/// gives up at. The deadline is generous: it bounds a condition that has to HOLD, never how
/// fast the host runs.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// Whether the services that own the resources a configuration names are mounted beside this
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Peers {
    /// IAM, Amazon S3 and Amazon EC2 on the same engine: every reference is resolvable.
    Mounted,
    /// This service alone, which is a deployment where nothing can answer about a reference.
    Absent,
}

/// One HTTP answer, reduced to what these tests assert on.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The modelled error code, which restJson1 carries in `__type`.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator these tests drive: one shared engine, this service, and — for
/// [`Peers::Mounted`] — the three services that own the resources a configuration names.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new(peers: Peers) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing here signs: the `Authorization` header carries only the credential scope,
        // which is what routes a request to its service.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_mwaa_amazonmwaa::EmulatedService::new(Arc::clone(&engine)),
        );
        if peers == Peers::Mounted {
            router.register_service(
                service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                    Arc::clone(&engine),
                ),
            );
            router.register_service(
                service_com_amazonaws_s3_amazons3::EmulatedService::new(Arc::clone(&engine)),
            );
            router.register_service(
                service_com_amazonaws_ec2_amazonec2::EmulatedService::new(engine),
            );
        }
        Self { router }
    }

    /// One restJson1 call to this service.
    async fn mwaa(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let payload = body
            .map(|value| serde_json::to_vec(&value).expect("a serialisable body"))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("api.airflow.{REGION}.amazonaws.com"))
            .header("authorization", authorization("airflow"))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body().to_vec();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Answer { status, body }
    }

    async fn create(&self, name: &str, body: &Value) -> Answer {
        self.mwaa("PUT", &format!("/environments/{name}"), Some(body.clone()))
            .await
    }

    async fn update(&self, name: &str, patch: &Value) -> Answer {
        self.mwaa("PATCH", &format!("/environments/{name}"), Some(patch.clone()))
            .await
    }

    async fn get(&self, name: &str) -> Answer {
        self.mwaa("GET", &format!("/environments/{name}"), None).await
    }

    /// The `Environment` of a successful `GetEnvironment`.
    async fn environment(&self, case: &str, name: &str) -> Value {
        let answer = self.get(name).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected GetEnvironment 200 for {name:?}, got {} {}",
            answer.status, answer.body
        );
        answer
            .body
            .get("Environment")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: GetEnvironment answered no Environment"))
    }

    /// An environment created from `body`, polled until the clock has settled it into
    /// `AVAILABLE` — the status an update is accepted in. Waits for the CONDITION, with a
    /// generous deadline.
    async fn available_environment(&self, case: &str, name: &str, body: &Value) {
        let created = self.create(name, body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the fixture create of {name:?} must succeed, got {} {}",
            created.status, created.body
        );
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let environment = self.environment(case, name).await;
            let status = environment
                .get("Status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if status == "AVAILABLE" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {name:?} never reached AVAILABLE, last status {status:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// One awsQuery/ec2Query call that must succeed, with its XML body.
    async fn query_ok(
        &self,
        case: &str,
        signing_name: &str,
        version: &str,
        action: &str,
        parameters: &[(&str, String)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in parameters {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization(signing_name))
            .body(bytes::Bytes::from(body))
            .expect("a well-formed query request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture {action} must succeed, got {status} with {answered}"
        );
        answered
    }

    /// A role of this account, created through IAM's own `CreateRole`: answers the ARN IAM
    /// reported for it.
    async fn role(&self, case: &str, role_name: &str, trust_policy: &str) -> String {
        let created = self
            .query_ok(
                case,
                "iam",
                IAM_VERSION,
                "CreateRole",
                &[
                    ("RoleName", role_name.to_string()),
                    ("AssumeRolePolicyDocument", trust_policy.to_string()),
                ],
            )
            .await;
        xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateRole answered no Arn: {created}"))
    }

    /// A bucket of this account, created through Amazon S3's own `CreateBucket`: answers the
    /// bucket ARN a configuration names it by.
    async fn bucket(&self, case: &str, bucket: &str) -> String {
        let request = http::Request::builder()
            .method("PUT")
            .uri(format!("/{bucket}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateBucket of {bucket:?} must succeed, got {status} \
             with {}",
            String::from_utf8_lossy(response.body())
        );
        format!("arn:aws:s3:::{bucket}")
    }

    /// A security group of this account, created through Amazon EC2's own
    /// `CreateSecurityGroup` in the Region's default VPC: answers the group id.
    async fn security_group(&self, case: &str, group_name: &str) -> String {
        let created = self
            .query_ok(
                case,
                "ec2",
                EC2_VERSION,
                "CreateSecurityGroup",
                &[
                    ("GroupName", group_name.to_string()),
                    ("GroupDescription", "the environment workers".to_string()),
                ],
            )
            .await;
        xml_element(&created, "groupId").unwrap_or_else(|| {
            panic!("case {case:?}: CreateSecurityGroup answered no groupId: {created}")
        })
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request to its service. The signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a trust policy's braces and quotes survive the round
/// trip.
///
/// A copy: an integration-test module cannot be imported across crates, so every service
/// suite that drives an awsQuery service holds its own.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The text of the first `<name>…</name>` element of a query response. A copy, for the same
/// reason as [`form_encode`].
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// What a row expects of the request it drives.
#[derive(Debug, Clone)]
enum Expected {
    /// Served: the patch is applied and reads back.
    Served,
    /// Refused with a `ValidationException` naming this member.
    Refused(&'static str),
}

/// One row of the resolution table: the case, the patch it sends, and what it expects.
struct Row {
    case: String,
    patch: Value,
    expected: Expected,
}

/// The references a configuration carries, as the members a create states them in.
fn create_body(role_arn: &str, bucket_arn: &str, group_id: &str) -> Value {
    json!({
        "ExecutionRoleArn": role_arn,
        "SourceBucketArn": bucket_arn,
        "DagS3Path": "dags",
        "NetworkConfiguration": {
            // Subnet ids are not references this service resolves — an update cannot even
            // replace them — so a row's placement is stated and never created.
            "SubnetIds": ["subnet-aaa111", "subnet-bbb222"],
            "SecurityGroupIds": [group_id],
        },
        "EnvironmentClass": "mw1.micro",
        "MinWorkers": 1,
        "MaxWorkers": 1,
    })
}

/// `EnvironmentName` is `^[a-zA-Z][0-9a-zA-Z-_]*$`, so a row's environment is named from its
/// table and its index.
fn env_name(table: &str, index: usize) -> String {
    format!("{table}Row{index}")
}

/// The members of the stored record a patch of a reference can move, as a read reports them.
fn references_of(environment: &Value) -> Value {
    json!({
        "ExecutionRoleArn": environment.get("ExecutionRoleArn").cloned(),
        "SourceBucketArn": environment.get("SourceBucketArn").cloned(),
        "SecurityGroupIds": environment
            .get("NetworkConfiguration")
            .and_then(|network| network.get("SecurityGroupIds"))
            .cloned(),
    })
}

/// The references `patch` would leave on the record, over the ones it already carries.
fn merged_references(stored: &Value, patch: &Value) -> Value {
    let mut expected = references_of(stored);
    let target = expected.as_object_mut().expect("an object");
    for member in ["ExecutionRoleArn", "SourceBucketArn"] {
        if let Some(value) = patch.get(member) {
            target.insert(member.to_string(), value.clone());
        }
    }
    if let Some(value) = patch
        .get("NetworkConfiguration")
        .and_then(|network| network.get("SecurityGroupIds"))
    {
        target.insert("SecurityGroupIds".to_string(), value.clone());
    }
    expected
}

/// Drive `rows` through `UpdateEnvironment` on a harness whose peers are `peers`: each row
/// gets its own environment, created from references that resolve, and is then patched.
async fn assert_updates(table: &str, peers: Peers, rows_of: fn(&Fixtures) -> Vec<Row>) {
    let harness = Harness::new(peers);
    let fixtures = Fixtures::provision(&harness, table, peers).await;
    let rows = rows_of(&fixtures);
    for (index, row) in rows.iter().enumerate() {
        let name = env_name(table, index);
        let case = &row.case;
        harness
            .available_environment(case, &name, &fixtures.create_body())
            .await;
        let before = harness.environment(case, &name).await;

        let answer = harness.update(&name, &row.patch).await;
        match row.expected {
            Expected::Served => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the patch to be served, got {} {}",
                    answer.status, answer.body
                );
                // The applied configuration is what a later read reports, so the patch is
                // read back once the update's phases have settled.
                let expected = merged_references(&before, &row.patch);
                let deadline = Instant::now() + POLL_DEADLINE;
                loop {
                    let reported = references_of(&harness.environment(case, &name).await);
                    if reported == expected {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "case {case:?}: expected the record to report {expected}, got {reported}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
            Expected::Refused(member) => {
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: expected HTTP 400, got {} {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code(),
                    "ValidationException",
                    "case {case:?}: expected a ValidationException, got {}",
                    answer.body
                );
                let message = answer.message();
                assert!(
                    message.contains(member),
                    "case {case:?}: expected the refusal to name {member:?}, got {message:?}"
                );
                let after = references_of(&harness.environment(case, &name).await);
                assert_eq!(
                    after,
                    references_of(&before),
                    "case {case:?}: a refused patch must leave the record alone, got {after}"
                );
            }
        }
    }
}

/// The resources of OTHER services a table's rows resolve against, each created through the
/// service that owns it.
struct Fixtures {
    /// A role IAM holds whose trust policy admits Amazon MWAA.
    assumable_role_arn: String,
    /// A role IAM holds whose trust policy admits another service only.
    untrusting_role_arn: String,
    /// A bucket Amazon S3 holds, and a second one of it.
    bucket_arn: String,
    other_bucket_arn: String,
    /// A security group Amazon EC2 holds, and a second one of it.
    group_id: String,
    other_group_id: String,
}

impl Fixtures {
    /// Create every resource through its owning service — or, where those services are not
    /// mounted, state the same values without creating anything, which is the case where
    /// nothing can answer about them.
    async fn provision(harness: &Harness, table: &str, peers: Peers) -> Self {
        if peers == Peers::Absent {
            return Self {
                assumable_role_arn: format!("arn:aws:iam::{ACCOUNT}:role/{table}-assumable"),
                untrusting_role_arn: format!("arn:aws:iam::{ACCOUNT}:role/{table}-untrusting"),
                bucket_arn: "arn:aws:s3:::mwaa-dags-first".to_string(),
                other_bucket_arn: "arn:aws:s3:::mwaa-dags-second".to_string(),
                group_id: "sg-aaa111".to_string(),
                other_group_id: "sg-bbb222".to_string(),
            };
        }
        let case = "the fixtures";
        Self {
            assumable_role_arn: harness
                .role(case, &format!("{table}-assumable"), TRUST_ADMITTING_MWAA)
                .await,
            untrusting_role_arn: harness
                .role(
                    case,
                    &format!("{table}-untrusting"),
                    TRUST_ADMITTING_ANOTHER_SERVICE,
                )
                .await,
            bucket_arn: harness.bucket(case, "mwaa-dags-first").await,
            other_bucket_arn: harness.bucket(case, "mwaa-dags-second").await,
            group_id: harness.security_group(case, "mwaa-first").await,
            other_group_id: harness.security_group(case, "mwaa-second").await,
        }
    }

    /// A create whose every reference resolves.
    fn create_body(&self) -> Value {
        create_body(&self.assumable_role_arn, &self.bucket_arn, &self.group_id)
    }
}

/// The rows that assert the rule, whichever harness they are driven on: the same patches read
/// one way where the owning service answers and another way where none does.
fn resolution_rows(fixtures: &Fixtures) -> Vec<(Row, Expected)> {
    // (the row as the mounted harness expects it, what the same patch expects where nothing
    // can answer about the reference)
    vec![
        (
            Row {
                case: "the execution role IAM holds whose trust policy admits Amazon MWAA"
                    .to_string(),
                patch: json!({"ExecutionRoleArn": fixtures.assumable_role_arn}),
                expected: Expected::Served,
            },
            Expected::Served,
        ),
        (
            Row {
                case: "an execution role IAM holds whose trust policy admits another service \
                       only"
                    .to_string(),
                patch: json!({"ExecutionRoleArn": fixtures.untrusting_role_arn}),
                expected: Expected::Refused("ExecutionRoleArn"),
            },
            Expected::Served,
        ),
        (
            Row {
                // Not a refusal anywhere: an execution role is routinely provisioned out of
                // band, so IAM holding nothing under the name is evidence about this
                // emulator's store rather than about the role the caller named.
                case: "an execution role this account's IAM holds nothing under".to_string(),
                patch: json!({
                    "ExecutionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/never-created"),
                }),
                expected: Expected::Served,
            },
            Expected::Served,
        ),
        (
            Row {
                case: "a source bucket Amazon S3 holds".to_string(),
                patch: json!({"SourceBucketArn": fixtures.other_bucket_arn}),
                expected: Expected::Served,
            },
            Expected::Served,
        ),
        (
            Row {
                case: "a source bucket Amazon S3 holds nothing under".to_string(),
                patch: json!({"SourceBucketArn": "arn:aws:s3:::mwaa-dags-never-created"}),
                expected: Expected::Refused("SourceBucketArn"),
            },
            Expected::Served,
        ),
        (
            Row {
                case: "the security groups Amazon EC2 holds".to_string(),
                patch: json!({
                    "NetworkConfiguration": {
                        "SecurityGroupIds": [fixtures.group_id, fixtures.other_group_id],
                    },
                }),
                expected: Expected::Served,
            },
            Expected::Served,
        ),
        (
            Row {
                case: "a security group Amazon EC2 has none of".to_string(),
                patch: json!({
                    "NetworkConfiguration": {"SecurityGroupIds": ["sg-never-created"]},
                }),
                expected: Expected::Refused("SecurityGroupIds"),
            },
            Expected::Served,
        ),
        (
            Row {
                case: "a group Amazon EC2 holds beside one it has none of".to_string(),
                patch: json!({
                    "NetworkConfiguration": {
                        "SecurityGroupIds": [fixtures.group_id, "sg-never-created"],
                    },
                }),
                expected: Expected::Refused("SecurityGroupIds"),
            },
            Expected::Served,
        ),
        (
            Row {
                case: "a patch naming none of the three references".to_string(),
                patch: json!({"MaxWorkers": 4}),
                expected: Expected::Served,
            },
            Expected::Served,
        ),
    ]
}

/// The rows as the harness with the owning services mounted expects them.
fn rows_with_peers(fixtures: &Fixtures) -> Vec<Row> {
    resolution_rows(fixtures)
        .into_iter()
        .map(|(row, _)| row)
        .collect()
}

/// The same rows where nothing can answer about a reference: every one of them is served.
fn rows_without_peers(fixtures: &Fixtures) -> Vec<Row> {
    resolution_rows(fixtures)
        .into_iter()
        .map(|(row, expected)| Row { expected, ..row })
        .collect()
}

/// RULE, on the update path: a reference the owning service answers is unusable is refused and
/// changes nothing; one it resolves is stored and reads back.
#[tokio::test]
async fn an_update_resolves_every_reference_it_replaces_against_the_service_that_owns_it() {
    assert_updates("UpdRef", Peers::Mounted, rows_with_peers).await;
}

/// RULE, the other half: a refusal has to be GROUNDED in what the owning service said, so a
/// deployment with no IAM, no Amazon S3 and no Amazon EC2 to ask refuses none of the same
/// patches.
#[tokio::test]
async fn an_update_refuses_nothing_where_no_owning_service_answers() {
    assert_updates("UpdSilent", Peers::Absent, rows_without_peers).await;
}

/// RULE, on the create path: the same resolution runs before anything is stored, so a create
/// naming a reference the owning service refuses leaves NO environment behind.
#[tokio::test]
async fn a_create_resolves_its_references_before_anything_is_stored() {
    let harness = Harness::new(Peers::Mounted);
    let fixtures = Fixtures::provision(&harness, "CreRef", Peers::Mounted).await;

    // (case, the create body, the member a refusal names — `None` for a create that is
    // served). The rows differ along which reference is unresolvable and which service
    // answers for it.
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "every reference resolves",
            fixtures.create_body(),
            None,
        ),
        (
            "a source bucket Amazon S3 holds nothing under",
            create_body(
                &fixtures.assumable_role_arn,
                "arn:aws:s3:::mwaa-dags-never-created",
                &fixtures.group_id,
            ),
            Some("SourceBucketArn"),
        ),
        (
            "an execution role IAM holds that admits another service only",
            create_body(
                &fixtures.untrusting_role_arn,
                &fixtures.bucket_arn,
                &fixtures.group_id,
            ),
            Some("ExecutionRoleArn"),
        ),
        (
            "a security group Amazon EC2 has none of",
            create_body(
                &fixtures.assumable_role_arn,
                &fixtures.bucket_arn,
                "sg-never-created",
            ),
            Some("SecurityGroupIds"),
        ),
    ];

    for (index, (case, body, refused_member)) in rows.iter().enumerate() {
        let name = env_name("CreRef", index);
        let answer = harness.create(&name, body).await;
        match refused_member {
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the create to be served, got {} {}",
                    answer.status, answer.body
                );
                let stored = harness.environment(case, &name).await;
                assert_eq!(
                    references_of(&stored),
                    merged_references(&stored, &json!({})),
                    "case {case:?}: the stored record must report the references it was \
                     created with, got {stored}"
                );
            }
            Some(member) => {
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: expected HTTP 400, got {} {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code(),
                    "ValidationException",
                    "case {case:?}: expected a ValidationException, got {}",
                    answer.body
                );
                let message = answer.message();
                assert!(
                    message.contains(member),
                    "case {case:?}: expected the refusal to name {member:?}, got {message:?}"
                );
                let read = harness.get(&name).await;
                assert_eq!(
                    read.status, 404,
                    "case {case:?}: a refused create must store nothing, GetEnvironment \
                     answered {} {}",
                    read.status, read.body
                );
            }
        }
    }
}

/// Every `AirflowVersion` the member's own doc lists as valid, in the ascending order it
/// lists them in ("Valid values: 2.7.2, 2.8.1, 2.9.2, 2.10.1, 2.10.3, 2.11.0, and 3.0.6"),
/// from `smithy_query(query="doc", shape="UpdateEnvironmentInput", member="AirflowVersion")`.
const AIRFLOW_VERSIONS: [&str; 7] = [
    "2.7.2", "2.8.1", "2.9.2", "2.10.1", "2.10.3", "2.11.0", "3.0.6",
];

/// RULE: `AirflowVersion` moves in EITHER direction — "To upgrade your environment, specify a
/// newer version … To downgrade your environment, specify an older version of Apache Airflow
/// supported by Amazon MWAA" — so a patch naming any supported release is served and reads
/// back, whichever side of the stored version it is on.
///
/// The rows span the catalog: the newest release down to the oldest, one step back, one step
/// forward, across the 2.x/3.x boundary in both directions, and a patch restating the version
/// the environment already runs.
#[tokio::test]
async fn an_update_moves_the_apache_airflow_version_in_either_direction() {
    let newest = AIRFLOW_VERSIONS[AIRFLOW_VERSIONS.len() - 1];
    let oldest = AIRFLOW_VERSIONS[0];
    let rows: [(&str, &str, &str); 6] = [
        ("the newest release back to the oldest", newest, oldest),
        ("one step back", AIRFLOW_VERSIONS[3], AIRFLOW_VERSIONS[2]),
        ("one step forward", AIRFLOW_VERSIONS[2], AIRFLOW_VERSIONS[3]),
        ("across the major boundary, back", newest, AIRFLOW_VERSIONS[5]),
        (
            "across the major boundary, forward",
            AIRFLOW_VERSIONS[5],
            newest,
        ),
        ("the version it already runs", AIRFLOW_VERSIONS[4], AIRFLOW_VERSIONS[4]),
    ];

    let harness = Harness::new(Peers::Absent);
    for (index, (case, from, to)) in rows.iter().enumerate() {
        let name = env_name("Version", index);
        let mut body = create_body(
            &format!("arn:aws:iam::{ACCOUNT}:role/{name}-execution"),
            "arn:aws:s3:::mwaa-dags-first",
            "sg-aaa111",
        );
        body.as_object_mut()
            .expect("an object body")
            .insert("AirflowVersion".to_string(), json!(from));
        harness.available_environment(case, &name, &body).await;

        let answer = harness.update(&name, &json!({"AirflowVersion": to})).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected a move from {from} to {to} to be served, got {} {}",
            answer.status, answer.body
        );
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let reported = harness
                .environment(case, &name)
                .await
                .get("AirflowVersion")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if reported == *to {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the record to report {to:?}, got {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
