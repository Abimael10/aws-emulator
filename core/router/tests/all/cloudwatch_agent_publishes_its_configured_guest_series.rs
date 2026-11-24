// SPDX-License-Identifier: CC0-1.0

//! The CLOUDWATCH AGENT installed in an instance's guest through Systems Manager Run Command,
//! on the whole assembled emulator a client talks to.
//!
//! RULE under test: an agent that was installed in a guest, handed a configuration and started
//! publishes EXACTLY the series that configuration declares — each measurement it names, taken
//! on that machine, and every StatsD value its listener receives — into the namespace and under
//! the dimensions the configuration declares, and publishes nothing for a measurement it does
//! not name and nothing at all while the agent is stopped.
//!
//! This suite drives the ASSEMBLED router — every service mounted on one engine, as
//! `core/router/src/lib.rs` assembles it — because the behaviour is derived across four of
//! those services: a client creates the role and instance profile through IAM, the instance
//! through Amazon EC2, installs and configures the agent through Systems Manager's Run Command,
//! and reads the series back through Amazon CloudWatch's own `GetMetricStatistics`. Nothing
//! reaches into a state struct: every fact is written and read through the service that owns
//! it, and the agent's own work happens inside the guest, as the script's own commands.
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.
//!
//! Nothing here waits a fixed time: every span this suite crosses — Amazon EC2's move to
//! `running`, the node's registration, the invocation's move off `Pending` and the arrival of a
//! datapoint — is a stored due instant, so a case LOOKS AGAIN until the condition holds or
//! [`DEADLINE`] passes, and no assertion is made about elapsed time.
//!
//! The rows span what the rule generalises over: which measurements a configuration declares
//! (two machines, two configurations, two namespaces), which port its listener opens on, and
//! how a program in the guest sends a datagram (the shell's own `/dev/udp` redirection and the
//! transfer program's datagram form).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The IAM API version a client sends in an awsQuery body.
const IAM_API_VERSION: &str = "2010-05-08";

/// The public Systems Manager parameter a client resolves the Amazon Linux 2023 image id
/// through, which is the image these instances are launched from.
const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The AWS-managed policies a client attaches to an instance profile's role so the agent in the
/// guest registers the node, takes the commands sent to it and may publish metrics.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// See [`SSM_CORE_POLICY`].
const AGENT_SERVER_POLICY: &str = "arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy";

/// The service principal a role has to admit for Amazon EC2 to act as it on an instance's
/// behalf.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The document Run Command runs a shell script on a Linux node with.
const SHELL_DOCUMENT: &str = "AWS-RunShellScript";

/// The instance state a client's `instance_running` waiter waits for.
const RUNNING: &str = "running";

/// The `PingStatus` of a node whose agent is responding.
const ONLINE: &str = "Online";

/// The invocation status a script that ran to success ends in.
const SUCCESS_STATUS: &str = "Success";

/// The statuses an invocation has reached its end in
/// (`smithy_query(query="shape",shape="CommandInvocationStatus")`).
const TERMINAL_STATUSES: &[&str] = &["Success", "Cancelled", "TimedOut", "Failed"];

/// The `X-Amz-Target` prefix CloudWatch's JSON wire carries, and the signing name that routes a
/// call to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// See [`CLOUDWATCH_TARGET_PREFIX`].
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The package the CloudWatch agent is installed as, and the directory it installs under —
/// where its control program sits, which is what the documented procedure invokes.
const AGENT_PACKAGE: &str = "amazon-cloudwatch-agent";

/// See [`AGENT_PACKAGE`].
const AGENT_CONTROL: &str = "/opt/aws/amazon-cloudwatch-agent/bin/amazon-cloudwatch-agent-ctl";

/// How often each configuration below has the agent take a measurement, in seconds — short, so
/// a case reads a collection the agent took rather than waiting out a minute of wall clock.
const COLLECTION_INTERVAL: u64 = 10;

