// SPDX-License-Identifier: CC0-1.0

//! Whether a `StartDataTransformationJob` may hand this service the
//! `DataAccessRoleArn` it names, end to end over the awsJson1_0 wire with IAM
//! mounted beside this service on ONE engine.
//!
//! RULE: a role the account's IAM HOLDS may be passed to this service only where
//! the trust policy that stored role carries lets this service's own principal
//! assume it (`sts:AssumeRole` for `healthlake.amazonaws.com`); a submission naming
//! one it does not is refused with the modelled `AccessDeniedException`, in the
//! `iam:PassRole` wording naming the principal the request's credential speaks for
//! and the role ARN it asked to pass, and mints NO job. A role the account's IAM
//! holds nothing under, and a value naming a role of another account, show nothing
//! about the role, so such a submission is served: a refusal has to be grounded in
//! what IAM answered.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the trust document — the principal it names (this service, another service,
//!   both), the action it grants (the assume, a wildcard covering it, another
//!   action), the effect (an allow, an explicit deny beside an allow), the form
//!   each element takes (one string or a list), and a document that is no policy at
//!   all;
//! * where the role stands — held by this account with that document, held with no
//!   document, held under no record at all, and belonging to another account;
//! * who asked — the account's own published credential and an IAM user of the
//!   account, so the sentence names the principal that signed rather than a fixed
//!   identity.
//!
//! Every role is created through IAM's OWN `CreateRole` over the awsQuery wire, so
//! the trust policy the start resolves is the record IAM really holds, and every
//! call goes through [`MultiServiceRouter::dispatch`](smithy_server_runtime::MultiServiceRouter)
//! — the entry point `core/router` serves.
//!
//! Every modelled CLIENT error is asserted at the status
//! [`wire_http_status`] gives it, for the reason `tests/all/error_envelopes.rs`
//! pins: awsJson1_0 binds nothing to the transport, so this service reports an
//! error's FAULT.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_healthlake_healthlake::scaffolded::wire_http_status;
use service_com_amazonaws_healthlake_healthlake::state::{
    pass_role_denied_message, ComAmazonawsHealthlakeHealthlakeState,
    DATA_ACCESS_SERVICE_PRINCIPAL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::fhir_datastore_lifecycle::{error_code, error_message, string_at};

/// Another service's principal, for the rows whose trust policy names the wrong
/// one. It is the principal of a service that assumes roles for compute instances,
/// so a role carrying it is a plausible mis-configuration rather than a nonsense
/// value.
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The IAM action a trust policy grants for a principal to be able to assume the
/// role, as `service_common::iam_trust_policy::ASSUME_ROLE_ACTION` states it.
const ASSUME_ROLE_ACTION: &str = "sts:AssumeRole";

/// This service and IAM over ONE engine, so a role a start names is the record
/// IAM's own handlers hold.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(
            service_com_amazonaws_healthlake_healthlake::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// One awsJson1_0 call to this service, as the principal `access_key_id` names
    /// (`None` for a request carrying no credential at all, which speaks for the
    /// account's own published one).
    async fn call(
        &self,
        operation: &str,
        body: &Value,
        access_key_id: Option<&str>,
    ) -> (u16, Value) {
        let mut request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("HealthLake.{operation}"));
        if let Some(access_key_id) = access_key_id {
            request = request.header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/us-east-1/healthlake/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            );
        }
        let request = request
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, payload)
    }

    /// One call to this service that must succeed, returning its body.
    async fn ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, payload) = self.call(operation, body, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer 200, got {status}: {payload}"
        );
        payload
    }

    /// One awsQuery call to IAM, answering the XML body it served.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(bytes::Bytes::from(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: IAM {action} must answer 200, got {status} with {text}"
        );
        text
    }

    /// A role of this account carrying `trust` as its trust policy, under the IAM
    /// path `path` (`None` for the account root), as the ARN IAM persisted.
    async fn role(
        &self,
        case: &str,
        name: &str,
        trust: &Value,
        path: Option<&str>,
    ) -> String {
        let mut params = vec![
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust.to_string()),
        ];
        if let Some(path) = path {
            params.push(("Path", path.to_string()));
        }
        let created = self.iam(case, "CreateRole", &params).await;
        xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateRole answers the role Arn, got {created}"))
    }

    /// An IAM user of this account holding its own access key, answered as
    /// `(access key id, user ARN)`.
    async fn user_with_key(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .iam(case, "CreateUser", &[("UserName", name.to_string())])
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateUser answers the user Arn, got {created}"));
        let key = self
            .iam(case, "CreateAccessKey", &[("UserName", name.to_string())])
            .await;
        let access_key_id = xml_element(&key, "AccessKeyId").unwrap_or_else(|| {
            panic!("case {case:?}: CreateAccessKey answers the AccessKeyId, got {key}")
        });
        (access_key_id, arn)
    }

    /// One data transformation profile, answered as its `ProfileId`.
    async fn profile(&self, case: &str, name: &str) -> String {
        let created = self
            .ok(
                case,
                "CreateDataTransformationProfile",
                &json!({
                    "ProfileName": name,
                    "SourceFormat": "CCDA",
                    "Source": { "ProfileMapping": { "ProfileMapping": {
                        "sections/allergies.map": "AllergyIntolerance <- allergy",
                    } } },
                }),
            )
            .await;
        string_at(&created, "ProfileId")
    }

    /// How many data transformation job records the store holds.
    fn held_jobs(&self) -> usize {
        let store = self
            .engine
            .state::<ComAmazonawsHealthlakeHealthlakeState>();
        let held = store.lock().transformation_jobs.len();
        held
    }

    /// The jobs `ListDataTransformationJobs` reports under the name `job_name`.
    async fn jobs_named(&self, case: &str, job_name: &str) -> Vec<Value> {
        let listed = self
            .ok(
                case,
                "ListDataTransformationJobs",
                &json!({ "JobName": job_name }),
            )
            .await;
        listed["Items"].as_array().cloned().unwrap_or_default()
    }
}

