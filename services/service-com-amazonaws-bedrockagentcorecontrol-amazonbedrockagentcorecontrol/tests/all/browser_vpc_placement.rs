// SPDX-License-Identifier: CC0-1.0

//! The EC2 subnets and security groups a caller-supplied `VpcConfig` names are
//! RESOLVED while the create or the update is served, and a request naming an id
//! EC2 holds no such resource for is refused `ValidationException` (HTTP 400)
//! with nothing persisted.
//!
//! A subnet and a security group are Amazon EC2's resources, not this control
//! plane's, and a VPC-mode browser, code interpreter or agent runtime reaches the
//! network only through the interfaces the service places in them — so an id that
//! resolves to nothing names nowhere to place one, and the resource must never
//! settle usable behind it.
//!
//! Every suite here mounts Amazon EC2 beside this control plane on ONE engine
//! ([`Emulator::with_ec2`]) — what the assembled emulator does — and creates the
//! networks it expects a request to resolve through EC2's own front door, so a
//! subnet or a group exists only because EC2 answered for it. The rows span what
//! the rule generalises over: how many ids a placement names, which member
//! carries the unresolvable one, where in the list it sits, how the resolvable
//! ones came to exist (created through EC2, a default subnet of the Region nobody
//! created, one created and then deleted), and which operation stores the
//! placement.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region every request in this suite is signed for. Not the emulator's
/// default, so the EC2 endpoint the handler resolves the placement at is the one
/// the REQUEST named rather than a constant.
const REGION: &str = "us-west-2";

/// The CIDR of the VPC these cases carve their subnets out of.
const VPC_CIDR: &str = "10.42.0.0/16";

/// The execution role every agent-runtime request in this suite names. No IAM is
/// mounted, so nothing is known about it and the role gate refuses nothing: what
/// each case turns on is the network placement alone.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AgentRuntimeRole";

/// This emulator, with Amazon EC2 — the owner of the account's subnets and
/// security groups — mounted beside the control plane on the one shared engine,
/// plus an EC2 client over the router's own in-process callback: the very seam
/// the handler's resolution reaches EC2 through.
struct Emulator {
    router: MultiServiceRouter,
    ec2: Option<::aws_sdk_ec2::Client>,
}

impl Emulator {
    /// An emulator with EC2 answering, which is what the assembled emulator is.
    fn with_ec2() -> Self {
        Self::over_engine(true)
    }

    /// An emulator with ONLY the control plane mounted: nothing can be asked
    /// about the account's network.
    fn without_ec2() -> Self {
        Self::over_engine(false)
    }

