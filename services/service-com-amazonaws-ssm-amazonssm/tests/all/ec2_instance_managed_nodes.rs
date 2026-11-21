// SPDX-License-Identifier: CC0-1.0

//! The harness the in-guest Run Command suite drives, and the premise it rests on: an
//! instance Amazon EC2 launched, with an instance profile whose role lets its agent
//! register, is a managed node this service can send a command to.
//!
//! WHICH instances register is `node_registration`'s rule and
//! `tests/all/ec2_managed_node_registration.rs` is its table; nothing is re-asserted here.
//! What this module owns is the setup every guest-run case needs — the three services on one
//! engine, the role/policy/instance profile, the launch, and the one thing a case reaches a
//! store for (the CLOCK) — plus the one case that the premise holds end to end.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_route53_awsdnsv20130401 as route53;
use service_com_amazonaws_route53resolver_route53resolver as route53resolver;
use service_com_amazonaws_ssm_amazonssm as ssm;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;

/// The Region every call is signed for unless a case says otherwise.
pub(crate) const REGION: &str = "us-west-2";

/// The EC2 API version an `ec2Query` body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The IAM API version an `awsQuery` body carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The AWS STS API version an `awsQuery` body carries, for the suite that signs a call as a
/// principal of its own rather than as the account's published credential.
const STS_API_VERSION: &str = "2011-06-15";

/// The path prefix every Route 53 request carries, which is that service's own API version.
pub(crate) const ROUTE53_PREFIX: &str = "/2013-04-01";

/// The `X-Amz-Target` prefix Route 53 Resolver's `awsJson1_1` wire carries, from that crate's
/// `OPERATIONS.md` wire-protocol section.
const RESOLVER_TARGET_PREFIX: &str = "Route53Resolver";

/// The access key id every call of this harness presents unless a case names another: one
/// neither STS nor IAM holds a record for, which resolves to the account's own identity.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// An instance type every case launches, so the rows differ in what the rule is about
/// rather than in the hardware.
const INSTANCE_TYPE: &str = "t3.micro";

/// The name pattern a client resolving a current Amazon Linux 2023 x86_64 AMI matches on.
pub(crate) const AL2023_NAME_PATTERN: &str = "al2023-ami-2023*x86_64";

/// The trust policy a role an EC2 instance runs as carries — the one `CreateRole` needs and
/// the one AWS documents for an instance profile's role.
const EC2_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The AWS-managed policy AWS documents for the instance profile of a node Systems Manager
/// is to manage.
pub(crate) const SSM_CORE_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// How far back a case moves an instance's launch instant to make its registration due:
/// past the span, with room to spare, so the case does not depend on how fast the host runs.
pub(crate) fn launched_long_enough_ago() -> Duration {
    ssm::state::node_registration::REGISTRATION_DELAY + Duration::from_secs(30)
}

