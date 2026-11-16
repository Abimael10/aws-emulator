// SPDX-License-Identifier: CC0-1.0

//! The IAM AUTHORIZATION of `ScanSbom`, driven end-to-end over the same router the emulator
//! serves with.
//!
//! RULE: a signed call is decided against the CALLING principal's identity-based IAM
//! policies before the service acts on the request, so a principal no readable document
//! grants the action to is answered the modelled `AccessDeniedException` (HTTP 403) and is
//! told nothing the call would have computed. The decision is IAM's own: an explicit `Deny`
//! about the request wins over every `Allow`, an `Allow` about it permits, and a principal
//! with neither falls to IAM's default deny. The action is `inspector-scan:<OperationName>`
//! — this service's own signing name and `arnNamespace` — and the resource is what the call
//! addresses: this operation's modelled HTTP path under that namespace, in the Region the
//! caller signed for.
//!
//! The rows span what the rule generalises over: whether the principal is an IAM user or an
//! assumed-role session; whether it holds no document at all, a grant over the whole service,
//! a grant scoped to the addressed resource, a grant scoped to another resource, a grant over
//! another service's actions, or a grant plus an explicit deny; and which Region the call was
//! signed for, which is what the addressed ARN carries. Beside the table: the account's own
//! published credential, which holds no IAM policies and keeps being served; a refused call
//! that records no scan and returns no report; and a refused call whose document is also
//! malformed, which is still an authorization failure because the decision is taken first.
//!
//! The principals are made with IAM's and STS's OWN operations over the same engine, so the
//! documents this service reads are the ones those services wrote. Every expected sentence is
//! spelled here rather than read off the crate under test.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_inspectorscan_inspectorscan::state::ComAmazonawsInspectorscanInspectorscanState;
use smithy_server_runtime::MultiServiceRouter;

use super::scan_sbom::{bom, component};

/// The Region most calls below are signed for, and the one the addressed ARN therefore
/// carries.
const REGION: &str = "us-east-1";

/// The action namespace this service publishes its operations under, which is also its SigV4
/// signing name.
const ACTION_PREFIX: &str = "inspector-scan";

/// The only operation this service models.
const OPERATION: &str = "ScanSbom";

/// The code and status an authorization refusal carries.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The access-key id standing for the account's own published credential: neither STS nor IAM
/// holds a record for it, so it resolves to the account's ambient identity, which holds no IAM
/// policies — the position AWS account root credentials are in. Every fixture is made with it.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The `RoleSessionName` every `AssumeRole` here asks for — the last segment of the
/// assumed-role ARN a refusal names as the principal.
const SESSION: &str = "probe";

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal must
/// blame it by — what `sts:GetCallerIdentity` reports for that key.
struct Principal {
    access_key_id: String,
    arn: String,
}

/// This service beside the IAM and STS services its callers are made with, all on ONE shared
/// engine.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing here signs: the `Authorization` header carries only the credential scope,
        // which is where a service reads the caller's access key id and Region from.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_inspectorscan_inspectorscan::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { engine, router }
    }

    /// How many scans this service has recorded — read to prove a refused call wrote nothing.
    fn scan_count(&self) -> usize {
        self.engine
            .state::<ComAmazonawsInspectorscanInspectorscanState>()
            .lock()
            .scan_count()
    }

    /// `POST /scan/sbom` with `request` as the body, signed for `region` with `access_key_id`
    /// in the credential scope.
    async fn scan_as(
        &self,
        access_key_id: &str,
        region: &str,
        request: &Value,
    ) -> (u16, Value) {
        let host = format!("{ACTION_PREFIX}.{region}.amazonaws.com");
        let body = serde_json::to_vec(request).expect("the ScanSbom request serializes");
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri(format!("https://{host}/scan/sbom"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/\
                     {ACTION_PREFIX}/aws4_request, SignedHeaders=host, \
                     Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a valid ScanSbom request");
        let response = self.router.dispatch(req).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One awsQuery call to IAM or STS, made with the account's own credential, that must be
    /// served.
    async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.amazonaws.com/"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "setup {action}: the call must succeed, got {status} with {text}"
        );
        text
    }
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a policy document's braces and quotes survive the round
/// trip.
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

/// The text of the first `<name>…</name>` element of an awsQuery response.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// One identity-based policy document carrying one statement.
fn statement(effect: &str, actions: &[&str], resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": actions, "Resource": resource}],
    })
    .to_string()
}