/// The period a `GetMetricStatistics` read below asks for: "must be … a multiple of 60"
/// (`GetMetricStatisticsInput$Period`), which is the granularity a regular-resolution datapoint
/// is stored at.
const PERIOD_SECONDS: i64 = 60;

/// How long a case is given for a condition another component's clock owes.
///
/// Generous on purpose: each span is a stored due instant and nothing sleeps for one, so a case
/// looks again until the condition holds. All this bound is for is that a move that never comes
/// fails instead of hanging; nothing asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before polling again. Not a bound on anything.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and where its Region comes from; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The body as the service wrote it, which is what an XML wire is read from.
    text: String,
    /// The same body parsed, for a JSON wire.
    body: Value,
}

/// One request this suite sends: where it goes, and what it carries.
struct Call<'a> {
    /// The signing name whose endpoint it is sent to, which is what routes it.
    signing_name: &'a str,
    content_type: &'a str,
    /// `x-amz-target`, for the RPC wires that carry the operation in a header.
    target: Option<String>,
    body: String,
}

/// One request at `call.signing_name`'s endpoint in [`REGION`].
fn send(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime, call: Call<'_>) -> Answer {
    let Call {
        signing_name,
        content_type,
        target,
        body,
    } = call;
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    if let Some(target) = target {
        builder = builder.header("x-amz-target", target);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer { status, text, body }
}

/// A form body carrying `action`, `version` and the parameters as written.
fn form_body(action: &str, version: &str, parameters: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// One form-encoded action at `signing_name`'s endpoint — the ec2Query and awsQuery wires.
fn form_call<'a>(
    signing_name: &'a str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> Call<'a> {
    Call {
        signing_name,
        content_type: "application/x-www-form-urlencoded",
        target: None,
        body: form_body(action, version, parameters),
    }
}

/// One ec2Query action of Amazon EC2 that must be served, as its XML body.
fn ec2_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let answer = send(
        router,
        runtime,
        form_call("ec2", EC2_API_VERSION, action, parameters),
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Amazon EC2 {action} must be served, got {answer:?}"
    );
    answer.text
}

/// One awsQuery action of IAM that must be served.
fn iam_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) {
    let answer = send(
        router,
        runtime,
        form_call("iam", IAM_API_VERSION, action, parameters),
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: IAM {action} must be served, got {answer:?}"
    );
}

/// One Systems Manager operation over its awsJson1_1 wire that must be served.
fn ssm_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let answer = send(
        router,
        runtime,
        Call {
            signing_name: "ssm",
            content_type: "application/x-amz-json-1.1",
            target: Some(format!("AmazonSSM.{operation}")),
            body: body.to_string(),
        },
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Systems Manager {operation} must be served, got {answer:?}"
    );
    answer.body
}

/// One CloudWatch operation over its JSON wire that must be served.
fn cloudwatch_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let answer = send(
        router,
        runtime,
        Call {
            signing_name: CLOUDWATCH_SIGNING_NAME,
            content_type: "application/x-amz-json-1.0",
            target: Some(format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}")),
            body: body.to_string(),
        },
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: CloudWatch {operation} must be served, got {answer:?}"
    );
    answer.body
}

/// A trust policy admitting `principal` and nothing else, written with none of `&`, `=`, `+`
/// or `%` in it.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// Build the instance profile the instances below run as, the way a client building one for the
/// agent does: the managed-node policy, so the node registers and takes commands, and the
/// agent's own server policy.
fn create_profile(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    profile: &str,
    role: &str,
) {
    let trust = trust_policy(EC2_PRINCIPAL);
    iam_ok(
        router,
        runtime,
        profile,
        "CreateRole",
        &[("RoleName", role), ("AssumeRolePolicyDocument", &trust)],
    );
    for policy in [SSM_CORE_POLICY, AGENT_SERVER_POLICY] {
        iam_ok(
            router,
            runtime,
            profile,
            "AttachRolePolicy",
            &[("RoleName", role), ("PolicyArn", policy)],
        );
    }
    iam_ok(
        router,
        runtime,
        profile,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile)],
    );
    iam_ok(
        router,
        runtime,
        profile,
        "AddRoleToInstanceProfile",
        &[("InstanceProfileName", profile), ("RoleName", role)],
    );
}