    fn over_engine(ec2: bool) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // `default()` registers no authenticator, so the suite exercises routing
        // and the handlers rather than this process's SigV4 keys.
        let mut router = MultiServiceRouter::default();
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        if ec2 {
            router.register_service(
                service_com_amazonaws_ec2_amazonec2::EmulatedService::new(Arc::clone(&engine)),
            );
        }
        let client = ec2.then(|| {
            let callback = EmbeddedEmulator::from_router(router.clone());
            ::aws_sdk_ec2::Client::from_conf(
                ::aws_sdk_ec2::Config::builder()
                    .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
                    .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
                    .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
                    .credentials_provider(callback.aws_credentials_provider())
                    .http_client(callback.http_client())
                    .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
                    .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
                    .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
                    .stalled_stream_protection(
                        ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
                    )
                    .build(),
            )
        });
        Self { router, ec2: client }
    }

    /// The EC2 client, for a suite that mounted EC2.
    fn ec2(&self) -> &::aws_sdk_ec2::Client {
        self.ec2
            .as_ref()
            .expect("an emulator built without EC2 cannot be seeded with networks")
    }

    /// One `restJson1` request to the control plane, signed for [`REGION`] —
    /// which is what the router routes on and what the handlers read the Region
    /// from.
    async fn call(&self, method: &str, uri: &str, body: Option<&Value>) -> Answer {
        let payload = body.map_or_else(Bytes::new, |value| {
            Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        });
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261022/{REGION}/bedrock-agentcore/\
             aws4_request, SignedHeaders=host, Signature=abc"
        );
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", authorization)
            .header("host", "bedrock-agentcore-control.amazonaws.com");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request.body(payload).expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// A VPC of `cidr`, as EC2 minted it.
    async fn create_vpc(&self, cidr: &str) -> String {
        self.ec2()
            .create_vpc()
            .cidr_block(cidr)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup: EC2 must create the VPC {cidr}: {error:?}"))
            .vpc()
            .and_then(|vpc| vpc.vpc_id().map(str::to_string))
            .unwrap_or_else(|| panic!("setup: EC2 must report a VpcId for {cidr}"))
    }

    /// A subnet of `cidr` in `vpc`, as EC2 minted it.
    async fn create_subnet(&self, vpc: &str, cidr: &str) -> String {
        self.ec2()
            .create_subnet()
            .vpc_id(vpc)
            .cidr_block(cidr)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup: EC2 must create the subnet {cidr}: {error:?}"))
            .subnet()
            .and_then(|subnet| subnet.subnet_id().map(str::to_string))
            .unwrap_or_else(|| panic!("setup: EC2 must report a SubnetId for {cidr}"))
    }

    /// A security group named `name` in `vpc`, as EC2 minted it.
    async fn create_security_group(&self, vpc: &str, name: &str) -> String {
        self.ec2()
            .create_security_group()
            .vpc_id(vpc)
            .group_name(name)
            .description("agentcore placement")
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup: EC2 must create the group {name}: {error:?}"))
            .group_id()
            .map(str::to_string)
            .unwrap_or_else(|| panic!("setup: EC2 must report a GroupId for {name}"))
    }

    /// One default subnet of [`REGION`], which the account holds without anybody
    /// creating it.
    async fn a_default_subnet(&self) -> String {
        self.ec2()
            .describe_subnets()
            .filters(
                ::aws_sdk_ec2::types::Filter::builder()
                    .name("default-for-az")
                    .values("true")
                    .build(),
            )
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup: EC2 must report the default subnets: {error:?}"))
            .subnets()
            .iter()
            .find_map(|subnet| subnet.subnet_id().map(str::to_string))
            .unwrap_or_else(|| panic!("setup: expected {REGION} to hold a default subnet"))
    }

    async fn delete_subnet(&self, subnet_id: &str) {
        self.ec2()
            .delete_subnet()
            .subnet_id(subnet_id)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup: EC2 must delete {subnet_id}: {error:?}"));
    }

    async fn create_browser(&self, body: &Value) -> Answer {
        self.call("PUT", "/browsers", Some(body)).await
    }

    async fn get_browser(&self, id: &str) -> Answer {
        self.call("GET", &format!("/browsers/{id}"), None).await
    }

    async fn list_custom_browsers(&self) -> Answer {
        self.call("POST", "/browsers?type=CUSTOM", Some(&json!({}))).await
    }

    async fn create_code_interpreter(&self, body: &Value) -> Answer {
        self.call("PUT", "/code-interpreters", Some(body)).await
    }

    async fn get_code_interpreter(&self, id: &str) -> Answer {
        self.call("GET", &format!("/code-interpreters/{id}"), None)
            .await
    }

    async fn create_runtime(&self, body: &Value) -> Answer {
        self.call("PUT", "/runtimes/", Some(body)).await
    }

    async fn update_runtime(&self, id: &str, body: &Value) -> Answer {
        self.call("PUT", &format!("/runtimes/{id}/"), Some(body))
            .await
    }

    async fn get_runtime(&self, id: &str) -> Answer {
        self.call("GET", &format!("/runtimes/{id}/"), None).await
    }
}

/// One answer from the emulator: its status and its JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn of(response: http::Response<Bytes>) -> Self {
        let status = response.status().as_u16();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Self { status, body }
    }

    /// The error code `restJson1` puts in `__type`.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

/// Which member of a placement carries an id EC2 resolves nothing for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unresolvable {
    /// Every id resolves.
    Nothing,
    /// A `subnets` entry names no subnet of the account.
    Subnet,
    /// A `securityGroups` entry names no security group of the account.
    SecurityGroup,
}

/// An id shaped like a subnet id (`Subnets`' member `@pattern` is
/// `^subnet-[0-9a-zA-Z]{8,17}$`, enforced at the boundary) that the account can
/// hold no subnet under: EC2 issues resource ids whose label is lowercase HEX
/// digits, so a label carrying a letter outside that alphabet names no subnet EC2
/// could ever have minted. The `index` keeps each row's id its own.
fn unresolvable_subnet(index: usize) -> String {
    format!("subnet-zzzzz{index:03}")
}

/// The same for a security group (`SecurityGroups`' member `@pattern` is
/// `^sg-[0-9a-zA-Z]{8,17}$`).
fn unresolvable_security_group(index: usize) -> String {
    format!("sg-zzzzz{index:03}")
}