/// Create an IAM user carrying `policies` as its inline policies, mint a long-term access key
/// for it, and answer that key beside the user's ARN.
async fn iam_user(aws: &Emulator, user_name: &str, policies: &[String]) -> Principal {
    let created = aws
        .query(
            "iam",
            "2010-05-08",
            "CreateUser",
            &[("UserName", user_name.to_string())],
        )
        .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateUser answers an Arn, got {created}"));
    for (index, policy) in policies.iter().enumerate() {
        aws.query(
            "iam",
            "2010-05-08",
            "PutUserPolicy",
            &[
                ("UserName", user_name.to_string()),
                ("PolicyName", format!("inline-{index}")),
                ("PolicyDocument", policy.clone()),
            ],
        )
        .await;
    }
    let key = aws
        .query(
            "iam",
            "2010-05-08",
            "CreateAccessKey",
            &[("UserName", user_name.to_string())],
        )
        .await;
    Principal {
        access_key_id: xml_element(&key, "AccessKeyId")
            .unwrap_or_else(|| panic!("CreateAccessKey answers an AccessKeyId, got {key}")),
        arn,
    }
}

/// Create an IAM role carrying `policies` as its inline policies, assume it through STS, and
/// answer the session's access key id beside the session ARN.
async fn assumed_role(aws: &Emulator, role_name: &str, policies: &[String]) -> Principal {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    aws.query(
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    for (index, policy) in policies.iter().enumerate() {
        aws.query(
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", format!("inline-{index}")),
                ("PolicyDocument", policy.clone()),
            ],
        )
        .await;
    }
    let assumed = aws
        .query(
            "sts",
            "2011-06-15",
            "AssumeRole",
            &[
                ("RoleArn", format!("arn:aws:iam::{account}:role/{role_name}")),
                ("RoleSessionName", SESSION.to_string()),
            ],
        )
        .await;
    Principal {
        access_key_id: xml_element(&assumed, "AccessKeyId")
            .unwrap_or_else(|| panic!("AssumeRole answers an AccessKeyId, got {assumed}")),
        arn: xml_element(&assumed, "Arn")
            .unwrap_or_else(|| panic!("AssumeRole answers the session's Arn, got {assumed}")),
    }
}

/// The resource ARN a call signed for `region` addresses, spelled here independently of the
/// crate under test: this operation's modelled HTTP path under this service's namespace.
fn addressed(region: &str) -> String {
    format!(
        "arn:aws:{ACTION_PREFIX}:{region}:{}:/scan/sbom",
        engine::emulator_account_id()
    )
}

/// The sentence IAM refuses a request with, stated here rather than read off the crate under
/// test: who was refused, which action, on which resource, and which of IAM's rules refused.
fn denial(principal: &str, region: &str, explicit_deny: bool) -> String {
    let action = format!("{ACTION_PREFIX}:{OPERATION}");
    let reason = if explicit_deny {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no identity-based policy allows the {action} action")
    };
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {} {reason}",
        addressed(region)
    )
}

/// A minimal valid CycloneDX 1.5 document carrying one known-vulnerable component, so an
/// authorized call has a report to answer and an unauthorized one has one to be denied.
fn request() -> Value {
    json!({
        "sbom": bom(vec![component(
            "ref-log4j",
            "log4j-core",
            "2.12.1",
            "pkg:maven/org.apache.logging.log4j/log4j-core@2.12.1",
        )]),
        "outputFormat": "CYCLONE_DX_1_5",
    })
}