/// `value` as an `application/x-www-form-urlencoded` field: everything but the
/// unreserved characters percent-encoded, which is how an awsQuery client writes a
/// policy document into a form body.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
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

/// A trust policy with one statement: `effect` for the principals `principal`
/// names, over the actions `action` names. Each element is handed in as the JSON the
/// row writes it, so a row can send one string or a list.
fn trust(effect: &str, principal: Value, action: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principal },
            "Action": action,
        }]
    })
}

/// A `StartDataTransformationJob` body naming `role_arn` as the role the job would
/// run as, with every other member a served submission carries.
fn start_request(profile_id: &str, role_arn: &str, client_token: &str, job_name: &str) -> Value {
    json!({
        "ProfileId": profile_id,
        "ClientToken": client_token,
        "JobName": job_name,
        "DataAccessRoleArn": role_arn,
        "InputDataConfig": { "S3Uri": "s3://transform-in-bucket" },
        "OutputDataConfig": { "S3Configuration": {
            "S3Uri": "s3://transform-out-bucket",
            "KmsKeyId": "1234abcd-12ab-34cd-56ef-1234567890ab",
        } },
    })
}

/// One row of the trust table: the trust document IAM holds for the role, the path
/// the role sits under, and whether the submission is served.
struct Row {
    case: &'static str,
    /// The trust policy the role is created with.
    trust: Value,
    /// The IAM path the role is created under, `None` for the account root.
    path: Option<&'static str>,
    /// Whether a start naming that role mints a job.
    served: bool,
}