/// One placement a request names: the ids it carries and which member, if any,
/// resolves to nothing.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    subnets: Vec<String>,
    security_groups: Vec<String>,
    unresolvable: Unresolvable,
}

impl Row {
    /// The `VpcConfig` JSON this row's placement is carried as.
    fn vpc_config(&self) -> Value {
        json!({"subnets": self.subnets, "securityGroups": self.security_groups})
    }

    /// The id the refusal must name: the first entry of the offending member
    /// that EC2 resolves nothing for.
    fn absent_id(&self) -> String {
        match self.unresolvable {
            Unresolvable::Nothing => String::new(),
            Unresolvable::Subnet => self
                .subnets
                .iter()
                .find(|id| id.contains('z'))
                .cloned()
                .unwrap_or_default(),
            Unresolvable::SecurityGroup => self
                .security_groups
                .iter()
                .find(|id| id.contains('z'))
                .cloned()
                .unwrap_or_default(),
        }
    }
}

/// The rows the rule is asserted over, built against networks EC2 really holds:
/// one resolvable subnet, a second one, a default subnet nobody created, and one
/// resolvable security group. Every row with an unresolvable id uses its own, so
/// a refusal naming the wrong value cannot pass.
fn rows(held_one: &str, held_two: &str, by_default: &str, group: &str) -> Vec<Row> {
    vec![
        Row {
            case: "one subnet and one group EC2 holds",
            subnets: vec![held_one.to_string()],
            security_groups: vec![group.to_string()],
            unresolvable: Unresolvable::Nothing,
        },
        Row {
            case: "a default subnet nobody created",
            subnets: vec![by_default.to_string()],
            security_groups: vec![group.to_string()],
            unresolvable: Unresolvable::Nothing,
        },
        Row {
            case: "two subnets EC2 holds",
            subnets: vec![held_one.to_string(), held_two.to_string()],
            security_groups: vec![group.to_string()],
            unresolvable: Unresolvable::Nothing,
        },
        Row {
            case: "one id EC2 holds no subnet for",
            subnets: vec![unresolvable_subnet(0)],
            security_groups: vec![group.to_string()],
            unresolvable: Unresolvable::Subnet,
        },
        Row {
            case: "one held subnet and one unresolvable id",
            subnets: vec![held_one.to_string(), unresolvable_subnet(1)],
            security_groups: vec![group.to_string()],
            unresolvable: Unresolvable::Subnet,
        },
        Row {
            case: "an unresolvable id ahead of a held subnet",
            subnets: vec![unresolvable_subnet(2), held_two.to_string()],
            security_groups: vec![group.to_string()],
            unresolvable: Unresolvable::Subnet,
        },
        Row {
            case: "held subnets and an id EC2 holds no security group for",
            subnets: vec![held_one.to_string()],
            security_groups: vec![unresolvable_security_group(3)],
            unresolvable: Unresolvable::SecurityGroup,
        },
        Row {
            case: "a held group and an unresolvable group beside it",
            subnets: vec![held_one.to_string()],
            security_groups: vec![group.to_string(), unresolvable_security_group(4)],
            unresolvable: Unresolvable::SecurityGroup,
        },
        Row {
            case: "neither member resolves — the subnet is reported",
            subnets: vec![unresolvable_subnet(5)],
            security_groups: vec![unresolvable_security_group(5)],
            unresolvable: Unresolvable::Subnet,
        },
    ]
}

/// Create the networks every row resolves through, and hand back
/// `(held_one, held_two, by_default, group)`.
async fn seed_networks(emulator: &Emulator) -> (String, String, String, String) {
    let vpc = emulator.create_vpc(VPC_CIDR).await;
    let held_one = emulator.create_subnet(&vpc, "10.42.1.0/24").await;
    let held_two = emulator.create_subnet(&vpc, "10.42.2.0/24").await;
    let by_default = emulator.a_default_subnet().await;
    let group = emulator.create_security_group(&vpc, "agentcore-placement").await;
    (held_one, held_two, by_default, group)
}

