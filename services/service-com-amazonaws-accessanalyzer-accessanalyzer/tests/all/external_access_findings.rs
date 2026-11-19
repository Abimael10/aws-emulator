// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of the external-access findings surface — `ListFindings` and
//! `GetFinding` — driven over the wire.
//!
//! Every case here goes through the real router: an `http::Request` is dispatched to the
//! mounted services, so the restJson1 routing, the boundary constraint validation, the
//! handler and the response serialization all run exactly as they do in the emulator
//! binary. Nothing reaches into a `state` module directly — the resources a finding is
//! about are created through their OWN service's API (IAM's `CreateRole` /
//! `UpdateAssumeRolePolicy`, S3's `CreateBucket` / `PutBucketPolicy`) and the findings are
//! only believed as `ListFindings` reports them.
//!
//! RULE under test: an external access analyzer reports, for every resource of its own
//! account whose resource-based policy this emulator holds, one finding per principal
//! OUTSIDE that account that an `Allow` statement of that policy grants access to — marked
//! `isPublic` when the statement admits every principal with nothing in its `Condition`
//! binding the caller — and none for a statement that grants nothing or grants only inside
//! the account. A finding has a LIFECYCLE: it is `ACTIVE` while the policy still grants the
//! access, `RESOLVED` once it does not, and it is addressable by its own id either way, which
//! reports the same finding the listing reports.
//!
//! The table spans the classes that rule generalises over: both resource types whose
//! policy this emulator holds (an IAM role's trust policy, an S3 bucket's bucket policy),
//! every principal form the IAM grammar keys (`AWS` as an account id, as an account root
//! ARN and as a role ARN, `Service`, `Federated`, the `*` wildcard and `NotPrincipal`),
//! `Allow` against `Deny`, an unscoped wildcard against a scoped one, one statement against
//! several, and an internal principal against an external one. The recorded
//! external-trust-is-reported and service-principal-trust-is-not cases are two of its rows.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_accessanalyzer_accessanalyzer::ops::types::{
    FINDING_SOURCE_TYPE_VALUES, FINDING_STATUS_VALUES, ORDER_BY_VALUES, RESOURCE_TYPE_VALUES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{is_timestamp, label};

/// The region every request is signed for; it is the region the analyzer's ARN carries.
const REGION: &str = "us-east-1";

/// An account that is not this emulator's own — the external one a finding reports.
const EXTERNAL_ACCOUNT: &str = "111111111111";

/// A second external account, so a case can report two findings that differ only in which
/// account they name.
const OTHER_ACCOUNT: &str = "222222222222";

/// The IAM action a role trust policy grants for a principal to be able to assume the role.
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The account every analyzer and every resource here belongs to: the emulator's own, which
/// is the zone of trust an account analyzer monitors.
fn own_account() -> String {
    engine::emulator_account_id().to_string()
}