/// The rows, each naming the dimension it varies.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a trust policy admitting this service, both elements as strings",
            trust: trust(
                "Allow",
                json!(DATA_ACCESS_SERVICE_PRINCIPAL),
                json!(ASSUME_ROLE_ACTION),
            ),
            path: None,
            served: true,
        },
        Row {
            case: "a trust policy admitting this service beside another, as lists",
            trust: trust(
                "Allow",
                json!([OTHER_PRINCIPAL, DATA_ACCESS_SERVICE_PRINCIPAL]),
                json!(["sts:TagSession", ASSUME_ROLE_ACTION]),
            ),
            path: None,
            served: true,
        },
        Row {
            case: "a trust policy admitting this service through a service wildcard",
            trust: trust(
                "Allow",
                json!(DATA_ACCESS_SERVICE_PRINCIPAL),
                json!("sts:*"),
            ),
            path: None,
            served: true,
        },
        Row {
            case: "a trust policy naming this service in another casing",
            trust: trust(
                "Allow",
                json!(DATA_ACCESS_SERVICE_PRINCIPAL.to_ascii_uppercase()),
                json!("STS:ASSUMEROLE"),
            ),
            path: Some("/service-role/"),
            served: true,
        },
        Row {
            case: "a trust policy whose SECOND statement admits this service",
            trust: json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "Service": OTHER_PRINCIPAL },
                        "Action": ASSUME_ROLE_ACTION,
                    },
                    {
                        "Effect": "Allow",
                        "Principal": { "Service": DATA_ACCESS_SERVICE_PRINCIPAL },
                        "Action": ASSUME_ROLE_ACTION,
                    },
                ]
            }),
            path: None,
            served: true,
        },
        Row {
            case: "a trust policy admitting another service only",
            trust: trust(
                "Allow",
                json!(OTHER_PRINCIPAL),
                json!(ASSUME_ROLE_ACTION),
            ),
            path: None,
            served: false,
        },
        Row {
            case: "a trust policy granting this service another action only",
            trust: trust(
                "Allow",
                json!(DATA_ACCESS_SERVICE_PRINCIPAL),
                json!("s3:GetObject"),
            ),
            path: Some("/service-role/"),
            served: false,
        },
        Row {
            case: "a trust policy denying this service beside an allow",
            trust: json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "Service": DATA_ACCESS_SERVICE_PRINCIPAL },
                        "Action": ASSUME_ROLE_ACTION,
                    },
                    {
                        "Effect": "Deny",
                        "Principal": { "Service": DATA_ACCESS_SERVICE_PRINCIPAL },
                        "Action": ASSUME_ROLE_ACTION,
                    },
                ]
            }),
            path: None,
            served: false,
        },
    ]
}

/// RULE: a role this account holds is passable to this service exactly where its
/// stored trust policy admits this service's principal with the assume, and a start
/// naming one it does not mints no job.
#[tokio::test]
async fn only_a_role_whose_trust_policy_admits_this_service_can_be_passed() {
    let emulator = Emulator::new();
    let profile_id = emulator.profile("the profile every row starts against", "role-trust").await;
    let caller = ::service_common::caller_principal::ambient_user_arn();
    let mut minted = 0;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let role_arn = emulator
            .role(
                case,
                &format!("transform-role-{index}"),
                &row.trust,
                row.path,
            )
            .await;
        let job_name = format!("job-{index}");
        let request = start_request(
            &profile_id,
            &role_arn,
            &format!("token-{index}"),
            &job_name,
        );
        let (status, payload) = emulator.call("StartDataTransformationJob", &request, None).await;

        if row.served {
            minted += 1;
            assert_eq!(
                status, 200,
                "case {case:?}: the submission must be served, got {status}: {payload}"
            );
            assert!(
                payload["JobId"].as_str().is_some_and(|id| !id.is_empty()),
                "case {case:?}: a served submission answers the minted JobId, got {payload}"
            );
            assert_eq!(
                emulator.jobs_named(case, &job_name).await.len(),
                1,
                "case {case:?}: the job it minted must be listed under its name"
            );
        } else {
            assert_eq!(
                (status, error_code(&payload).as_str()),
                (wire_http_status(403), "AccessDeniedException"),
                "case {case:?}: the submission must be refused as access denied, got \
                 {status}: {payload}"
            );
            let expected = pass_role_denied_message(&caller, &role_arn);
            assert_eq!(
                error_message(&payload),
                expected,
                "case {case:?}: expected {expected:?}, got {payload}"
            );
            assert!(
                emulator.jobs_named(case, &job_name).await.is_empty(),
                "case {case:?}: a refused submission must leave no job under its name"
            );
        }
        assert_eq!(
            emulator.held_jobs(),
            minted,
            "case {case:?}: only the served submissions may have minted a job"
        );
    }
}