/// Assert that `answer` is this service's refusal of a network resource that is
/// not there: HTTP 400 `ValidationException`, the sentence naming the id the
/// request carried, the member path in the `fieldList` the shape declares for it,
/// and `reason` saying a field failed validation.
fn assert_refused(case: &str, answer: &Answer, row: &Row, member_prefix: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 for a network resource that is not there, got {} and \
         body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got body {}",
        answer.body
    );
    let absent = row.absent_id();
    // The sentence and the member path are spelled out here rather than read off
    // the implementation, so a drift in either is a failing test.
    let (expected_message, expected_member) = match row.unresolvable {
        Unresolvable::Subnet => (
            format!("Subnet not found: {absent}. Verify the subnet ID is correct and retry."),
            format!("{member_prefix}.subnets"),
        ),
        Unresolvable::SecurityGroup => (
            format!(
                "Security group not found: {absent}. Verify the security group ID is correct \
                 and retry."
            ),
            format!("{member_prefix}.securityGroups"),
        ),
        Unresolvable::Nothing => panic!("case {case:?}: a resolvable row is not refused"),
    };
    assert_eq!(
        answer.body["message"],
        json!(expected_message),
        "case {case:?}: unexpected refusal message, got body {}",
        answer.body
    );
    assert_eq!(
        answer.body["fieldList"][0]["name"],
        json!(expected_member),
        "case {case:?}: the refusal must name the member that failed, got body {}",
        answer.body
    );
    assert_eq!(
        answer.body["reason"],
        json!("FieldValidationFailed"),
        "case {case:?}: a member of the request failed validation, got body {}",
        answer.body
    );
}

/// `CreateBrowser` resolves every id of a VPC-mode placement against EC2: a
/// placement EC2 holds every resource of yields a browser that reads back READY
/// with that network, and one carrying an id it resolves nothing for is refused
/// 400 `ValidationException` with no browser recorded.
#[tokio::test]
async fn create_browser_resolves_its_vpc_placement_against_ec2() {
    let emulator = Emulator::with_ec2();
    let (held_one, held_two, by_default, group) = seed_networks(&emulator).await;
    let mut served = 0_usize;

    for (index, row) in rows(&held_one, &held_two, &by_default, &group)
        .iter()
        .enumerate()
    {
        let case = row.case;
        let name = format!("vpc_browser_{index}");
        let network = json!({"networkMode": "VPC", "vpcConfig": row.vpc_config()});
        let created = emulator
            .create_browser(&json!({"name": name, "networkConfiguration": network}))
            .await;

        if row.unresolvable == Unresolvable::Nothing {
            served += 1;
            assert_eq!(
                created.status, 202,
                "case {case:?}: expected HTTP 202 from CreateBrowser, got {} and body {}",
                created.status, created.body
            );
            // The round trip: the placement the create resolved is the one the
            // stored browser reports, and it is usable.
            let id = created.body["browserId"].as_str().unwrap_or_default();
            let read = emulator.get_browser(id).await;
            assert_eq!(
                read.body["networkConfiguration"],
                network,
                "case {case:?}: GetBrowser must report the placement the create stored, got {} \
                 and body {}",
                read.status,
                read.body
            );
            assert_eq!(
                read.body["status"],
                json!("READY"),
                "case {case:?}: a browser on a resolved placement is READY, got body {}",
                read.body
            );
            continue;
        }

        assert_refused(case, &created, row, "networkConfiguration.vpcConfig");
        // Refused BEFORE anything is recorded: no browser of that name is
        // listed, so a client cannot see one it would believe is usable.
        let listed = emulator.list_custom_browsers().await;
        assert_eq!(
            listed.body["browserSummaries"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            served,
            "case {case:?}: only the served creates may have persisted, got {} and body {}",
            listed.status,
            listed.body
        );
    }
}

/// A browser whose `networkMode` is the public one names no placement at all, so
/// nothing is resolved for it: the create is served with EC2 mounted and
/// answering, and the browser reads back READY.
#[tokio::test]
async fn a_public_browser_resolves_no_placement() {
    let case = "networkMode PUBLIC with EC2 answering";
    // An EC2 holding no network of the account's own: a placement-based gate
    // applied to this request would refuse it.
    let emulator = Emulator::with_ec2();
    let created = emulator
        .create_browser(&json!({
            "name": "public_browser",
            "networkConfiguration": {"networkMode": "PUBLIC"},
        }))
        .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected HTTP 202 from CreateBrowser, got {} and body {}",
        created.status, created.body
    );
    let id = created.body["browserId"].as_str().unwrap_or_default();
    let read = emulator.get_browser(id).await;
    assert_eq!(
        read.body["status"],
        json!("READY"),
        "case {case:?}: expected the browser to be READY, got {} and body {}",
        read.status,
        read.body
    );
}