/// What a role carries, which is the axis the registration gate turns on.
pub(crate) enum Grant {
    /// A managed policy attached by ARN — AWS's own or the account's.
    Attached(&'static str),
    /// No policy at all.
    Nothing,
}

/// One emulator: Systems Manager, Amazon EC2 and IAM mounted on ONE shared engine, the way
/// the router mounts every service.
pub(crate) struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signatures are not what these tests are about; the router still reads the SigV4
        // credential scope, which is where each call's Region comes from.
        router.set_request_authentication(false);
        router.register_service(ssm::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));
        // AWS STS, so a case can sign a call as a principal of its own: the identity a
        // request speaks for is resolved from the access key id it signed with, which is the
        // same place `sts:GetCallerIdentity` answers from.
        router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
        // Route 53 and Route 53 Resolver, for the suite about the DNS a node's guest
        // resolves a name through: the zones and the endpoints it is answered from are those
        // services' own records, created over their own front doors.
        router.register_service(route53::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(route53resolver::EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// This service's own store, for the one thing a case reaches it for: the CLOCK.
    pub(crate) fn ssm_state(
        &self,
    ) -> engine::ServiceState<ssm::state::ComAmazonawsSsmAmazonssmState> {
        ssm::state::service_state(&*self.engine).expect("the Systems Manager store")
    }

    /// One `ec2Query` call, which must be served.
    pub(crate) async fn ec2(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let (status, body) = self
            .form(
                &format!("https://ec2.{REGION}.amazonaws.com/"),
                "ec2",
                EC2_API_VERSION,
                action,
                params,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: EC2 {action} must be served, got {status}: {body}"
        );
        body
    }

    /// One `awsQuery` IAM call, which must be served.
    pub(crate) async fn iam(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let (status, body) = self
            .form("https://iam.amazonaws.com/", "iam", IAM_API_VERSION, action, params)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: IAM {action} must be served, got {status}: {body}"
        );
        body
    }

    /// One `awsQuery` AWS STS call, which must be served.
    pub(crate) async fn sts(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let (status, body) = self
            .form(
                &format!("https://sts.{REGION}.amazonaws.com/"),
                "sts",
                STS_API_VERSION,
                action,
                params,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: STS {action} must be served, got {status}: {body}"
        );
        body
    }

    /// One Route 53 call over that service's `restXml` wire, which must answer `expected`.
    pub(crate) async fn route53(
        &self,
        case: &str,
        method: &str,
        path: &str,
        body: &str,
        expected: u16,
    ) -> String {
        let host = "route53.amazonaws.com";
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{ROUTE53_PREFIX}{path}"))
            .header("host", host)
            .header("content-type", "application/xml")
            .header("authorization", authorization("route53", REGION))
            .body(Bytes::from(body.to_owned()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, expected,
            "case {case:?}: Route 53 {method} {path} expected HTTP {expected}, got {status}: \
             {text}"
        );
        text
    }

    /// One Route 53 Resolver call over that service's `awsJson1_1` wire.
    pub(crate) async fn resolver(&self, case: &str, operation: &str, body: &Value) -> Value {
        let host = format!("route53resolver.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", host)
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("{RESOLVER_TARGET_PREFIX}.{operation}"),
            )
            .header("authorization", authorization("route53resolver", REGION))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: Route 53 Resolver {operation} must be served, got {status}: {text}"
        );
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    /// The text of the first `<tag>` element of an XML answer — the id or ARN a setup call
    /// answered with.
    pub(crate) fn answered(body: &str, tag: &str) -> Option<String> {
        elements(body, tag).into_iter().next()
    }

    /// One form-encoded POST, as both of those wires carry.
    async fn form(
        &self,
        uri: &str,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, &str)],
    ) -> (u16, String) {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization(signing_name, REGION))
            .body(Bytes::from(body))
            .expect("a well-formed form request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, String::from_utf8_lossy(response.body()).to_string())
    }

    /// One Systems Manager call signed for `region`.
    pub(crate) async fn ssm(&self, region: &str, operation: &str, body: &Value) -> (u16, Value) {
        self.ssm_as(ACCOUNT_KEY, region, operation, body).await
    }

    /// FETCH one address this emulator handed out, the way the caller it was handed to does:
    /// a plain `GET` of that URL carrying no credentials of its own, because a presigned URL
    /// carries its own in its query string.
    pub(crate) async fn fetch(&self, url: &str) -> (u16, String) {
        let request = http::Request::builder()
            .method("GET")
            .uri(url)
            .body(Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, String::from_utf8_lossy(response.body()).to_string())
    }

    /// The same call, signed with `access_key_id` in the credential scope — which is where a
    /// service reads WHO the caller is.
    pub(crate) async fn ssm_as(
        &self,
        access_key_id: &str,
        region: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("ssm.{region}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                authorization_as(access_key_id, "ssm", region),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let mut parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        // The error shape travels in `__type`; fold in the header the framework also sets so
        // one accessor reads either.
        if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
            map.entry("__type")
                .or_insert_with(|| Value::String(error_type));
        }
        (status, parsed)
    }

    /// Every managed node `region`'s endpoint reports, keyed by node id, walked over as
    /// many pages as it takes.
    pub(crate) async fn nodes(&self, case: &str, region: &str) -> BTreeMap<String, Value> {
        let mut seen = BTreeMap::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({});
            if let Some(next) = &token {
                body["NextToken"] = Value::String(next.clone());
            }
            let (status, answered) = self.ssm(region, "DescribeInstanceInformation", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeInstanceInformation must be served, got {status}: \
                 {answered}"
            );
            for node in answered["InstanceInformationList"]
                .as_array()
                .unwrap_or(&Vec::new())
            {
                let id = node["InstanceId"].as_str().unwrap_or_default().to_string();
                seen.insert(id, node.clone());
            }
            match answered["NextToken"].as_str() {
                Some(next) if !next.is_empty() => token = Some(next.to_string()),
                _ => break,
            }
        }
        seen
    }

    /// The published AMI whose name matches `pattern`, resolved the way a client resolves
    /// one before a launch.
    pub(crate) async fn image(&self, case: &str, pattern: &str) -> String {
        let body = self
            .ec2(
                case,
                "DescribeImages",
                &[
                    ("Owner.1", "amazon"),
                    ("Filter.1.Name", "name"),
                    ("Filter.1.Value.1", pattern),
                    ("Filter.2.Name", "state"),
                    ("Filter.2.Value.1", "available"),
                ],
            )
            .await;
        elements(&body, "imageId")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: no current AMI matches {pattern:?}"))
    }

    /// Launch one instance from `image_id`, running as `profile_name` when a case names one,
    /// and answer the instance id EC2 minted.
    pub(crate) async fn launch(
        &self,
        case: &str,
        image_id: &str,
        profile_name: Option<&str>,
    ) -> String {
        self.launch_with_user_data(case, image_id, profile_name, None)
            .await
    }

    /// [`Self::launch`] carrying `user_data` — the program the instance's guest runs when it
    /// comes up — for a suite about what that program left behind.
    ///
    /// The text travels as the API carries it: "the user data to make available to the
    /// instance" is base64-encoded on the wire, so a case states the program and this encodes
    /// it.
    pub(crate) async fn launch_with_user_data(
        &self,
        case: &str,
        image_id: &str,
        profile_name: Option<&str>,
        user_data: Option<&str>,
    ) -> String {
        let encoded = user_data.map(::aws_smithy_types::base64::encode);
        let mut params = vec![
            ("ImageId", image_id),
            ("InstanceType", INSTANCE_TYPE),
            ("MinCount", "1"),
            ("MaxCount", "1"),
        ];
        if let Some(name) = profile_name {
            params.push(("IamInstanceProfile.Name", name));
        }
        if let Some(encoded) = encoded.as_deref() {
            params.push(("UserData", encoded));
        }
        let body = self.ec2(case, "RunInstances", &params).await;
        elements(&body, "instanceId")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: RunInstances must mint an instance: {body}"))
    }

    /// Move `instance_id`'s launch instant to `ago` in the past and make the state move EC2
    /// owes it due now, so a case about what a command DOES is not spent waiting out the
    /// registration span.
    pub(crate) fn launched_ago(&self, instance_id: &str, ago: Duration) {
        let state = ec2::state::service_state(&*self.engine).expect("the EC2 store");
        let mut store = state.lock();
        let instance = store
            .instances
            .get_mut(instance_id)
            .unwrap_or_else(|| panic!("EC2 must hold the launched instance {instance_id}"));
        instance.launch_time = SystemTime::now() - ago;
        // The guest's first boot is owed from the launch, so moving the launch moves it too.
        instance.boot_due = Some(instance.launch_time + ec2::state::TRANSITION_DELAY);
        if let Some(pending) = instance.pending_transition.as_mut() {
            pending.due_at = std::time::Instant::now();
        }
    }

    /// Create a role carrying `grant`, an instance profile holding it, and answer the
    /// profile's name — the chain an EC2 launch names to give an instance an identity.
    pub(crate) async fn profile(&self, case: &str, name: &str, grant: &Grant) -> String {
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", name),
                ("AssumeRolePolicyDocument", EC2_TRUST_POLICY),
            ],
        )
        .await;
        match grant {
            Grant::Attached(policy_arn) => {
                self.iam(
                    case,
                    "AttachRolePolicy",
                    &[("RoleName", name), ("PolicyArn", policy_arn)],
                )
                .await;
            }
            Grant::Nothing => {}
        }
        self.iam(case, "CreateInstanceProfile", &[("InstanceProfileName", name)])
            .await;
        self.iam(
            case,
            "AddRoleToInstanceProfile",
            &[("InstanceProfileName", name), ("RoleName", name)],
        )
        .await;
        name.to_string()
    }
}