/// Which kind of principal a row's caller is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    /// An IAM user signing with a long-term access key of its own.
    User,
    /// An `sts:AssumeRole` session, whose permissions are the role's.
    AssumedRole,
}

/// One row of the grant table.
struct Row {
    case: &'static str,
    identity: Identity,
    /// The Region the call is signed for, which is what the addressed ARN carries.
    region: &'static str,
    /// The documents the principal carries, built from the ARN the call addresses.
    policies: fn(&str) -> Vec<String>,
    /// Whether the call must be refused, and whether the refusal names an explicit deny.
    refused: bool,
    explicit_deny: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an IAM user carrying no policy at all",
        identity: Identity::User,
        region: REGION,
        policies: |_addressed| Vec::new(),
        refused: true,
        explicit_deny: false,
    },
    Row {
        case: "an IAM user granted every action of this service",
        identity: Identity::User,
        region: REGION,
        policies: |_addressed| vec![statement("Allow", &["inspector-scan:*"], "*")],
        refused: false,
        explicit_deny: false,
    },
    Row {
        case: "an IAM user granted the action on the addressed resource",
        identity: Identity::User,
        region: REGION,
        policies: |addressed| {
            vec![statement(
                "Allow",
                &["inspector-scan:ScanSbom"],
                addressed,
            )]
        },
        refused: false,
        explicit_deny: false,
    },
    Row {
        case: "an IAM user granted the action on another resource",
        identity: Identity::User,
        region: REGION,
        policies: |_addressed| {
            vec![statement(
                "Allow",
                &["inspector-scan:ScanSbom"],
                "arn:aws:inspector-scan:us-east-1:000000000000:/scan/other",
            )]
        },
        refused: true,
        explicit_deny: false,
    },
    Row {
        case: "an IAM user granted another service's actions",
        identity: Identity::User,
        region: REGION,
        policies: |_addressed| vec![statement("Allow", &["inspector2:*"], "*")],
        refused: true,
        explicit_deny: false,
    },
    Row {
        case: "an IAM user granted everything with this action explicitly denied",
        identity: Identity::User,
        region: REGION,
        policies: |_addressed| {
            vec![
                statement("Allow", &["*"], "*"),
                statement("Deny", &["inspector-scan:ScanSbom"], "*"),
            ]
        },
        refused: true,
        explicit_deny: true,
    },
    Row {
        // The addressed ARN carries the Region the call was signed for, so a grant
        // scoped to one Region's ARN does not reach a call signed for another.
        case: "an IAM user granted the action in one Region, calling in another",
        identity: Identity::User,
        region: "eu-west-1",
        policies: |_addressed| {
            vec![statement(
                "Allow",
                &["inspector-scan:ScanSbom"],
                "arn:aws:inspector-scan:us-east-1:000000000000:/scan/sbom",
            )]
        },
        refused: true,
        explicit_deny: false,
    },
    Row {
        case: "an IAM user granted the action in the Region it calls in",
        identity: Identity::User,
        region: "eu-west-1",
        policies: |addressed| {
            vec![statement(
                "Allow",
                &["inspector-scan:ScanSbom"],
                addressed,
            )]
        },
        refused: false,
        explicit_deny: false,
    },
    Row {
        case: "an assumed-role session whose role carries no policy at all",
        identity: Identity::AssumedRole,
        region: REGION,
        policies: |_addressed| Vec::new(),
        refused: true,
        explicit_deny: false,
    },
    Row {
        case: "an assumed-role session whose role grants the action",
        identity: Identity::AssumedRole,
        region: REGION,
        policies: |addressed| {
            vec![statement(
                "Allow",
                &["inspector-scan:ScanSbom"],
                addressed,
            )]
        },
        refused: false,
        explicit_deny: false,
    },
    Row {
        case: "an assumed-role session whose role grants everything but denies this action",
        identity: Identity::AssumedRole,
        region: REGION,
        policies: |_addressed| {
            vec![
                statement("Allow", &["inspector-scan:*"], "*"),
                statement("Deny", &["inspector-scan:ScanSbom"], "*"),
            ]
        },
        refused: true,
        explicit_deny: true,
    },
];