/// RULE, the half about values IAM says nothing about: a `DataAccessRoleArn` the
/// account's IAM holds no record of, and one naming a role of another account, are
/// served — a refusal has to be grounded in what IAM answered about the role.
#[tokio::test]
async fn a_role_iam_says_nothing_about_is_passed_through() {
    let emulator = Emulator::new();
    let profile_id = emulator.profile("the profile every row starts against", "role-silent").await;
    // One role IAM DOES hold and refuses, so the rows below are about the absence of
    // an answer and not about a gate that admits everything.
    let untrusted = emulator
        .role(
            "the control role",
            "control-untrusted",
            &trust(
                "Allow",
                json!(OTHER_PRINCIPAL),
                json!(ASSUME_ROLE_ACTION),
            ),
            None,
        )
        .await;
    let (status, payload) = emulator
        .call(
            "StartDataTransformationJob",
            &start_request(&profile_id, &untrusted, "token-control", "job-control"),
            None,
        )
        .await;
    assert_eq!(
        (status, error_code(&payload).as_str()),
        (wire_http_status(403), "AccessDeniedException"),
        "case the control role: a role IAM holds and this service cannot assume is refused, \
         got {status}: {payload}"
    );

    // The account this emulator serves, read off an ARN it mints rather than written
    // out here, and another account's id of the same shape.
    let own_account = engine::emulator_account_id().to_string();
    let rows: [(&str, String); 4] = [
        (
            "a role of this account IAM holds no record of",
            format!("arn:aws:iam::{own_account}:role/never-created"),
        ),
        (
            "another role of this account IAM holds no record of, under a path",
            format!("arn:aws:iam::{own_account}:role/service-role/never-created-either"),
        ),
        (
            "a role of another account",
            "arn:aws:iam::111122223333:role/HealthLakeTransform".to_string(),
        ),
        (
            "a role of another account in another partition",
            "arn:aws-us-gov:iam::111122223333:role/HealthLakeTransform".to_string(),
        ),
    ];
    for (index, (case, role_arn)) in rows.into_iter().enumerate() {
        let job_name = format!("job-silent-{index}");
        let (status, payload) = emulator
            .call(
                "StartDataTransformationJob",
                &start_request(
                    &profile_id,
                    &role_arn,
                    &format!("token-silent-{index}"),
                    &job_name,
                ),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a role IAM says nothing about must be served, got {status}: {payload}"
        );
        assert_eq!(
            emulator.jobs_named(case, &job_name).await.len(),
            1,
            "case {case:?}: the served submission must be listed under its name"
        );
    }
}

/// RULE, the half about WHO asked: the refusal names the principal the request's
/// credential speaks for — the identity the token service reports for that same
/// credential — so two callers refused about one role are blamed by their own ARNs.
#[tokio::test]
async fn the_refusal_names_the_principal_the_credential_speaks_for() {
    let emulator = Emulator::new();
    let profile_id = emulator.profile("the profile both callers start against", "role-caller").await;
    let role_arn = emulator
        .role(
            "the untrusted role both callers pass",
            "caller-untrusted",
            &trust(
                "Allow",
                json!(OTHER_PRINCIPAL),
                json!(ASSUME_ROLE_ACTION),
            ),
            None,
        )
        .await;
    let (access_key_id, user_arn) = emulator.user_with_key("an IAM user of the account", "authoring-user").await;

    // The callers the rows span: the account's own published credential (a request
    // carrying none resolves to it) and an IAM user holding its own access key.
    let callers: [(&str, Option<&str>, String); 3] = [
        (
            "the account's own published credential",
            None,
            ::service_common::caller_principal::ambient_user_arn(),
        ),
        (
            "an IAM user of the account, by its own access key",
            Some(access_key_id.as_str()),
            user_arn.clone(),
        ),
        (
            "an access key no record of this account names",
            Some("AKIANOTHINGHOLDSTHIS"),
            ::service_common::caller_principal::ambient_user_arn(),
        ),
    ];
    for (index, (case, access_key_id, expected_caller)) in callers.into_iter().enumerate() {
        let (status, payload) = emulator
            .call(
                "StartDataTransformationJob",
                &start_request(
                    &profile_id,
                    &role_arn,
                    &format!("token-caller-{index}"),
                    &format!("job-caller-{index}"),
                ),
                access_key_id,
            )
            .await;
        assert_eq!(
            (status, error_code(&payload).as_str()),
            (wire_http_status(403), "AccessDeniedException"),
            "case {case:?}: the submission must be refused as access denied, got {status}: {payload}"
        );
        let expected = pass_role_denied_message(&expected_caller, &role_arn);
        assert_eq!(
            error_message(&payload),
            expected,
            "case {case:?}: expected {expected:?}, got {payload}"
        );
    }
    assert_eq!(
        emulator.held_jobs(),
        0,
        "no refused submission may mint a job"
    );
}