/// A SigV4 credential scope naming `signing_name` and `region`, which is how the router
/// picks the service out and where each service reads the Region from. The harness switches
/// request authentication off, so the signature itself is never verified.
fn authorization(signing_name: &str, region: &str) -> String {
    authorization_as(ACCOUNT_KEY, signing_name, region)
}

/// [`authorization`] presenting `access_key_id`: the credential scope is also where a service
/// reads WHO the caller is, so a case that signs as a principal of its own names its key here.
fn authorization_as(access_key_id: &str, signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=00"
    )
}

/// Percent-encode one form component, leaving only RFC 3986's unreserved characters.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// The text of every `<tag>` element of an XML body, in document order.
fn elements(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        found.push(after[..end].to_string());
        rest = &after[end + close.len()..];
    }
    found
}

/// RULE: a command may be sent to the node a launched instance became, and to nothing else —
/// the premise every case of the guest-run suite rests on, and the one thing that makes a
/// registered node useful.
#[tokio::test]
async fn a_registered_node_takes_a_command_and_an_unmanaged_instance_does_not() {
    let emulator = Emulator::new();
    let image = emulator.image("the send case", AL2023_NAME_PATTERN).await;
    let registered_profile = emulator
        .profile("the send case", "send-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let registered = emulator
        .launch("the send case", &image, Some(&registered_profile))
        .await;
    emulator.launched_ago(&registered, launched_long_enough_ago());
    // An instance whose role may not register: a send naming it has no node to run on.
    let unmanaged_profile = emulator
        .profile("the send case", "send-unmanaged-role", &Grant::Nothing)
        .await;
    let unmanaged = emulator
        .launch("the send case", &image, Some(&unmanaged_profile))
        .await;
    emulator.launched_ago(&unmanaged, launched_long_enough_ago());

    let reported = emulator.nodes("the send case", REGION).await;
    let node = reported.get(&registered).unwrap_or_else(|| {
        panic!(
            "case \"the send case\": the launched instance must be a managed node, got {:?}",
            reported.keys().collect::<Vec<_>>()
        )
    });
    assert_eq!(
        node["PingStatus"].as_str(),
        Some("Online"),
        "case \"the send case\": a node that registered pings, got {node:?}"
    );

    // (case, the node the send names, whether it is served)
    let rows: Vec<(&str, &str, bool)> = vec![
        ("a node the agent registered", registered.as_str(), true),
        ("an instance that is no node", unmanaged.as_str(), false),
    ];
    for (case, instance, served) in rows {
        let (status, answered) = emulator
            .ssm(
                REGION,
                "SendCommand",
                &json!({
                    "DocumentName": "AWS-RunShellScript",
                    "InstanceIds": [instance],
                    "Parameters": {"commands": ["echo hello"]},
                }),
            )
            .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: a send to a registered node must be served, got {status}: \
                 {answered}"
            );
            assert_eq!(
                answered["Command"]["InstanceIds"],
                json!([instance]),
                "case {case:?}: the command must name the node it was sent to, got {answered}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: a send naming no registered node must be refused, got {status}: \
                 {answered}"
            );
            assert_eq!(
                answered["__type"]
                    .as_str()
                    .map(|named| named.rsplit('#').next().unwrap_or(named).to_string()),
                Some("InvalidInstanceId".to_string()),
                "case {case:?}: expected InvalidInstanceId, got {answered}"
            );
        }
    }
}