/// Nothing is cached: a subnet that stops existing stops resolving, so the same
/// placement that was served before the delete is refused after it.
#[tokio::test]
async fn a_subnet_that_stops_existing_stops_resolving() {
    let case = "a subnet deleted between two creates";
    let emulator = Emulator::with_ec2();
    let vpc = emulator.create_vpc(VPC_CIDR).await;
    let subnet = emulator.create_subnet(&vpc, "10.42.3.0/24").await;
    let group = emulator.create_security_group(&vpc, "agentcore-deleted").await;
    let network = json!({
        "networkMode": "VPC",
        "vpcConfig": {"subnets": [&subnet], "securityGroups": [&group]},
    });

    let before = emulator
        .create_browser(&json!({"name": "before_delete", "networkConfiguration": network}))
        .await;
    assert_eq!(
        before.status, 202,
        "case {case:?}: expected the first create to be served, got {} and body {}",
        before.status, before.body
    );

    emulator.delete_subnet(&subnet).await;

    let after = emulator
        .create_browser(&json!({"name": "after_delete", "networkConfiguration": network}))
        .await;
    assert_eq!(
        after.status, 400,
        "case {case:?}: expected the second create to be refused, got {} and body {}",
        after.status, after.body
    );
    assert_eq!(
        after.body["message"],
        json!(format!(
            "Subnet not found: {subnet}. Verify the subnet ID is correct and retry."
        )),
        "case {case:?}: the refusal must name the deleted subnet, got body {}",
        after.body
    );
    let listed = emulator.list_custom_browsers().await;
    assert_eq!(
        listed.body["browserSummaries"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "case {case:?}: only the first create may have persisted, got body {}",
        listed.body
    );
}

/// `CreateCodeInterpreter` resolves the placement it stores the same way: the
/// same gate, on the same member of the same shape, of the other create that
/// binds a sandbox resource to a caller-named VPC.
#[tokio::test]
async fn create_code_interpreter_resolves_its_vpc_placement_against_ec2() {
    let emulator = Emulator::with_ec2();
    let (held_one, held_two, by_default, group) = seed_networks(&emulator).await;

    for (index, row) in rows(&held_one, &held_two, &by_default, &group)
        .iter()
        .enumerate()
    {
        let case = row.case;
        let name = format!("vpc_interpreter_{index}");
        let network = json!({"networkMode": "VPC", "vpcConfig": row.vpc_config()});
        let created = emulator
            .create_code_interpreter(&json!({"name": name, "networkConfiguration": network}))
            .await;

        if row.unresolvable == Unresolvable::Nothing {
            assert_eq!(
                created.status, 202,
                "case {case:?}: expected HTTP 202 from CreateCodeInterpreter, got {} and body {}",
                created.status, created.body
            );
            let id = created.body["codeInterpreterId"].as_str().unwrap_or_default();
            let read = emulator.get_code_interpreter(id).await;
            assert_eq!(
                read.body["networkConfiguration"],
                network,
                "case {case:?}: GetCodeInterpreter must report the placement the create stored, \
                 got {} and body {}",
                read.status,
                read.body
            );
            continue;
        }

        assert_refused(case, &created, row, "networkConfiguration.vpcConfig");
    }
}

/// A `containerConfiguration` artifact, whose URI satisfies `RuntimeContainerUri`'s
/// ECR pattern. It names no object-storage location, so no object storage has to
/// be mounted for the create to be served.
fn container_artifact() -> Value {
    json!({"containerConfiguration": {"containerUri": "public.ecr.aws/agentcore/demo:v1"}})
}

/// `CreateAgentRuntime` resolves the placement its `networkConfiguration` names,
/// which the runtime family spells `networkModeConfig` — the same `VpcConfig`
/// shape, so the same gate and the same refusal, reported on that member's own
/// path.
#[tokio::test]
async fn create_agent_runtime_resolves_its_vpc_placement_against_ec2() {
    let emulator = Emulator::with_ec2();
    let (held_one, held_two, by_default, group) = seed_networks(&emulator).await;

    for (index, row) in rows(&held_one, &held_two, &by_default, &group)
        .iter()
        .enumerate()
    {
        let case = row.case;
        let network = json!({"networkMode": "VPC", "networkModeConfig": row.vpc_config()});
        let created = emulator
            .create_runtime(&json!({
                "agentRuntimeName": format!("vpc_runtime_{index}"),
                "agentRuntimeArtifact": container_artifact(),
                "roleArn": ROLE_ARN,
                "networkConfiguration": network,
            }))
            .await;

        if row.unresolvable == Unresolvable::Nothing {
            assert_eq!(
                created.status, 202,
                "case {case:?}: expected HTTP 202 from CreateAgentRuntime, got {} and body {}",
                created.status, created.body
            );
            let id = created.body["agentRuntimeId"].as_str().unwrap_or_default();
            let read = emulator.get_runtime(id).await;
            assert_eq!(
                read.body["networkConfiguration"],
                network,
                "case {case:?}: GetAgentRuntime must report the placement the create stored, got \
                 {} and body {}",
                read.status,
                read.body
            );
            continue;
        }

        assert_refused(
            case,
            &created,
            row,
            "networkConfiguration.networkModeConfig",
        );
    }
}

/// `UpdateAgentRuntime` resolves the placement of the version it would publish:
/// the same gate on the other operation that stores one. A refused update
/// publishes no version, so the runtime still reports the network it was created
/// with.
#[tokio::test]
async fn update_agent_runtime_resolves_its_vpc_placement_against_ec2() {
    let emulator = Emulator::with_ec2();
    let (held_one, held_two, by_default, group) = seed_networks(&emulator).await;
    let created_with = json!({"networkMode": "PUBLIC"});

    for (index, row) in rows(&held_one, &held_two, &by_default, &group)
        .iter()
        .enumerate()
    {
        let case = row.case;
        let created = emulator
            .create_runtime(&json!({
                "agentRuntimeName": format!("updated_runtime_{index}"),
                "agentRuntimeArtifact": container_artifact(),
                "roleArn": ROLE_ARN,
                "networkConfiguration": created_with,
            }))
            .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: seeding the runtime: expected HTTP 202, got {} and body {}",
            created.status, created.body
        );
        let id = created.body["agentRuntimeId"]
            .as_str()
            .expect("the create reports an agentRuntimeId")
            .to_string();

        let network = json!({"networkMode": "VPC", "networkModeConfig": row.vpc_config()});
        let updated = emulator
            .update_runtime(
                &id,
                &json!({
                    "agentRuntimeArtifact": container_artifact(),
                    "roleArn": ROLE_ARN,
                    "networkConfiguration": network,
                }),
            )
            .await;

        if row.unresolvable == Unresolvable::Nothing {
            assert_eq!(
                updated.status, 202,
                "case {case:?}: expected HTTP 202 from UpdateAgentRuntime, got {} and body {}",
                updated.status, updated.body
            );
            assert_eq!(
                updated.body["agentRuntimeVersion"],
                json!("2"),
                "case {case:?}: a served update publishes the next version, got body {}",
                updated.body
            );
            continue;
        }

        assert_refused(
            case,
            &updated,
            row,
            "networkConfiguration.networkModeConfig",
        );
        let read = emulator.get_runtime(&id).await;
        assert_eq!(
            read.body["agentRuntimeVersion"],
            json!("1"),
            "case {case:?}: a refused update publishes no version, got {} and body {}",
            read.status,
            read.body
        );
        assert_eq!(
            read.body["networkConfiguration"],
            created_with,
            "case {case:?}: the runtime must still report the network it was created with, got \
             body {}",
            read.body
        );
    }
}