#[tokio::test]
async fn a_scan_is_decided_against_the_callers_identity_based_policies() {
    for (index, row) in ROWS.iter().enumerate() {
        let aws = Emulator::new();
        let policies = (row.policies)(&addressed(row.region));
        let name = format!("probe-{index}");
        let caller = match row.identity {
            Identity::User => iam_user(&aws, &name, &policies).await,
            Identity::AssumedRole => assumed_role(&aws, &name, &policies).await,
        };

        let before = aws.scan_count();
        let (status, response) = aws.scan_as(&caller.access_key_id, row.region, &request()).await;

        if row.refused {
            assert_eq!(
                status, ACCESS_DENIED_STATUS,
                "case {:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} with {response}",
                row.case
            );
            assert_eq!(
                response["__type"]
                    .as_str()
                    .map(|named| named.split('#').next_back().unwrap_or(named)),
                Some(ACCESS_DENIED),
                "case {:?}: expected the {ACCESS_DENIED} error code, got {response}",
                row.case
            );
            assert_eq!(
                response["message"].as_str(),
                Some(denial(&caller.arn, row.region, row.explicit_deny).as_str()),
                "case {:?}: expected IAM's own sentence, got {response}",
                row.case
            );
            assert!(
                response["sbom"].is_null(),
                "case {:?}: a refusal carries no report, got {response}",
                row.case
            );
            assert_eq!(
                aws.scan_count(),
                before,
                "case {:?}: a refused call records no scan",
                row.case
            );
        } else {
            assert_eq!(
                status, 200,
                "case {:?}: expected HTTP 200, got {status} with {response}",
                row.case
            );
            assert_eq!(
                response["sbom"]["vulnerabilities"][0]["id"].as_str(),
                Some("CVE-2021-44228"),
                "case {:?}: an authorized scan answers the report, got {response}",
                row.case
            );
            assert_eq!(
                aws.scan_count(),
                before + 1,
                "case {:?}: a served call records its scan",
                row.case
            );
        }
    }
}

/// The account's own published credential is not a principal the IAM store holds policies
/// for — the position AWS account root credentials are in — so it keeps being served.
#[tokio::test]
async fn the_accounts_own_credential_is_served() {
    let aws = Emulator::new();
    let (status, response) = aws.scan_as(ACCOUNT_KEY, REGION, &request()).await;
    assert_eq!(
        status, 200,
        "the account's own credential holds no IAM policy and is served, got {status} with \
         {response}"
    );
    assert_eq!(
        response["sbom"]["vulnerabilities"][0]["id"].as_str(),
        Some("CVE-2021-44228"),
        "the served scan answers the report, got {response}"
    );
}

/// The decision is taken BEFORE the submitted document is read, so an unauthorized caller is
/// answered the authorization failure whatever its input looks like — never a validation
/// refusal that would tell it the document was examined.
#[tokio::test]
async fn authorization_is_decided_before_the_document_is_read() {
    let aws = Emulator::new();
    let caller = iam_user(&aws, "ungranted", &[]).await;
    // Each of these bodies is refused with a `ValidationException` for an authorized caller
    // (`scan_sbom_refusals.rs` pins those sentences); for this one the answer is 403.
    for (case, sbom) in [
        ("a document that is not an object", json!("not-a-sbom")),
        ("a document carrying no bomFormat", json!({})),
        (
            "a document whose bomFormat is not CycloneDX",
            json!({"bomFormat": "SPDX", "specVersion": "1.5", "components": []}),
        ),
    ] {
        let (status, response) = aws
            .scan_as(&caller.access_key_id, REGION, &json!({ "sbom": sbom }))
            .await;
        assert_eq!(
            status, ACCESS_DENIED_STATUS,
            "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} with {response}"
        );
        assert_eq!(
            response["message"].as_str(),
            Some(denial(&caller.arn, REGION, false).as_str()),
            "case {case:?}: expected IAM's own sentence, got {response}"
        );
    }
}