/// The image id the public Systems Manager parameter resolves to.
fn image_id(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> String {
    let answered = ssm_ok(
        router,
        runtime,
        "the image",
        "GetParameter",
        &json!({ "Name": AL2023_PARAMETER }),
    );
    answered["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("GetParameter must report an image id, got {answered}"))
        .to_string()
}

/// Launch one instance running as `profile`, as the instance id Amazon EC2 answered with.
fn launch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    image: &str,
    profile: &str,
) -> String {
    let launched = ec2_ok(
        router,
        runtime,
        case,
        "RunInstances",
        &[
            ("ImageId", image),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("InstanceType", "t3.micro"),
            ("IamInstanceProfile.Name", profile),
        ],
    );
    element(&launched, "instanceId")
}

/// Look again until Amazon EC2 reports `instance_id` running — the condition a client's
/// `instance_running` waiter waits for — and then until Systems Manager reports it as a managed
/// node whose agent is responding.
fn when_managed(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) {
    let until = Instant::now() + DEADLINE;
    loop {
        let described = ec2_ok(
            router,
            runtime,
            case,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        );
        let state = block_of(&described, "instanceState").unwrap_or_default();
        if elements(state, "name").iter().any(|name| name == RUNNING) {
            break;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected Amazon EC2 to report {instance_id} {RUNNING}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
    loop {
        let reported = ssm_ok(
            router,
            runtime,
            case,
            "DescribeInstanceInformation",
            &json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] }),
        );
        let online = reported["InstanceInformationList"]
            .as_array()
            .is_some_and(|listed| {
                listed
                    .iter()
                    .any(|node| node["PingStatus"].as_str() == Some(ONLINE))
            });
        if online {
            break;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to become a managed node reporting {ONLINE}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Send `commands` to the guest of `instance_id` as one shell script, and look again until the
/// invocation has reached a terminal status — answering the whole `GetCommandInvocation`
/// report, which is what a client reads the script's two streams and its exit code out of.
fn ran_in_guest(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
    commands: &[String],
) -> Value {
    let sent = ssm_ok(
        router,
        runtime,
        case,
        "SendCommand",
        &json!({
            "DocumentName": SHELL_DOCUMENT,
            "InstanceIds": [instance_id],
            "Parameters": { "commands": commands },
        }),
    );
    let command_id = sent["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: SendCommand must mint a command id, got {sent}"))
        .to_string();
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = ssm_ok(
            router,
            runtime,
            case,
            "GetCommandInvocation",
            &json!({ "CommandId": command_id, "InstanceId": instance_id }),
        );
        if reported["Status"]
            .as_str()
            .is_some_and(|status| TERMINAL_STATUSES.contains(&status))
        {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the invocation of {command_id} to reach a terminal status, \
             last read {reported}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What the script wrote on standard output, as `GetCommandInvocation` reports it.
fn written(report: &Value) -> String {
    report["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The value the marker `marker` carries in what a script wrote: the rest of the line that
/// starts with it, which is how a client reads a fact back out of a guest.
fn marker(output: &str, marker: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{marker} ")))
        .map(str::trim)
        .map(str::to_string)
}

/// One agent configuration this suite hands a guest: which namespace it publishes under, which
/// measurements it declares and which port its StatsD listener opens on.
struct AgentConfiguration<'a> {
    /// The case this configuration belongs to.
    case: &'a str,
    /// "The namespace to use for the metrics collected by the agent".
    namespace: &'a str,
    /// The measurements its `mem` section declares.
    measurements: &'a [&'a str],
    /// The port its `statsd` section's `service_address` names.
    statsd_port: u16,
}

impl AgentConfiguration<'_> {
    /// The configuration document, as a client writes one for the agent.
    fn document(&self) -> String {
        let measurements = self
            .measurements
            .iter()
            .map(|measurement| format!("\"{measurement}\""))
            .collect::<Vec<String>>()
            .join(", ");
        format!(
            "{{\n\
             \x20 \"agent\": {{\"metrics_collection_interval\": {COLLECTION_INTERVAL}}},\n\
             \x20 \"metrics\": {{\n\
             \x20   \"namespace\": \"{}\",\n\
             \x20   \"append_dimensions\": {{\"InstanceId\": \"${{aws:InstanceId}}\"}},\n\
             \x20   \"metrics_collected\": {{\n\
             \x20     \"mem\": {{\"measurement\": [{measurements}]}},\n\
             \x20     \"statsd\": {{\"service_address\": \":{}\", \
             \"metrics_collection_interval\": {COLLECTION_INTERVAL}}}\n\
             \x20   }}\n\
             \x20 }}\n\
             }}",
            self.namespace, self.statsd_port
        )
    }

    /// The script that installs the agent, hands it this configuration, starts it and then
    /// echoes what the guest reports about it — the documented procedure, marker by marker.
    fn install_script(&self) -> Vec<String> {
        let port = format!("{:04X}", self.statsd_port);
        vec![
            "set -e".to_string(),
            format!("dnf install -y {AGENT_PACKAGE}"),
            "mkdir -p /opt/aws/amazon-cloudwatch-agent/etc".to_string(),
            format!(
                "cat > /tmp/cwagent.json <<'EOF'\n{}\nEOF",
                self.document()
            ),
            format!("{AGENT_CONTROL} -a fetch-config -m ec2 -s -c file:/tmp/cwagent.json"),
            format!("echo \"CWAGENT-RPM $(rpm -q {AGENT_PACKAGE})\""),
            format!("echo \"CWAGENT-UNIT $(systemctl is-active {AGENT_PACKAGE} || true)\""),
            format!(
                "echo \"CWAGENT-STATUS $({AGENT_CONTROL} -a status -m ec2 | tr -d ' ' \
                 | grep -o '\\\"status\\\":\\\"[a-z]*\\\"' | head -1)\""
            ),
            // The same document, read by a program of the guest rather than by a text filter:
            // the status the agent reports is a JSON document, so an inline interpreter reading
            // the command's standard input parses it.
            format!(
                "echo \"CWAGENT-PARSED $({AGENT_CONTROL} -a status -m ec2 \
                 | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"status\"])')\""
            ),
            format!("echo \"CWAGENT-LISTEN $(grep -c :{port} /proc/net/udp || true)\""),
            "echo \"CWAGENT-MEMTOTAL $(grep MemTotal /proc/meminfo | tr -s ' ' | cut -d' ' -f2)\""
                .to_string(),
            "echo \"CWAGENT-MEMAVAIL $(grep MemAvailable /proc/meminfo | tr -s ' ' \
             | cut -d' ' -f2)\""
                .to_string(),
        ]
    }
}

/// How a program inside the guest hands a datagram to a listener of its own machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sender {
    /// The shell's own redirection to the machine's datagram address.
    Redirection,
    /// The transfer program's datagram form.
    Transfer,
}

impl Sender {
    /// The script line that sends `payload` to `port` on this machine's loopback address.
    fn line(self, port: u16, payload: &str) -> String {
        match self {
            Self::Redirection => {
                format!("printf '%s\\n' '{payload}' > /dev/udp/127.0.0.1/{port}")
            }
            Self::Transfer => {
                format!("printf '%s\\n' '{payload}' | nc -u -w 1 127.0.0.1 {port}")
            }
        }
    }
}

/// Now, as the epoch seconds a CloudWatch read windows by.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// What one series holds over a window the whole suite falls in: how many observations it
/// carries, what they sum to and the largest of them — the three statistics a client reads a
/// counter and a gauge with.
#[derive(Debug, Clone, Default, PartialEq)]
struct Series {
    /// How many observations the window holds.
    samples: f64,
    /// What they sum to.
    sum: f64,
    /// The largest of them, for a series that holds any.
    maximum: Option<f64>,
}

impl Series {
    /// Whether the series holds no observation at all.
    fn is_empty(&self) -> bool {
        self.samples == 0.0 && self.maximum.is_none()
    }
}

/// Read one series exactly as a client reads one: `GetMetricStatistics` over a window the whole
/// suite falls in, dimensioned on the instance the agent runs on.
fn series(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    namespace: &str,
    metric: &str,
    instance_id: &str,
) -> Series {
    let now = now_secs();
    let answered = cloudwatch_ok(
        router,
        runtime,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": namespace,
            "MetricName": metric,
            "Dimensions": [{"Name": "InstanceId", "Value": instance_id}],
            // An hour back and a minute on, so every datapoint taken by now is inside the read
            // whatever minute it was taken in.
            "StartTime": now - 3_600.0,
            "EndTime": now + 60.0,
            "Period": PERIOD_SECONDS,
            "Statistics": ["SampleCount", "Sum", "Maximum"],
        }),
    );
    let mut held = Series::default();
    for point in answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        held.samples += point["SampleCount"].as_f64().unwrap_or_default();
        held.sum += point["Sum"].as_f64().unwrap_or_default();
        if let Some(maximum) = point["Maximum"].as_f64() {
            held.maximum = Some(held.maximum.map_or(maximum, |held: f64| held.max(maximum)));
        }
    }
    held
}