/// An emulator assembled WITHOUT EC2 cannot say a subnet or a group is absent, so
/// it refuses no request for one.
///
/// The request such an emulator makes is answered by the router's own
/// `UnknownService` — a statement about the emulator, not about the caller's
/// network — and reporting it as a missing subnet would tell a client its network
/// is not there when nothing looked for it. The create is served and the
/// placement the request named is stored as it stands.
#[tokio::test]
async fn an_emulator_without_ec2_refuses_no_placement() {
    let case = "no EC2 mounted";
    let emulator = Emulator::without_ec2();
    let network = json!({
        "networkMode": "VPC",
        "vpcConfig": {
            "subnets": [unresolvable_subnet(9)],
            "securityGroups": [unresolvable_security_group(9)],
        },
    });

    let created = emulator
        .create_browser(&json!({"name": "unresolved_browser", "networkConfiguration": network}))
        .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected HTTP 202 from CreateBrowser, got {} and body {}",
        created.status, created.body
    );
    let id = created.body["browserId"].as_str().unwrap_or_default();
    let read = emulator.get_browser(id).await;
    assert_eq!(
        read.body["networkConfiguration"],
        network,
        "case {case:?}: the placement the request named must be stored as it stands, got {} and \
         body {}",
        read.status,
        read.body
    );
}