/// One response, reduced to what the assertions read.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.error_code.as_deref().unwrap_or("")
    }

    /// A member of the response body, treating JSON `null` as absent the way an omitted
    /// optional member is.
    fn member(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|value| !value.is_null())
    }

    /// The findings this reply reports, in the order it reports them.
    fn findings(&self) -> Vec<Value> {
        self.member("findings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("expected a findings list in {:?}", self.body))
    }

    /// The `name` of the single `fieldList` entry a field validation refusal carries.
    fn field_name(&self) -> String {
        self.member("fieldList")
            .and_then(Value::as_array)
            .and_then(|fields| fields.first())
            .and_then(|field| field.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn message(&self) -> String {
        self.member("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// An emulator holding the analyzer service beside the services that own the resources it
/// analyzes: the policies are read out of IAM's and S3's own stores, so both have to be
/// mounted for a finding to exist at all.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing here is signed: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_accessanalyzer_accessanalyzer::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One restJson1 call to the analyzer service.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match body {
            Some(value) => serde_json::to_vec(&value).expect("the request body serializes"),
            None => Vec::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://access-analyzer.{REGION}.amazonaws.com{path}"))
            .header("host", format!("access-analyzer.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/access-analyzer/\
                     aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        let error_code = error_code.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// Create an analyzer and answer the ARN it reports, which every findings call is
    /// scoped by.
    async fn analyzer(&self, case: &str, name: &str, r#type: &str) -> String {
        let reply = self
            .send(
                "PUT",
                "/analyzer",
                Some(json!({"analyzerName": name, "type": r#type})),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: CreateAnalyzer must succeed, got {reply:?}"
        );
        reply
            .member("arn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an arn in {:?}", reply.body))
            .to_string()
    }

    /// `POST /finding` with the given request body.
    async fn list_findings(&self, body: Value) -> Reply {
        self.send("POST", "/finding", Some(body)).await
    }

    /// `GET /finding/{id}?analyzerArn=…`: the id is an `@httpLabel` and the analyzer ARN an
    /// `@httpQuery` entry, both encoded the way an SDK encodes one.
    async fn get_finding(&self, analyzer_arn: &str, id: &str) -> Reply {
        self.send(
            "GET",
            &format!(
                "/finding/{}?analyzerArn={}",
                label(id),
                label(analyzer_arn)
            ),
            None,
        )
        .await
    }

    /// One awsQuery call to IAM.
    async fn iam(&self, action: &str, params: &[(&str, String)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// Create an IAM role carrying `trust` as its trust policy, and answer the ARN IAM
    /// minted for it.
    async fn create_role(&self, case: &str, name: &str, trust: &str) -> String {
        let (status, body) = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", name.to_string()),
                    ("AssumeRolePolicyDocument", trust.to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateRole must succeed, got {status} with {body}"
        );
        xml_member(&body, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: expected an Arn in {body}"))
    }

    /// Replace a role's trust policy through IAM's own `UpdateAssumeRolePolicy`.
    async fn update_trust(&self, case: &str, name: &str, trust: &str) {
        let (status, body) = self
            .iam(
                "UpdateAssumeRolePolicy",
                &[
                    ("RoleName", name.to_string()),
                    ("PolicyDocument", trust.to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateAssumeRolePolicy must succeed, got {status} with {body}"
        );
    }

    /// One S3 request, addressed path-style to the Regional endpoint.
    async fn s3(&self, method: &str, uri: &str, body: &[u8]) -> (u16, String) {
        let host = format!("s3.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("a well-formed S3 request");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// Create a bucket and put `policy` on it, and answer the bucket's ARN.
    async fn create_bucket(&self, case: &str, name: &str, policy: &str) -> String {
        let (status, body) = self.s3("PUT", &format!("/{name}"), b"").await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateBucket must succeed, got {status} with {body}"
        );
        let (status, body) = self
            .s3("PUT", &format!("/{name}?policy"), policy.as_bytes())
            .await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: PutBucketPolicy must succeed, got {status} with {body}"
        );
        format!("arn:aws:s3:::{name}")
    }
}

/// A form value encoded the way an SDK encodes one in an awsQuery body: every character
/// outside the unreserved set is percent-escaped.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<name>…</name>` element of an awsQuery XML response.
fn xml_member(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// A one-statement resource policy: `effect` to `principal` for `actions`, with an optional
/// `Resource` and `Condition`.
fn policy(
    effect: &str,
    principal: Value,
    actions: Value,
    resource: Option<&str>,
    condition: Option<Value>,
) -> String {
    let mut statement = json!({"Effect": effect, "Principal": principal, "Action": actions});
    if let Some(resource) = resource {
        statement["Resource"] = json!(resource);
    }
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// A trust policy: `Allow` to `principal` for [`ASSUME_ROLE`].
fn trust(principal: Value) -> String {
    policy("Allow", principal, json!(ASSUME_ROLE), None, None)
}

/// What one expected finding is asserted on: the principal it reports (type and value),
/// whether the access is public, the actions it reports, the resource type and the
/// `sources` types it carries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Expected {
    principal: (String, String),
    is_public: bool,
    actions: Vec<String>,
    resource_type: String,
    sources: Vec<String>,
}

impl Expected {
    fn role(kind: &str, value: impl Into<String>, is_public: bool, actions: &[&str]) -> Self {
        Self {
            principal: (kind.to_string(), value.into()),
            is_public,
            actions: actions.iter().map(|action| action.to_string()).collect(),
            resource_type: "AWS::IAM::Role".to_string(),
            sources: Vec::new(),
        }
    }

    fn bucket(kind: &str, value: impl Into<String>, actions: &[&str]) -> Self {
        Self {
            principal: (kind.to_string(), value.into()),
            is_public: false,
            actions: actions.iter().map(|action| action.to_string()).collect(),
            resource_type: "AWS::S3::Bucket".to_string(),
            // "It is populated for Amazon S3 bucket findings": the bucket's own policy
            // granted the access.
            sources: vec!["POLICY".to_string()],
        }
    }
}

/// One finding off the wire, reduced to the same shape.
fn reduce(finding: &Value) -> Expected {
    let principal: Vec<(String, String)> = finding
        .get("principal")
        .and_then(Value::as_object)
        .map(|map| {
            let mut entries: Vec<(String, String)> = map
                .iter()
                .map(|(kind, value)| (kind.clone(), value.as_str().unwrap_or_default().to_string()))
                .collect();
            entries.sort();
            entries
        })
        .unwrap_or_default();
    assert!(
        principal.len() <= 1,
        "a finding reports one principal, got {finding:?}"
    );
    Expected {
        principal: principal
            .into_iter()
            .next()
            .unwrap_or_else(|| (String::new(), String::new())),
        is_public: finding
            .get("isPublic")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        actions: finding
            .get("action")
            .and_then(Value::as_array)
            .map(|actions| {
                actions
                    .iter()
                    .map(|action| action.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        resource_type: finding
            .get("resourceType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        sources: finding
            .get("sources")
            .and_then(Value::as_array)
            .map(|sources| {
                sources
                    .iter()
                    .map(|source| {
                        source
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// A string member of a finding.
fn text(finding: &Value, name: &str) -> String {
    finding
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// `ApplyArchiveRule`: which of the analyzer's findings a rule archives
// ---------------------------------------------------------------------------

/// The `FindingStatus` of a finding whose access is granted and which no archive rule has
/// archived, from the model's `FindingStatus` enum ([`FINDING_STATUS_VALUES`], asserted
/// against it below).
const STATUS_ACTIVE: &str = "ACTIVE";

/// The `FindingStatus` of a finding an archive rule has archived, from the same enum.
const STATUS_ARCHIVED: &str = "ARCHIVED";

/// The `FindingStatus` of a finding whose access the account's policies no longer grant.
const STATUS_RESOLVED: &str = "RESOLVED";

/// The condition key the fixture's organization-bound trust constrains its wildcard
/// principal with, so a rule can select a finding by the condition it carries.
const ORG_ID_CONDITION_KEY: &str = "aws:PrincipalOrgID";

impl Emulator {
    /// `PUT /analyzer/{analyzerName}/archive-rule`: create an archive rule carrying `filter`
    /// on the analyzer `analyzer_name` names.
    async fn create_archive_rule(
        &self,
        case: &str,
        analyzer_name: &str,
        rule_name: &str,
        filter: Value,
    ) -> Reply {
        let reply = self
            .send(
                "PUT",
                &format!("/analyzer/{analyzer_name}/archive-rule"),
                Some(json!({"ruleName": rule_name, "filter": filter})),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: CreateArchiveRule must succeed, got {reply:?}"
        );
        reply
    }

    /// `PUT /archive-rule`: apply the rule `rule_name` of the analyzer `analyzer_arn` names.
    async fn apply_archive_rule(&self, analyzer_arn: &str, rule_name: &str) -> Reply {
        self.apply(json!({"analyzerArn": analyzer_arn, "ruleName": rule_name}))
            .await
    }

    /// `PUT /archive-rule` with the whole request body, for a case that carries a
    /// `clientToken` or names something that does not exist.
    async fn apply(&self, body: Value) -> Reply {
        self.send("PUT", "/archive-rule", Some(body)).await
    }
}

/// The resources every archive case analyzes, and the analyzer that reports them: one
/// finding each, differing in the resource type, the principal form, `isPublic` and whether
/// the statement carries a `Condition` — the finding members an archive rule's criteria read.
struct ArchiveFixture {
    /// The ARN of the analyzer the rules are applied to.
    analyzer: String,
    /// A role whose trust admits every principal: one public finding.
    public_role: String,
    /// A role trusting one external account.
    peer_role: String,
    /// A role whose wildcard principal is bound by a condition.
    org_role: String,
    /// A bucket whose bucket policy grants a second external account.
    bucket: String,
}

/// The analyzer name every fixture creates; each case builds its own emulator, so the name
/// and the resource names are the same in every one of them.
const ARCHIVE_ANALYZER: &str = "archive-analyzer";

/// The rule name every case applies.
const ARCHIVE_RULE: &str = "archiveRule";

impl ArchiveFixture {
    /// Every resource ARN the analyzer reports a finding for, in the order a listing
    /// sorted by resource reports them.
    fn resources(&self) -> Vec<String> {
        let mut resources = vec![
            self.public_role.clone(),
            self.peer_role.clone(),
            self.org_role.clone(),
            self.bucket.clone(),
        ];
        resources.sort();
        resources
    }
}

/// Create the fixture through each resource's OWN service, and answer what a rule's criteria
/// can name.
async fn archive_fixture(case: &str, emulator: &Emulator) -> ArchiveFixture {
    let analyzer = emulator.analyzer(case, ARCHIVE_ANALYZER, "ACCOUNT").await;
    let public_role = emulator
        .create_role(case, "archive-public", &trust(json!("*")))
        .await;
    let peer_role = emulator
        .create_role(
            case,
            "archive-peer",
            &trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")})),
        )
        .await;
    let org_role = emulator
        .create_role(
            case,
            "archive-org",
            &policy(
                "Allow",
                json!({"AWS": "*"}),
                json!(ASSUME_ROLE),
                None,
                Some(json!({"StringEquals": {ORG_ID_CONDITION_KEY: "o-abc123"}})),
            ),
        )
        .await;
    let bucket = emulator
        .create_bucket(
            case,
            "archive-bucket",
            &policy(
                "Allow",
                json!({"AWS": OTHER_ACCOUNT}),
                json!("s3:GetObject"),
                Some("arn:aws:s3:::archive-bucket/*"),
                None,
            ),
        )
        .await;
    ArchiveFixture {
        analyzer,
        public_role,
        peer_role,
        org_role,
        bucket,
    }
}

/// Every finding the analyzer reports, ordered by the resource it is about so two listings
/// are compared row by row.
async fn findings_by_resource(case: &str, emulator: &Emulator, analyzer: &str) -> Vec<Value> {
    let listed = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListFindings must succeed, got {listed:?}"
    );
    let mut findings = listed.findings();
    findings.sort_by_key(|finding| text(finding, "resource"));
    findings
}

/// One row of the archive table: the criteria a rule carries, and the resources whose
/// findings it must archive.
struct ArchiveCase {
    case: &'static str,
    /// Built from the fixture, so a criterion can name a stored resource's own ARN.
    filter: fn(&ArchiveFixture) -> Value,
    /// The resources whose findings the rule selects; every other finding stays active.
    archived: fn(&ArchiveFixture) -> Vec<String>,
}

/// RULE: applying an archive rule archives exactly the findings the analyzer reports whose
/// members satisfy that rule's stored `filter` — each keeping its id and everything it says
/// about the access, with `status` `ARCHIVED` in place of `ACTIVE` — and leaves every other
/// finding alone.
///
/// The rows span what the rule generalises over: which finding member a criterion reads (the
/// resource, the resource type, `isPublic`, one principal, a condition key, the status), the
/// operator it reads it with (`eq`, `neq`, `contains`, `exists`), how many criteria the
/// filter holds (a conjunction), and how many findings the criteria select — all of them, a
/// subset, exactly one, and none. The recorded case is the first row: one resource named by
/// `eq`.
#[tokio::test]
async fn an_archive_rule_archives_the_findings_its_criteria_select() {
    assert!(
        [STATUS_ACTIVE, STATUS_ARCHIVED, STATUS_RESOLVED]
            .iter()
            .all(|status| FINDING_STATUS_VALUES.contains(status)),
        "the statuses under test must be modelled FindingStatus values, got \
         {FINDING_STATUS_VALUES:?}"
    );

    let cases: Vec<ArchiveCase> = vec![
        ArchiveCase {
            case: "the recorded shape: one resource named by eq",
            filter: |fixture| json!({"resource": {"eq": [fixture.bucket]}}),
            archived: |fixture| vec![fixture.bucket.clone()],
        },
        ArchiveCase {
            case: "a resource type, which several findings share",
            filter: |_| json!({"resourceType": {"eq": ["AWS::IAM::Role"]}}),
            archived: |fixture| {
                vec![
                    fixture.public_role.clone(),
                    fixture.peer_role.clone(),
                    fixture.org_role.clone(),
                ]
            },
        },
        ArchiveCase {
            case: "the public findings only",
            filter: |_| json!({"isPublic": {"eq": ["true"]}}),
            archived: |fixture| vec![fixture.public_role.clone()],
        },
        ArchiveCase {
            case: "one principal, read at its principal type",
            filter: |_| json!({"principal.AWS": {"eq": [EXTERNAL_ACCOUNT]}}),
            archived: |fixture| vec![fixture.peer_role.clone()],
        },
        ArchiveCase {
            case: "a substring of the resource, with contains",
            filter: |_| json!({"resource": {"contains": ["archive-bucket"]}}),
            archived: |fixture| vec![fixture.bucket.clone()],
        },
        ArchiveCase {
            case: "every finding carrying a condition key",
            filter: |_| {
                let key = format!("condition.{ORG_ID_CONDITION_KEY}");
                json!({key: {"exists": true}})
            },
            archived: |fixture| vec![fixture.org_role.clone()],
        },
        ArchiveCase {
            case: "every finding but one resource type, with neq",
            filter: |_| json!({"resourceType": {"neq": ["AWS::S3::Bucket"]}}),
            archived: |fixture| {
                vec![
                    fixture.public_role.clone(),
                    fixture.peer_role.clone(),
                    fixture.org_role.clone(),
                ]
            },
        },
        ArchiveCase {
            case: "two criteria are a conjunction",
            filter: |_| {
                json!({
                    "resourceType": {"eq": ["AWS::IAM::Role"]},
                    "isPublic": {"eq": ["true"]},
                })
            },
            archived: |fixture| vec![fixture.public_role.clone()],
        },
        ArchiveCase {
            case: "the status every reported finding carries, so the rule selects them all",
            filter: |_| json!({"status": {"eq": [STATUS_ACTIVE]}}),
            archived: |fixture| fixture.resources(),
        },
        ArchiveCase {
            case: "criteria no finding of this analyzer satisfies",
            filter: |_| json!({"resourceType": {"eq": ["AWS::SQS::Queue"]}}),
            archived: |_| Vec::new(),
        },
    ];

    for ArchiveCase {
        case,
        filter,
        archived,
    } in cases
    {
        // One emulator per row: an apply is recorded on the analyzer, so every row is
        // applied to findings no earlier row has archived.
        let emulator = Emulator::new();
        let fixture = archive_fixture(case, &emulator).await;

        let before = findings_by_resource(case, &emulator, &fixture.analyzer).await;
        assert_eq!(
            before
                .iter()
                .map(|finding| text(finding, "resource"))
                .collect::<Vec<String>>(),
            fixture.resources(),
            "case {case:?}: the fixture must report one finding per resource, got {before:?}"
        );
        assert!(
            before
                .iter()
                .all(|finding| text(finding, "status") == STATUS_ACTIVE),
            "case {case:?}: every finding starts active, got {before:?}"
        );

        emulator
            .create_archive_rule(case, ARCHIVE_ANALYZER, ARCHIVE_RULE, filter(&fixture))
            .await;
        let applied = emulator
            .apply_archive_rule(&fixture.analyzer, ARCHIVE_RULE)
            .await;
        assert_eq!(
            applied.status, 200,
            "case {case:?}: ApplyArchiveRule must answer 200, got {applied:?}"
        );
        assert_eq!(
            applied.body,
            Value::Null,
            "case {case:?}: the modelled Unit output carries no body, got {applied:?}"
        );

        let expected_archived = archived(&fixture);
        let after = findings_by_resource(case, &emulator, &fixture.analyzer).await;
        assert_eq!(
            after.len(),
            before.len(),
            "case {case:?}: an apply archives findings rather than removing them, got {after:?}"
        );
        for (was, now) in before.iter().zip(after.iter()) {
            let resource = text(now, "resource");
            let expected = if expected_archived.contains(&resource) {
                STATUS_ARCHIVED
            } else {
                STATUS_ACTIVE
            };
            assert_eq!(
                text(now, "status"),
                expected,
                "case {case:?}: expected {expected} for the finding about {resource:?}, got \
                 {now:?}"
            );
            // Everything the finding says about the ACCESS is what the analysis derives, so
            // an archive leaves all of it — and the finding's id above all — alone.
            for member in [
                "id",
                "principal",
                "action",
                "resource",
                "resourceType",
                "isPublic",
                "condition",
                "sources",
                "resourceOwnerAccount",
                "createdAt",
            ] {
                assert_eq!(
                    now.get(member),
                    was.get(member),
                    "case {case:?}: an archive must not change {member:?} of the finding \
                     about {resource:?}, got {now:?} after {was:?}"
                );
            }
            assert!(
                crate::harness::timestamp_at_least(now.get("updatedAt"), now.get("createdAt")),
                "case {case:?}: updatedAt must not precede createdAt for the finding about \
                 {resource:?}, got {now:?}"
            );
            // The same finding, read by its own id: one status, whichever way it is read.
            let id = text(now, "id");
            let read = emulator.get_finding(&fixture.analyzer, &id).await;
            assert_eq!(
                read.member("finding"),
                Some(now),
                "case {case:?}: GetFinding must report the finding the listing reports for \
                 {resource:?}, got {read:?}"
            );
        }

        // And the status a filter reads is the status the analyzer reports, so a listing
        // filtered by `ARCHIVED` reports exactly the findings the rule archived.
        let archived_listing = emulator
            .list_findings(json!({
                "analyzerArn": fixture.analyzer,
                "filter": {"status": {"eq": [STATUS_ARCHIVED]}},
            }))
            .await;
        let mut selected: Vec<String> = archived_listing
            .findings()
            .iter()
            .map(|finding| text(finding, "resource"))
            .collect();
        selected.sort();
        let mut expected = expected_archived.clone();
        expected.sort();
        assert_eq!(
            selected, expected,
            "case {case:?}: a listing filtered by the archived status reports the findings \
             the rule archived, got {archived_listing:?}"
        );
    }
}

/// RULE: applying the same rule again archives nothing further — the operation is
/// `@idempotent`, so a retry (with or without its `clientToken`) answers success and reports
/// the same findings, with the same statuses and the same instants.
#[tokio::test]
async fn applying_a_rule_again_archives_nothing_further() {
    let case = "a replayed apply";
    let emulator = Emulator::new();
    let fixture = archive_fixture(case, &emulator).await;
    emulator
        .create_archive_rule(
            case,
            ARCHIVE_ANALYZER,
            ARCHIVE_RULE,
            json!({"resourceType": {"eq": ["AWS::S3::Bucket"]}}),
        )
        .await;

    let first = emulator
        .apply_archive_rule(&fixture.analyzer, ARCHIVE_RULE)
        .await;
    assert_eq!(
        first.status, 200,
        "case {case:?}: the first apply must succeed, got {first:?}"
    );
    let archived_once = findings_by_resource(case, &emulator, &fixture.analyzer).await;
    assert_eq!(
        archived_once
            .iter()
            .filter(|finding| text(finding, "status") == STATUS_ARCHIVED)
            .map(|finding| text(finding, "resource"))
            .collect::<Vec<String>>(),
        vec![fixture.bucket.clone()],
        "case {case:?}: the rule selects the bucket's finding, got {archived_once:?}"
    );

    // (what the retry carries) — an `@idempotencyToken` is optional, so both forms of the
    // same retry are replayed.
    let retries = [
        (
            "with a clientToken",
            json!({
                "analyzerArn": fixture.analyzer,
                "ruleName": ARCHIVE_RULE,
                "clientToken": "apply-token-1",
            }),
        ),
        (
            "without a clientToken",
            json!({"analyzerArn": fixture.analyzer, "ruleName": ARCHIVE_RULE}),
        ),
    ];
    for attempt in 1..=3 {
        for (retry, body) in &retries {
            let replayed = emulator.apply(body.clone()).await;
            assert_eq!(
                replayed.status, 200,
                "case {retry:?} attempt {attempt}: a replayed apply must succeed, got \
                 {replayed:?}"
            );
            let again = findings_by_resource(retry, &emulator, &fixture.analyzer).await;
            assert_eq!(
                again, archived_once,
                "case {retry:?} attempt {attempt}: a replayed apply must report the same \
                 findings, got {again:?} after {archived_once:?}"
            );
        }
    }
}

/// RULE: an apply names an existing rule of an existing analyzer; either naming nothing is
/// the modelled 404, and a refused apply archives nothing.
///
/// Rows differ along what does not exist: the analyzer, the rule, and a rule that exists on
/// ANOTHER analyzer — a rule is scoped to the analyzer it was created under.
#[tokio::test]
async fn an_apply_that_names_no_rule_of_that_analyzer_is_refused_and_archives_nothing() {
    let own = own_account();
    let case = "refused applies";
    let emulator = Emulator::new();
    let fixture = archive_fixture(case, &emulator).await;
    let other = emulator
        .analyzer(case, "archive-other-analyzer", "ACCOUNT")
        .await;
    // A rule that would select every finding, created on the OTHER analyzer: applying it
    // with this analyzer's ARN names no rule of this analyzer.
    emulator
        .create_archive_rule(
            case,
            "archive-other-analyzer",
            "otherRule",
            json!({"status": {"eq": [STATUS_ACTIVE]}}),
        )
        .await;
    let missing = format!("arn:aws:access-analyzer:{REGION}:{own}:analyzer/never-created");

    for (row, analyzer, rule, message) in [
        (
            "an analyzer this account never created",
            missing.as_str(),
            ARCHIVE_RULE,
            "Analyzer not found",
        ),
        (
            "a rule the analyzer does not hold",
            fixture.analyzer.as_str(),
            "neverCreated",
            "Archive rule not found",
        ),
        (
            "a rule of another analyzer",
            fixture.analyzer.as_str(),
            "otherRule",
            "Archive rule not found",
        ),
    ] {
        let reply = emulator.apply_archive_rule(analyzer, rule).await;
        assert_eq!(
            (reply.status, reply.code(), reply.message().as_str()),
            (404, "ResourceNotFoundException", message),
            "case {row:?}: expected HTTP 404 ResourceNotFoundException {message:?}, got \
             {reply:?}"
        );

        // Nothing was archived by the refusal, on either analyzer.
        for (whose, analyzer_arn) in [
            ("the named analyzer", fixture.analyzer.as_str()),
            ("the other analyzer", other.as_str()),
        ] {
            let findings = findings_by_resource(row, &emulator, analyzer_arn).await;
            assert!(
                findings
                    .iter()
                    .all(|finding| text(finding, "status") == STATUS_ACTIVE),
                "case {row:?}: a refused apply must archive no finding of {whose}, got \
                 {findings:?}"
            );
        }
    }
}

/// RULE: an archive is a statement about the FINDING, not about the grant — so a finding the
/// account's policies no longer grant is reported resolved whether or not a rule archived it,
/// and it is archived again once the access it reports is granted again, because the rule
/// that selected it still holds.
#[tokio::test]
async fn an_archived_finding_resolves_when_its_access_is_removed() {
    let own = own_account();
    let case = "an archived finding's lifecycle";
    let emulator = Emulator::new();
    let analyzer = emulator
        .analyzer(case, "archive-lifecycle-analyzer", "ACCOUNT")
        .await;
    let external = trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")}));
    emulator
        .create_role(case, "archive-lifecycle-role", &external)
        .await;

    let reported = findings_by_resource(case, &emulator, &analyzer).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: the trust grants one external account, got {reported:?}"
    );
    let id = text(&reported[0], "id");

    emulator
        .create_archive_rule(
            case,
            "archive-lifecycle-analyzer",
            ARCHIVE_RULE,
            json!({"principal.AWS": {"eq": [EXTERNAL_ACCOUNT]}}),
        )
        .await;
    let applied = emulator.apply_archive_rule(&analyzer, ARCHIVE_RULE).await;
    assert_eq!(
        applied.status, 200,
        "case {case:?}: the apply must succeed, got {applied:?}"
    );

    // (step, what the policy grants after it, the status the finding then carries)
    let steps: [(&str, Option<&str>, &str); 3] = [
        ("archived", None, STATUS_ARCHIVED),
        ("the access removed", Some("own"), STATUS_RESOLVED),
        ("the access granted again", Some("external"), STATUS_ARCHIVED),
    ];
    for (step, grant, expected) in steps {
        if let Some(grant) = grant {
            let document = match grant {
                "own" => trust(json!({"AWS": format!("arn:aws:iam::{own}:root")})),
                _ => external.clone(),
            };
            emulator
                .update_trust(step, "archive-lifecycle-role", &document)
                .await;
        }
        let read = emulator.get_finding(&analyzer, &id).await;
        let finding = read
            .member("finding")
            .unwrap_or_else(|| panic!("case {step:?}: expected a finding, got {read:?}"))
            .clone();
        assert_eq!(
            (
                text(&finding, "id").as_str(),
                text(&finding, "status").as_str()
            ),
            (id.as_str(), expected),
            "case {step:?}: expected the same finding with status {expected}, got {finding:?}"
        );
    }
}

/// RULE: the findings of an external access analyzer are the external access the account's
/// own resource-based policies grant.
///
/// Rows differ along the resource type whose policy carries the grant, the principal form
/// the statement names, the effect, the scoping of a wildcard and the number of statements;
/// the recorded cross-account-trust and service-principal-trust rows are the first two.
#[tokio::test]
async fn findings_are_the_external_access_the_accounts_policies_grant() {
    let own = own_account();
    let external_role = format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:role/peer");

    // (case, the resource the policy is attached to and its document, the findings expected)
    let cases: Vec<(&str, Resource, Vec<Expected>)> = vec![
        (
            "a role trust policy granting another account's root",
            Resource::Role(trust(
                json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")}),
            )),
            // The external entity the grant reaches is the ACCOUNT, which a finding names
            // by its id whichever of the two equivalent spellings the policy used.
            vec![Expected::role("AWS", EXTERNAL_ACCOUNT, false, &[ASSUME_ROLE])],
        ),
        (
            "a role trust policy granting only a service principal",
            Resource::Role(trust(json!({"Service": "lambda.amazonaws.com"}))),
            // A service acting on the account's own behalf is not an entity outside it.
            vec![],
        ),
        (
            "a role trust policy granting another account by id",
            Resource::Role(trust(json!({"AWS": EXTERNAL_ACCOUNT}))),
            vec![Expected::role("AWS", EXTERNAL_ACCOUNT, false, &[ASSUME_ROLE])],
        ),
        (
            "a role trust policy granting a role in another account",
            Resource::Role(trust(json!({"AWS": external_role.clone()}))),
            vec![Expected::role(
                "AWS",
                external_role.clone(),
                false,
                &[ASSUME_ROLE],
            )],
        ),
        (
            "a role trust policy granting a principal of this account",
            Resource::Role(trust(json!({"AWS": format!("arn:aws:iam::{own}:root")}))),
            vec![],
        ),
        (
            "a role trust policy granting a federated identity provider",
            Resource::Role(policy(
                "Allow",
                json!({"Federated": "accounts.google.com"}),
                json!("sts:AssumeRoleWithWebIdentity"),
                None,
                None,
            )),
            vec![Expected::role(
                "Federated",
                "accounts.google.com",
                false,
                &["sts:AssumeRoleWithWebIdentity"],
            )],
        ),
        (
            "a role trust policy granting every principal",
            Resource::Role(trust(json!("*"))),
            vec![Expected::role("AWS", "*", true, &[ASSUME_ROLE])],
        ),
        (
            "a role trust policy granting every principal under a condition",
            Resource::Role(policy(
                "Allow",
                json!("*"),
                json!(ASSUME_ROLE),
                None,
                Some(json!({"StringEquals": {"aws:PrincipalOrgID": "o-example"}})),
            )),
            // Every principal, but a condition binds the caller: reported, not public.
            vec![Expected::role("AWS", "*", false, &[ASSUME_ROLE])],
        ),
        (
            "a role trust policy excluding this account with NotPrincipal",
            Resource::Role(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "NotPrincipal": {"AWS": own.clone()},
                        "Action": ASSUME_ROLE,
                    }],
                })
                .to_string(),
            ),
            vec![Expected::role("AWS", "*", true, &[ASSUME_ROLE])],
        ),
        (
            "a role trust policy denying another account",
            Resource::Role(policy(
                "Deny",
                json!({"AWS": EXTERNAL_ACCOUNT}),
                json!(ASSUME_ROLE),
                None,
                None,
            )),
            vec![],
        ),
        (
            "a role trust policy granting two accounts in two statements",
            Resource::Role(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": {"AWS": EXTERNAL_ACCOUNT},
                            "Action": ASSUME_ROLE,
                        },
                        {
                            "Effect": "Allow",
                            "Principal": {"AWS": OTHER_ACCOUNT},
                            "Action": ["sts:AssumeRole", "sts:TagSession"],
                        },
                    ],
                })
                .to_string(),
            ),
            vec![
                Expected::role("AWS", EXTERNAL_ACCOUNT, false, &[ASSUME_ROLE]),
                Expected::role(
                    "AWS",
                    OTHER_ACCOUNT,
                    false,
                    &["sts:AssumeRole", "sts:TagSession"],
                ),
            ],
        ),
        (
            "a bucket policy granting another account",
            Resource::Bucket(policy(
                "Allow",
                json!({"AWS": EXTERNAL_ACCOUNT}),
                json!("s3:GetObject"),
                Some("arn:aws:s3:::findings-bucket/*"),
                None,
            )),
            vec![Expected::bucket("AWS", EXTERNAL_ACCOUNT, &["s3:GetObject"])],
        ),
        (
            "a bucket policy granting this account",
            Resource::Bucket(policy(
                "Allow",
                json!({"AWS": format!("arn:aws:iam::{own}:root")}),
                json!("s3:GetObject"),
                Some("arn:aws:s3:::findings-bucket/*"),
                None,
            )),
            vec![],
        ),
    ];

    for (case, resource, expected) in cases {
        let emulator = Emulator::new();
        let analyzer = emulator.analyzer(case, "findings-analyzer", "ACCOUNT").await;
        let resource_arn = match &resource {
            Resource::Role(document) => {
                emulator
                    .create_role(case, "findings-role", document)
                    .await
            }
            Resource::Bucket(document) => {
                emulator
                    .create_bucket(case, "findings-bucket", document)
                    .await
            }
        };

        // Unfiltered, and filtered by the resource the way the recorded call filters: both
        // report the same findings, because the account holds one analyzable resource.
        for (scope, body) in [
            ("unfiltered", json!({"analyzerArn": analyzer})),
            (
                "filtered by resource",
                json!({
                    "analyzerArn": analyzer,
                    "filter": {"resource": {"eq": [resource_arn.clone()]}},
                }),
            ),
        ] {
            let reply = emulator.list_findings(body).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?} ({scope}): ListFindings must answer 200, got {reply:?}"
            );
            let findings = reply.findings();
            let mut reported: Vec<Expected> = findings.iter().map(reduce).collect();
            let mut wanted: Vec<Expected> = expected.clone();
            reported.sort();
            wanted.sort();
            assert_eq!(
                reported, wanted,
                "case {case:?} ({scope}): expected {wanted:?}, got {findings:?}"
            );

            for finding in &findings {
                assert_eq!(
                    text(finding, "resource"),
                    resource_arn,
                    "case {case:?} ({scope}): the finding must name the analyzed resource, \
                     got {finding:?}"
                );
                assert_eq!(
                    text(finding, "resourceOwnerAccount"),
                    own,
                    "case {case:?} ({scope}): the finding must name the owning account, \
                     got {finding:?}"
                );
                assert_eq!(
                    text(finding, "status"),
                    "ACTIVE",
                    "case {case:?} ({scope}): a reported finding is ACTIVE, got {finding:?}"
                );
                assert!(
                    FINDING_STATUS_VALUES.contains(&text(finding, "status").as_str()),
                    "case {case:?} ({scope}): the status must be modelled, got {finding:?}"
                );
                assert!(
                    RESOURCE_TYPE_VALUES.contains(&text(finding, "resourceType").as_str()),
                    "case {case:?} ({scope}): the resourceType must be modelled, got {finding:?}"
                );
                assert!(
                    !text(finding, "id").is_empty(),
                    "case {case:?} ({scope}): the finding must carry an id, got {finding:?}"
                );
                assert!(
                    finding.get("error").is_none() || finding["error"].is_null(),
                    "case {case:?} ({scope}): an analyzable policy reports no error, \
                     got {finding:?}"
                );
                for instant in ["createdAt", "analyzedAt", "updatedAt"] {
                    assert!(
                        is_timestamp(finding.get(instant)),
                        "case {case:?} ({scope}): {instant} must be an instant of this run, \
                         got {finding:?}"
                    );
                }
                assert!(
                    finding
                        .get("condition")
                        .is_some_and(|condition| condition.is_object()),
                    "case {case:?} ({scope}): condition is a required member, got {finding:?}"
                );
                for source in finding
                    .get("sources")
                    .and_then(Value::as_array)
                    .unwrap_or(&Vec::new())
                {
                    let kind = source.get("type").and_then(Value::as_str).unwrap_or_default();
                    assert!(
                        FINDING_SOURCE_TYPE_VALUES.contains(&kind),
                        "case {case:?} ({scope}): a source type must be modelled, got {source:?}"
                    );
                }
            }
        }
    }
}

/// Which resource a row attaches its policy to.
#[derive(Debug)]
enum Resource {
    /// An IAM role, whose trust policy is the document.
    Role(String),
    /// An S3 general purpose bucket, whose bucket policy is the document.
    Bucket(String),
}

/// The condition keys a statement constrains travel on the finding, so a filter can ask
/// about them: the member is "the condition in the analyzed policy statement that resulted
/// in a finding".
#[tokio::test]
async fn a_findings_condition_reports_the_statements_condition_keys() {
    let emulator = Emulator::new();
    let analyzer = emulator
        .analyzer("condition keys", "condition-analyzer", "ACCOUNT")
        .await;
    emulator
        .create_role(
            "condition keys",
            "condition-role",
            &policy(
                "Allow",
                json!({"AWS": EXTERNAL_ACCOUNT}),
                json!(ASSUME_ROLE),
                None,
                Some(json!({"StringEquals": {"sts:ExternalId": "shared-secret"}})),
            ),
        )
        .await;

    let reply = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    assert_eq!(
        reply.status, 200,
        "case \"condition keys\": ListFindings must answer 200, got {reply:?}"
    );
    let findings = reply.findings();
    assert_eq!(
        findings.len(),
        1,
        "case \"condition keys\": expected one finding, got {findings:?}"
    );
    assert_eq!(
        findings[0].get("condition"),
        Some(&json!({"sts:ExternalId": "shared-secret"})),
        "case \"condition keys\": expected the statement's condition, got {:?}",
        findings[0]
    );
}

/// A read of the findings reports the same findings as the one before it: the analysis is a
/// function of the stored policies, so nothing about reading it moves.
///
/// And when the trust is NARROWED through IAM, the next read reports the finding the analyzer
/// already reported for the access that is gone as `RESOLVED` — a finding has a lifecycle, so
/// an access the policy no longer grants resolves the finding about it rather than erasing
/// it. Widening the trust again grants the access anew, and the finding is `ACTIVE` once
/// more.
#[tokio::test]
async fn findings_follow_the_stored_policy_and_are_stable_across_reads() {
    let own = own_account();
    let emulator = Emulator::new();
    let analyzer = emulator
        .analyzer("stability", "stable-analyzer", "ACCOUNT")
        .await;
    emulator
        .create_role(
            "stability",
            "stable-role",
            &trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")})),
        )
        .await;

    let first = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    let second = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    assert_eq!(
        first.findings(),
        second.findings(),
        "case \"two reads\": a read must report the same findings as the one before it, \
         got {:?} then {:?}",
        first.findings(),
        second.findings()
    );
    assert_eq!(
        first.findings().len(),
        1,
        "case \"two reads\": expected one finding, got {:?}",
        first.findings()
    );
    let reported = first.findings()[0].clone();
    let reported_id = reported
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // Narrow the trust to this account alone: the external access is gone, so the finding
    // that reported it is resolved — same id, same access reported, `RESOLVED`.
    emulator
        .update_trust(
            "narrowed",
            "stable-role",
            &trust(json!({"AWS": format!("arn:aws:iam::{own}:root")})),
        )
        .await;
    let narrowed = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    let resolved = narrowed.findings();
    assert_eq!(
        resolved.len(),
        1,
        "case \"narrowed\": the finding the analyzer reported is resolved, not erased, got \
         {resolved:?}"
    );
    assert_eq!(
        (
            resolved[0].get("id").and_then(Value::as_str),
            resolved[0].get("status").and_then(Value::as_str),
            resolved[0].get("principal"),
            resolved[0].get("action"),
        ),
        (
            Some(reported_id.as_str()),
            Some("RESOLVED"),
            reported.get("principal"),
            reported.get("action"),
        ),
        "case \"narrowed\": a trust that no longer names anyone outside the account resolves \
         the finding it granted, keeping its id and what it reported, got {resolved:?} after \
         {reported:?}"
    );

    // Widen it again: the access is granted anew, so the finding is active again.
    emulator
        .update_trust(
            "widened again",
            "stable-role",
            &trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")})),
        )
        .await;
    let widened = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    let active = widened.findings();
    assert_eq!(
        (
            active.len(),
            active
                .first()
                .and_then(|finding| finding.get("status"))
                .and_then(Value::as_str),
            active
                .first()
                .and_then(|finding| finding.get("id"))
                .and_then(Value::as_str),
        ),
        (1, Some("ACTIVE"), Some(reported_id.as_str())),
        "case \"widened again\": the access is granted once more, so the finding about it is \
         active again, got {active:?}"
    );
}

/// RULE: `filter` returns the findings every criterion holds for, and `sort` orders them by
/// one member of a finding.
///
/// Rows differ along the operator (`eq`, `neq`, `contains`, `exists`), the member it reads
/// (a scalar, a map addressed at one key, a boolean) and whether the criterion holds.
#[tokio::test]
async fn a_filter_selects_findings_and_a_sort_orders_them() {
    let emulator = Emulator::new();
    let analyzer = emulator
        .analyzer("filtering", "filter-analyzer", "ACCOUNT")
        .await;
    // Two roles, so a criterion can tell one finding from another: one grants an external
    // account, the other grants every principal.
    let named = emulator
        .create_role(
            "filtering",
            "filter-role-a",
            &trust(json!({"AWS": EXTERNAL_ACCOUNT})),
        )
        .await;
    let public = emulator
        .create_role("filtering", "filter-role-b", &trust(json!("*")))
        .await;

    for (case, filter, expected) in [
        (
            "eq on resource",
            json!({"resource": {"eq": [named.clone()]}}),
            vec![named.clone()],
        ),
        (
            "neq on resource",
            json!({"resource": {"neq": [named.clone()]}}),
            vec![public.clone()],
        ),
        (
            "contains on resource",
            json!({"resource": {"contains": ["filter-role-b"]}}),
            vec![public.clone()],
        ),
        (
            "eq on principal.AWS",
            json!({"principal.AWS": {"eq": [EXTERNAL_ACCOUNT]}}),
            vec![named.clone()],
        ),
        (
            "eq on isPublic",
            json!({"isPublic": {"eq": ["true"]}}),
            vec![public.clone()],
        ),
        (
            "exists on principal",
            json!({"principal": {"exists": true}}),
            vec![named.clone(), public.clone()],
        ),
        (
            "exists on error is false for an analyzable policy",
            json!({"error": {"exists": true}}),
            vec![],
        ),
        (
            "two criteria are a conjunction",
            json!({
                "resource": {"eq": [named.clone()]},
                "status": {"eq": ["ACTIVE"]},
            }),
            vec![named.clone()],
        ),
        (
            "a criterion nothing satisfies",
            json!({"resourceType": {"eq": ["AWS::SQS::Queue"]}}),
            vec![],
        ),
    ] {
        let reply = emulator
            .list_findings(json!({"analyzerArn": analyzer, "filter": filter}))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: ListFindings must answer 200, got {reply:?}"
        );
        let reported: Vec<String> = reply
            .findings()
            .iter()
            .map(|finding| text(finding, "resource"))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected findings for {expected:?}, got {reported:?}"
        );
    }

    // `sort` orders by one member, in either direction the model's `OrderBy` lists.
    let ascending: Vec<String> = emulator
        .list_findings(json!({
            "analyzerArn": analyzer,
            "sort": {"attributeName": "resource", "orderBy": ORDER_BY_VALUES[0]},
        }))
        .await
        .findings()
        .iter()
        .map(|finding| text(finding, "resource"))
        .collect();
    let descending: Vec<String> = emulator
        .list_findings(json!({
            "analyzerArn": analyzer,
            "sort": {"attributeName": "resource", "orderBy": ORDER_BY_VALUES[1]},
        }))
        .await
        .findings()
        .iter()
        .map(|finding| text(finding, "resource"))
        .collect();
    assert_eq!(
        ascending,
        vec![named.clone(), public.clone()],
        "case \"sort ASC\": expected the findings in resource order, got {ascending:?}"
    );
    let mut reversed = ascending.clone();
    reversed.reverse();
    assert_eq!(
        descending, reversed,
        "case \"sort DESC\": expected the reverse of {ascending:?}, got {descending:?}"
    );
}

/// RULE: `maxResults`/`nextToken` page over the findings, so a caller that follows the
/// tokens to exhaustion sees every finding exactly once and the last page carries no token.
///
/// Rows differ along the page size, from one finding per page to more than the result holds.
#[tokio::test]
async fn pagination_walks_every_finding_exactly_once() {
    let emulator = Emulator::new();
    let analyzer = emulator
        .analyzer("pagination", "paging-analyzer", "ACCOUNT")
        .await;
    for (index, account) in [EXTERNAL_ACCOUNT, OTHER_ACCOUNT, "333333333333"]
        .iter()
        .enumerate()
    {
        emulator
            .create_role(
                "pagination",
                &format!("paging-role-{index}"),
                &trust(json!({"AWS": *account})),
            )
            .await;
    }

    let whole: Vec<String> = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await
        .findings()
        .iter()
        .map(|finding| text(finding, "id"))
        .collect();
    assert_eq!(
        whole.len(),
        3,
        "case \"the whole result\": expected three findings, got {whole:?}"
    );

    for page_size in [1, 2, 3, 10] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut body = json!({"analyzerArn": analyzer, "maxResults": page_size});
            if let Some(token) = &token {
                body["nextToken"] = json!(token);
            }
            let reply = emulator.list_findings(body).await;
            assert_eq!(
                reply.status, 200,
                "case \"maxResults={page_size}\": ListFindings must answer 200, got {reply:?}"
            );
            let findings = reply.findings();
            assert!(
                findings.len() <= page_size as usize,
                "case \"maxResults={page_size}\": a page carries at most {page_size} findings, \
                 got {findings:?}"
            );
            walked.extend(findings.iter().map(|finding| text(finding, "id")));
            token = reply
                .member("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            pages += 1;
            assert!(
                pages <= 8,
                "case \"maxResults={page_size}\": the walk must terminate, got {walked:?}"
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, whole,
            "case \"maxResults={page_size}\": the walk must report every finding exactly once"
        );
    }
}

/// RULE: a request this operation cannot answer is refused with the modelled error for what
/// is wrong with it, never with an empty success.
///
/// Rows differ along what is wrong: the analyzer, its family, the filter, the sort and the
/// paging members.
#[tokio::test]
async fn refusals_name_what_the_caller_has_to_change() {
    let emulator = Emulator::new();
    let external = emulator
        .analyzer("refusals", "refusal-analyzer", "ACCOUNT")
        .await;
    let unused = emulator
        .analyzer("refusals", "refusal-unused", "ACCOUNT_UNUSED_ACCESS")
        .await;
    let missing = format!(
        "arn:aws:access-analyzer:{REGION}:{}:analyzer/never-created",
        own_account()
    );

    for (case, body, status, code, field) in [
        (
            "an analyzer this account never created",
            json!({"analyzerArn": missing}),
            404,
            "ResourceNotFoundException",
            None,
        ),
        (
            "an unused access analyzer",
            json!({"analyzerArn": unused}),
            400,
            "ValidationException",
            None,
        ),
        (
            "a filter key naming no member of a finding",
            json!({"analyzerArn": external, "filter": {"notAMember": {"eq": ["x"]}}}),
            400,
            "ValidationException",
            Some("filter"),
        ),
        (
            "a sort attribute naming no member of a finding",
            json!({"analyzerArn": external, "sort": {"attributeName": "notAMember"}}),
            400,
            "ValidationException",
            Some("sort.attributeName"),
        ),
        (
            "an order the model does not list",
            json!({
                "analyzerArn": external,
                "sort": {"attributeName": "resource", "orderBy": "SIDEWAYS"},
            }),
            400,
            "ValidationException",
            // `orderBy` is a modelled enum, so the boundary refuses a value outside it
            // before the handler runs: that refusal names the member in its message rather
            // than in a `fieldList`.
            None,
        ),
        (
            "a page size below one",
            json!({"analyzerArn": external, "maxResults": 0}),
            400,
            "ValidationException",
            Some("maxResults"),
        ),
        (
            "a token this service never issued",
            json!({"analyzerArn": external, "nextToken": "not-a-token"}),
            400,
            "ValidationException",
            Some("nextToken"),
        ),
    ] {
        let reply = emulator.list_findings(body).await;
        assert_eq!(
            (reply.status, reply.code()),
            (status, code),
            "case {case:?}: expected HTTP {status} {code}, got {reply:?}"
        );
        if let Some(field) = field {
            assert_eq!(
                reply.field_name(),
                field,
                "case {case:?}: the refusal must name {field:?}, got {reply:?}"
            );
        }
    }

    // The family refusal is the service's fixed text for an action the resource does not
    // support: it names neither the operation nor the analyzer.
    let refused = emulator
        .list_findings(json!({"analyzerArn": unused}))
        .await;
    assert_eq!(
        refused.message(),
        "Unsupported operation",
        "case \"an unused access analyzer\": expected the fixed unsupported-action text, \
         got {refused:?}"
    );
}

// ---------------------------------------------------------------------------
// `GetFinding`: ONE finding of the same analysis, addressed by its id
// ---------------------------------------------------------------------------

/// RULE: a finding the analyzer reports is addressable by its own id — a read of it reports
/// the SAME finding the listing reports, member for member, whatever the finding is about.
///
/// The rows differ along what the rule generalises over: the resource type whose policy
/// granted the access (an IAM role's trust policy, an S3 bucket's bucket policy), the
/// principal form the grant names (an account root ARN, a bare account id, a role ARN, the
/// `*` wildcard), the number of actions it allows, and whether the statement carries a
/// `Condition` — one row per finding the analyzer reports, each read back by its id.
#[tokio::test]
async fn every_finding_the_analyzer_reports_is_read_back_by_its_id() {
    let emulator = Emulator::new();
    let analyzer = emulator
        .analyzer("read back", "read-back-analyzer", "ACCOUNT")
        .await;
    // One resource per class the rule spans, so the listing reports several findings that
    // differ in resource type, principal form, action count and condition.
    emulator
        .create_role(
            "read back",
            "read-back-root",
            &trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")})),
        )
        .await;
    emulator
        .create_role("read back", "read-back-account", &trust(json!({"AWS": OTHER_ACCOUNT})))
        .await;
    emulator
        .create_role(
            "read back",
            "read-back-role",
            &trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:role/peer")})),
        )
        .await;
    emulator
        .create_role("read back", "read-back-public", &trust(json!("*")))
        .await;
    emulator
        .create_bucket(
            "read back",
            "read-back-bucket",
            &policy(
                "Allow",
                json!({"AWS": EXTERNAL_ACCOUNT}),
                json!(["s3:GetObject", "s3:PutObject"]),
                Some("arn:aws:s3:::read-back-bucket/*"),
                Some(json!({"StringEquals": {"aws:PrincipalOrgID": "o-abc123"}})),
            ),
        )
        .await;

    let listed = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    let findings = listed.findings();
    assert_eq!(
        findings.len(),
        5,
        "the fixture grants five external accesses, so the listing reports five findings, \
         got {findings:?}"
    );

    for finding in &findings {
        let case = finding
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let id = finding
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an id in {finding:?}"));
        let read = emulator.get_finding(&analyzer, id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetFinding must answer 200 for an id the listing reported, got \
             {read:?}"
        );
        assert_eq!(
            read.member("finding"),
            Some(finding),
            "case {case:?}: expected the finding the listing reports, got {read:?}"
        );
        // And reading it again reports the same finding: a read of a finding changes none of
        // it.
        let again = emulator.get_finding(&analyzer, id).await;
        assert_eq!(
            again.member("finding"),
            Some(finding),
            "case {case:?}: a second read must report the same finding, got {again:?}"
        );
    }
}

/// RULE: a finding has a lifecycle — while the access it reports is granted it is `ACTIVE`,
/// once the policy no longer grants that access the finding the analyzer reported is
/// `RESOLVED` (same id, same access reported), and if the access is granted again the finding
/// is `ACTIVE` once more.
///
/// The rows are the steps of that lifecycle, each driven through the policy's OWN service:
/// granted, removed, granted again.
#[tokio::test]
async fn a_finding_resolves_when_its_access_is_removed_and_activates_when_it_returns() {
    let own = own_account();
    let emulator = Emulator::new();
    let analyzer = emulator
        .analyzer("lifecycle", "lifecycle-analyzer", "ACCOUNT")
        .await;
    let external = trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")}));
    emulator
        .create_role("lifecycle", "lifecycle-role", &external)
        .await;

    let listed = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    let active = listed.findings();
    assert_eq!(
        active.len(),
        1,
        "case \"granted\": the trust grants one external account, got {active:?}"
    );
    let id = active[0]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case \"granted\": expected an id in {:?}", active[0]))
        .to_string();
    let granted = emulator.get_finding(&analyzer, &id).await;
    let granted_finding = granted
        .member("finding")
        .unwrap_or_else(|| panic!("case \"granted\": expected a finding, got {granted:?}"))
        .clone();
    assert_eq!(
        granted_finding.get("status").and_then(Value::as_str),
        Some("ACTIVE"),
        "case \"granted\": an access the policy still grants is an active finding, got \
         {granted_finding:?}"
    );

    // Remove the access through IAM: the finding the analyzer reported is resolved, not gone.
    emulator
        .update_trust(
            "removed",
            "lifecycle-role",
            &trust(json!({"AWS": format!("arn:aws:iam::{own}:root")})),
        )
        .await;
    let resolved = emulator.get_finding(&analyzer, &id).await;
    assert_eq!(
        resolved.status, 200,
        "case \"removed\": GetFinding must still report the finding it reported, got \
         {resolved:?}"
    );
    let resolved_finding = resolved
        .member("finding")
        .unwrap_or_else(|| panic!("case \"removed\": expected a finding, got {resolved:?}"))
        .clone();
    assert_eq!(
        (
            resolved_finding.get("id").and_then(Value::as_str),
            resolved_finding.get("status").and_then(Value::as_str),
            resolved_finding.get("principal"),
            resolved_finding.get("action"),
            resolved_finding.get("resource"),
            resolved_finding.get("createdAt"),
        ),
        (
            Some(id.as_str()),
            Some("RESOLVED"),
            granted_finding.get("principal"),
            granted_finding.get("action"),
            granted_finding.get("resource"),
            granted_finding.get("createdAt"),
        ),
        "case \"removed\": the resolved finding keeps its id and what it reported, got \
         {resolved_finding:?} after {granted_finding:?}"
    );
    assert!(
        crate::harness::timestamp_at_least(
            resolved_finding.get("updatedAt"),
            resolved_finding.get("createdAt"),
        ),
        "case \"removed\": the finding was updated when it was resolved, so updatedAt is at \
         or after createdAt, got {resolved_finding:?}"
    );
    // A resolved finding is a stored record, so reading it again reports it unchanged, and
    // the listing reports the same record.
    let again = emulator.get_finding(&analyzer, &id).await;
    assert_eq!(
        again.member("finding"),
        Some(&resolved_finding),
        "case \"removed\": a second read must report the same resolved finding, got {again:?}"
    );
    let relisted = emulator
        .list_findings(json!({"analyzerArn": analyzer}))
        .await;
    assert_eq!(
        relisted.findings(),
        vec![resolved_finding.clone()],
        "case \"removed\": the listing reports the same resolved finding, got {:?}",
        relisted.findings()
    );

    // Grant it again: the access is back, so the finding about it is active again.
    emulator
        .update_trust("granted again", "lifecycle-role", &external)
        .await;
    let reactivated = emulator.get_finding(&analyzer, &id).await;
    let reactivated_finding = reactivated
        .member("finding")
        .unwrap_or_else(|| panic!("case \"granted again\": expected a finding, got {reactivated:?}"))
        .clone();
    assert_eq!(
        (
            reactivated_finding.get("id").and_then(Value::as_str),
            reactivated_finding.get("status").and_then(Value::as_str),
            reactivated_finding.get("principal"),
        ),
        (
            Some(id.as_str()),
            Some("ACTIVE"),
            granted_finding.get("principal"),
        ),
        "case \"granted again\": the access is granted once more, so the finding is active \
         again, got {reactivated_finding:?}"
    );
}

/// RULE: a request this operation cannot answer is refused with the modelled error for what
/// is wrong with it, never with an empty success.
///
/// Rows differ along what is wrong: the analyzer, the analyzer's family, and what the `id`
/// identifies (nothing at all, and a finding of ANOTHER analyzer — every analyzer derives its
/// own ids, so one analyzer's id is not a finding of the next).
#[tokio::test]
async fn a_finding_read_names_what_the_caller_has_to_change() {
    let own = own_account();
    let emulator = Emulator::new();
    let external = emulator
        .analyzer("refusals", "finding-refusal-analyzer", "ACCOUNT")
        .await;
    let other = emulator
        .analyzer("refusals", "finding-other-analyzer", "ACCOUNT")
        .await;
    let unused = emulator
        .analyzer("refusals", "finding-refusal-unused", "ACCOUNT_UNUSED_ACCESS")
        .await;
    let missing = format!("arn:aws:access-analyzer:{REGION}:{own}:analyzer/never-created");
    emulator
        .create_role(
            "refusals",
            "refusal-role",
            &trust(json!({"AWS": format!("arn:aws:iam::{EXTERNAL_ACCOUNT}:root")})),
        )
        .await;
    // The id the OTHER analyzer reports for the same access: a finding of that analyzer, and
    // of no other.
    let other_listing = emulator.list_findings(json!({"analyzerArn": other})).await;
    let other_id = other_listing.findings()[0]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an id in {:?}", other_listing.findings()))
        .to_string();

    for (case, analyzer, id, status, code, message) in [
        (
            "an analyzer this account never created",
            missing.as_str(),
            other_id.as_str(),
            404,
            "ResourceNotFoundException",
            "Analyzer not found",
        ),
        (
            "an unused access analyzer",
            unused.as_str(),
            other_id.as_str(),
            400,
            "ValidationException",
            "Unsupported operation",
        ),
        (
            "an id that identifies no finding at all",
            external.as_str(),
            "00000000-0000-0000-0000-000000000000",
            404,
            "ResourceNotFoundException",
            "Finding not found",
        ),
        (
            "an id that is not an id this service mints",
            external.as_str(),
            "not-a-finding-id",
            404,
            "ResourceNotFoundException",
            "Finding not found",
        ),
        (
            "a finding of another analyzer",
            external.as_str(),
            other_id.as_str(),
            404,
            "ResourceNotFoundException",
            "Finding not found",
        ),
    ] {
        let reply = emulator.get_finding(analyzer, id).await;
        assert_eq!(
            (reply.status, reply.code(), reply.message().as_str()),
            (status, code, message),
            "case {case:?}: expected HTTP {status} {code} {message:?}, got {reply:?}"
        );
        assert!(
            reply.member("finding").is_none(),
            "case {case:?}: a refusal carries no finding, got {reply:?}"
        );
    }

    // A finding refusal carries the identifier the caller named in the modelled structured
    // members rather than in its message.
    let unknown = emulator
        .get_finding(&external, "00000000-0000-0000-0000-000000000000")
        .await;
    assert_eq!(
        (
            unknown.member("resourceId").and_then(Value::as_str),
            unknown.member("resourceType").and_then(Value::as_str),
        ),
        (
            Some("00000000-0000-0000-0000-000000000000"),
            Some("finding")
        ),
        "case \"an id that identifies no finding at all\": expected the id and the resource \
         kind in the modelled members, got {unknown:?}"
    );
}