/// Look again until the series `metric` holds an observation, answering what it holds.
fn when_published(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    namespace: &str,
    metric: &str,
    instance_id: &str,
) -> Series {
    let until = Instant::now() + DEADLINE;
    loop {
        let held = series(router, runtime, case, namespace, metric, instance_id);
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the agent to publish {namespace}/{metric} for \
             {instance_id}, and it published nothing"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[test]
fn an_ssm_installed_agent_publishes_exactly_the_series_its_configuration_declares() {
    std::thread::Builder::new()
        .name("cloudwatch-agent-in-a-guest".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            // Signatures are not what this suite is about; the router still reads the SigV4
            // credential scope, which is where each call's Region comes from.
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            create_profile(&router, &runtime, "cwagent-node", "cwagent-node-role");
            let image = image_id(&router, &runtime);

            // Two machines, each handed a configuration of its own: what each publishes is what
            // ITS document declares, which is the rule this table spans.
            let configurations = [
                AgentConfiguration {
                    case: "the used-share configuration",
                    namespace: "Dcwagent/UsedShare",
                    measurements: &["mem_used_percent", "mem_available_percent"],
                    statsd_port: 8125,
                },
                AgentConfiguration {
                    case: "the total-memory configuration",
                    namespace: "Dcwagent/TotalMemory",
                    // Named without its section's prefix, which is how the configuration
                    // reference writes a measurement too.
                    measurements: &["total"],
                    statsd_port: 8135,
                },
            ];
            let mut nodes: Vec<(String, String, String)> = Vec::new();
            for configuration in &configurations {
                let node = launch(
                    &router,
                    &runtime,
                    configuration.case,
                    &image,
                    "cwagent-node",
                );
                when_managed(&router, &runtime, configuration.case, &node);
                let report = ran_in_guest(
                    &router,
                    &runtime,
                    configuration.case,
                    &node,
                    &configuration.install_script(),
                );
                assert_eq!(
                    report["Status"].as_str(),
                    Some(SUCCESS_STATUS),
                    "case {:?}: the install and configure script must succeed, got {report}",
                    configuration.case
                );
                let output = written(&report);
                let case = configuration.case;
                let rpm = marker(&output, "CWAGENT-RPM").unwrap_or_default();
                assert!(
                    rpm.starts_with(&format!("{AGENT_PACKAGE}-")),
                    "case {case:?}: the guest must report the installed agent package, got \
                     {rpm:?}; stdout was {output:?}"
                );
                assert_eq!(
                    marker(&output, "CWAGENT-UNIT").as_deref(),
                    Some("active"),
                    "case {case:?}: the agent's unit must be active once it is started, stdout \
                     was {output:?}"
                );
                assert_eq!(
                    marker(&output, "CWAGENT-STATUS").as_deref(),
                    Some("\"status\":\"running\""),
                    "case {case:?}: the control program must report the agent running, stdout \
                     was {output:?}"
                );
                assert_eq!(
                    marker(&output, "CWAGENT-PARSED").as_deref(),
                    Some("running"),
                    "case {case:?}: the status document must be one a program of the guest can \
                     parse, stdout was {output:?}"
                );
                assert_eq!(
                    marker(&output, "CWAGENT-LISTEN").as_deref(),
                    Some("1"),
                    "case {case:?}: the kernel must report the configured StatsD listener, \
                     stdout was {output:?}"
                );
                let figure = |name: &str| {
                    marker(&output, name)
                        .and_then(|value| value.parse::<f64>().ok())
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: the guest must report {name}, stdout was {output:?}")
                        })
                };
                let (total, available) = (figure("CWAGENT-MEMTOTAL"), figure("CWAGENT-MEMAVAIL"));
                assert!(
                    total > 0.0 && available > 0.0 && available < total,
                    "case {case:?}: the guest's own memory figures must be a reading of a \
                     machine, got total {total} available {available}"
                );
                let used_share = (total - available) / total * 100.0;
                nodes.push((node, configuration.namespace.to_string(), format!("{used_share}")));
            }

            // Each machine publishes the measurements ITS configuration declares — taken on
            // that machine, so the share the series carries is the share the guest's own
            // figures come to — and nothing for a measurement it does not declare.
            let (first_node, first_namespace, first_share) = nodes[0].clone();
            let (second_node, second_namespace, _) = nodes[1].clone();
            let used_share: f64 = first_share.parse().expect("the guest's own share");
            let published = when_published(
                &router,
                &runtime,
                configurations[0].case,
                &first_namespace,
                "mem_used_percent",
                &first_node,
            );
            let reported = published.maximum.unwrap_or_default();
            assert!(
                (reported - used_share).abs() < 0.5,
                "case {:?}: expected the agent's mem_used_percent to be the share the guest \
                 reports ({used_share}), got {reported}",
                configurations[0].case
            );
            let available = when_published(
                &router,
                &runtime,
                configurations[0].case,
                &first_namespace,
                "mem_available_percent",
                &first_node,
            );
            let available_share = available.maximum.unwrap_or_default();
            assert!(
                (available_share + used_share - 100.0).abs() < 0.5,
                "case {:?}: expected the two shares of one machine to come to 100, got \
                 {available_share} beside {used_share}",
                configurations[0].case
            );
            let total = when_published(
                &router,
                &runtime,
                configurations[1].case,
                &second_namespace,
                "mem_total",
                &second_node,
            );
            assert!(
                total.maximum.unwrap_or_default() > 0.0,
                "case {:?}: expected the declared total to be published, got {total:?}",
                configurations[1].case
            );
            // (the case, the machine, the namespace, the measurement its document never
            // declared)
            let undeclared: &[(&str, &String, &String, &str)] = &[
                (
                    "a measurement the first document never declared",
                    &first_node,
                    &first_namespace,
                    "mem_total",
                ),
                (
                    "a filesystem measurement neither document declared",
                    &first_node,
                    &first_namespace,
                    "disk_used_percent",
                ),
                (
                    "a measurement the second document never declared",
                    &second_node,
                    &second_namespace,
                    "mem_used_percent",
                ),
            ];
            for (case, node, namespace, metric) in undeclared {
                let held = series(&router, &runtime, case, namespace, metric, node);
                assert!(
                    held.is_empty(),
                    "case {case:?}: a measurement no configuration declares must never appear, \
                     got {held:?}"
                );
            }

            // A StatsD value sent inside the guest appears as a datapoint of the metric the
            // datagram named, under the configured namespace and dimensions — whichever way a
            // program of the guest sends it.
            // (the case, how the datagram is sent, the metric it names, the value it carries)
            let sent: &[(&str, Sender, &str, f64)] = &[
                (
                    "a datagram the shell redirects",
                    Sender::Redirection,
                    "orders_placed",
                    7.0,
                ),
                (
                    "a datagram the transfer program sends",
                    Sender::Transfer,
                    "orders_shipped",
                    3962.0,
                ),
            ];
            for (case, sender, metric, value) in sent {
                let report = ran_in_guest(
                    &router,
                    &runtime,
                    case,
                    &first_node,
                    &[
                        "set -e".to_string(),
                        sender.line(
                            configurations[0].statsd_port,
                            &format!("{metric}:{value}|c"),
                        ),
                        format!("echo \"CWAGENT-SENT {value}\""),
                    ],
                );
                assert_eq!(
                    report["Status"].as_str(),
                    Some(SUCCESS_STATUS),
                    "case {case:?}: the send script must succeed, got {report}"
                );
                let published = when_published(
                    &router,
                    &runtime,
                    case,
                    &first_namespace,
                    metric,
                    &first_node,
                );
                assert!(
                    (published.sum - value).abs() < f64::EPSILON,
                    "case {case:?}: expected the value the listener received ({value}), got \
                     {published:?}"
                );
                // The same datagram reaches no listener of the OTHER machine: its agent's
                // listener is open on another port, and a datagram to a port nothing listens on
                // is dropped.
                let elsewhere = series(
                    &router,
                    &runtime,
                    case,
                    &second_namespace,
                    metric,
                    &second_node,
                );
                assert!(
                    elsewhere.is_empty(),
                    "case {case:?}: a datagram of one machine must not appear in another's \
                     namespace, got {elsewhere:?}"
                );
            }

            // Nothing is published while the agent is STOPPED: a value sent to a listener that
            // is not open is dropped, and the same value sent once the agent is started again
            // arrives. The second arrival is what proves the first was not merely late.
            let blocked = ran_in_guest(
                &router,
                &runtime,
                "a value sent while the agent is stopped",
                &first_node,
                &[
                    "set -e".to_string(),
                    format!("{AGENT_CONTROL} -a stop -m ec2"),
                    Sender::Redirection.line(configurations[0].statsd_port, "orders_dropped:11|c"),
                    format!("{AGENT_CONTROL} -a start -m ec2"),
                    Sender::Redirection.line(configurations[0].statsd_port, "orders_resumed:13|c"),
                ],
            );
            assert_eq!(
                blocked["Status"].as_str(),
                Some(SUCCESS_STATUS),
                "the stop/send/start/send script must succeed, got {blocked}"
            );
            let resumed = when_published(
                &router,
                &runtime,
                "a value sent once the agent is started again",
                &first_namespace,
                "orders_resumed",
                &first_node,
            );
            assert!(
                (resumed.sum - 13.0).abs() < f64::EPSILON,
                "expected the value sent to the started agent, got {resumed:?}"
            );
            let dropped = series(
                &router,
                &runtime,
                "a value sent while the agent is stopped",
                &first_namespace,
                "orders_dropped",
                &first_node,
            );
            assert!(
                dropped.is_empty(),
                "expected a value sent while the agent is stopped to be dropped, got {dropped:?}"
            );
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
