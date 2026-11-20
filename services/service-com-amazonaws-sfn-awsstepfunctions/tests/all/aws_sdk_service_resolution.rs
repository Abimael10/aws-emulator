// SPDX-License-Identifier: CC0-1.0

//! A `Task` state's AWS SDK integration with the services the AWS SDK family is the ONLY
//! way to name: AWS Account Management and Amazon API Gateway V2, beside two further
//! services and two further operations the same family reaches.
//!
//! The rule under test: `arn:aws:states:::aws-sdk:<sdk name>:<action>` names ONE API
//! operation of the service whose SDK id is that token, and the task performs it — the
//! document the state built IS that operation's request, wherever that service's protocol
//! puts each member, and the operation's own response document IS the state's result. It
//! holds for every service whose wire facts this emulator states, not only for the ones an
//! OPTIMIZED integration also names: a service with no optimized integration is reached by
//! the family's own token (`state::integration::AWS_SDK_APIS`), so a fan-out workflow over
//! several services gets each service's own answer rather than an interpreter failure.
//!
//! The recorded case this suite's issue was filed about — a four-branch `Parallel` whose
//! branches name `account:listRegions`, `apigatewayv2:getApis`,
//! `eventbridge:listApiDestinations` and `lambda:listCodeSigningConfigs` — is the first
//! test; the table below it spans the rest of the class, and the last test holds the
//! refusal a role that may not make such a call reaches the state as.
//!
//! Six services are mounted on ONE engine, the shape `core/router/src/lib.rs` mounts them
//! in, so every assertion is over what the owning service's own handlers really stored.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element};
use crate::state_machine_lifecycle::{string, ACCOUNT, REGION};
use crate::task_execution_role::{masked, refusal_metadata, trust_document, ACCESS_DENIED_CODE};

/// How long a test waits for an execution to leave `RUNNING`, and how long it waits
/// between two looks: generous, so a loaded host cannot decide the outcome.
const PATIENCE: Duration = Duration::from_secs(60);
/// See [`PATIENCE`].
const POLL: Duration = Duration::from_millis(25);

/// The four `Resource`s the recorded workflow's branches name.
const ACCOUNT_LIST_REGIONS: &str = "arn:aws:states:::aws-sdk:account:listRegions";
/// See [`ACCOUNT_LIST_REGIONS`].
const APIGATEWAYV2_GET_APIS: &str = "arn:aws:states:::aws-sdk:apigatewayv2:getApis";
/// See [`ACCOUNT_LIST_REGIONS`].
const EVENTBRIDGE_LIST_DESTINATIONS: &str =
    "arn:aws:states:::aws-sdk:eventbridge:listApiDestinations";
/// See [`ACCOUNT_LIST_REGIONS`].
const LAMBDA_LIST_CONFIGS: &str = "arn:aws:states:::aws-sdk:lambda:listCodeSigningConfigs";

/// The IAM actions the four calls above are taken under: the permission each invoked
/// operation publishes, which for each of these account-level reads names no resource type
/// at all — so a role that may make them carries each action over `*`.
const LIST_REGIONS_ACTION: &str = "account:ListRegions";
/// See [`LIST_REGIONS_ACTION`].
const LIST_DESTINATIONS_ACTION: &str = "events:ListApiDestinations";
/// See [`LIST_REGIONS_ACTION`].
const LIST_CONFIGS_ACTION: &str = "lambda:ListCodeSigningConfigs";
/// The permission API Gateway publishes for a read of its control plane, which is keyed on
/// the HTTP method rather than on the operation's name.
const APIGATEWAY_READ_ACTION: &str = "apigateway:GET";

/// Where `CreateApi` and `GetApis` are bound (`/v2/apis`), for the reads this suite makes
/// through Amazon API Gateway V2's own API.
const APIS_URI: &str = "/v2/apis";

/// Where `CreateCodeSigningConfig` and `ListCodeSigningConfigs` are bound, for the reads
/// this suite makes through AWS Lambda's own API.
const CODE_SIGNING_CONFIGS_URI: &str = "/2020-04-22/code-signing-configs";

/// A signing profile version ARN satisfying Lambda's modelled `Arn` pattern, in the form
/// AWS Signer vends one.
fn signing_profile(name: &str) -> String {
    format!("arn:aws:signer:{REGION}:{ACCOUNT}:/signing-profiles/{name}/0000000a")
}

/// Step Functions, IAM and the four services a branch calls, over ONE engine.
struct Chain {
    router: MultiServiceRouter,
}

impl Chain {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_account_account::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    async fn dispatch(&self, request: http::Request<Bytes>) -> (u16, Value) {
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "expected a JSON body, got {error} for {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// One `awsJson` RPC call against the service whose signing name is `signing_name`.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        self.dispatch(request).await
    }

    /// Call one Step Functions operation, requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .rpc("states", "AWSStepFunctions", "1.0", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected 200, got {status} with body {parsed}"
        );
        parsed
    }

    /// Call one EventBridge operation, requiring a 200.
    async fn events(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.rpc("events", "AWSEvents", "1.1", operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected 200, got {status} with body {parsed}"
        );
        parsed
    }

    /// One `restJson1` call to the service whose signing name is `signing_name`, as a
    /// client of that service addresses it: the method and path its operation binds, and
    /// the document body it carries.
    async fn rest(
        &self,
        signing_name: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> (u16, Value) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
        );
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", &host)
            .header("authorization", authorization);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let payload = body.map_or_else(Bytes::new, |document| {
            Bytes::from(serde_json::to_vec(document).expect("the request body serializes"))
        });
        let request = builder.body(payload).expect("a well-formed request");
        self.dispatch(request).await
    }

    /// The same, requiring a 2xx.
    async fn rest_ok(
        &self,
        signing_name: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Value {
        let (status, parsed) = self.rest(signing_name, method, path, body).await;
        assert!(
            (200..300).contains(&status),
            "case {method} {path}: expected a 2xx, got {status} with body {parsed}"
        );
        parsed
    }

    /// One awsQuery call to IAM, requiring a 200.
    async fn iam(&self, action: &str, params: &[(&str, String)]) -> String {
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
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {action:?}: expected 200 from IAM, got {status} with {text}"
        );
        text
    }

    /// An IAM role trusted by this service and carrying `policy` as its inline policy, by
    /// its own ARN.
    async fn role(&self, name: &str, policy: &Value) -> String {
        let created = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", name.to_string()),
                    ("AssumeRolePolicyDocument", trust_document().to_string()),
                ],
            )
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateRole answers the role Arn, got {created}"));
        self.iam(
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
        arn
    }

    /// Create a state machine under `role_arn` and return its ARN.
    async fn machine(&self, name: &str, definition: &str, role_arn: &str) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({ "name": name, "definition": definition, "roleArn": role_arn }),
            )
            .await;
        string(&created, "stateMachineArn")
    }

    /// Start one execution and describe it once it has left `RUNNING`, looking again until
    /// it has rather than waiting a fixed time for it.
    async fn ran(&self, machine_arn: &str, case: &str) -> Value {
        let started = self
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine_arn, "name": "once" }),
            )
            .await;
        let execution_arn = string(&started, "executionArn");
        let began = Instant::now();
        loop {
            let described = self
                .sfn(
                    "DescribeExecution",
                    &json!({ "executionArn": execution_arn }),
                )
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, got {described}"
            );
            ::tokio::time::sleep(POLL).await;
        }
    }

    /// Create a state machine whose one `Task` state calls `resource` with `parameters`,
    /// run one execution of it, and report what it ended as.
    async fn ran_task(
        &self,
        name: &str,
        resource: &str,
        parameters: &Value,
        role_arn: &str,
        case: &str,
    ) -> Value {
        let definition = json!({
            "StartAt": "Work",
            "States": {
                "Work": {
                    "Type": "Task",
                    "Resource": resource,
                    "Parameters": parameters,
                    "End": true
                }
            }
        })
        .to_string();
        let machine = self.machine(name, &definition, role_arn).await;
        self.ran(&machine, case).await
    }

    /// An API-key connection called `name`, created through EventBridge's own
    /// `CreateConnection`; answers its ARN.
    async fn given_connection(&self, name: &str) -> String {
        let created = self
            .events(
                "CreateConnection",
                &json!({
                    "Name": name,
                    "AuthorizationType": "API_KEY",
                    "AuthParameters": {
                        "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "sekrit" }
                    }
                }),
            )
            .await;
        string(&created, "ConnectionArn")
    }

    /// An api destination called `name`, created through EventBridge's own
    /// `CreateApiDestination`; answers its ARN.
    async fn given_api_destination(&self, name: &str, connection_arn: &str) -> String {
        let created = self
            .events(
                "CreateApiDestination",
                &json!({
                    "Name": name,
                    "ConnectionArn": connection_arn,
                    "InvocationEndpoint": "https://example.com/events",
                    "HttpMethod": "POST"
                }),
            )
            .await;
        string(&created, "ApiDestinationArn")
    }
}

/// The result document a SUCCEEDED execution reports.
fn result(described: &Value, case: &str) -> Value {
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected SUCCEEDED, got {described}"
    );
    described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described}"))
}

/// The entries of the list `member` carries in `reported`.
fn listed(reported: &Value, member: &str, case: &str) -> Vec<Value> {
    reported
        .get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the list {member:?} in {reported}"))
}

/// A policy granting every action the four calls are taken under, over `*` — the only
/// resource a grant of an action that names no resource type can be written for.
fn full_grant() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                LIST_REGIONS_ACTION,
                APIGATEWAY_READ_ACTION,
                LIST_DESTINATIONS_ACTION,
                LIST_CONFIGS_ACTION
            ],
            "Resource": "*"
        }]
    })
}

/// A policy granting every action of the services this suite calls, for the rows whose
/// subject is the CALL rather than the role's own documents.
fn every_action() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["account:*", "apigateway:*", "events:*", "lambda:*"],
            "Resource": "*"
        }]
    })
}

// ---------------------------------------------------------------------------
// The recorded case: one Parallel state, four services
// ---------------------------------------------------------------------------

/// A four-branch `Parallel` whose branches each name a DIFFERENT service through the AWS
/// SDK integration family succeeds, and each branch's result is that service's own
/// response document.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parallel_state_calls_every_aws_sdk_service_its_branches_name() {
    let chain = Chain::new();
    // The records each listing must report: an api of API Gateway V2's own, an api
    // destination of EventBridge's own, and a code signing configuration of Lambda's own,
    // each written through that service's own API.
    let api = chain
        .rest_ok(
            "apigateway",
            "POST",
            APIS_URI,
            Some(&json!({ "name": "fan-out-api", "protocolType": "HTTP" })),
        )
        .await;
    let api_id = string(&api, "apiId");
    let connection = chain.given_connection("fan-out-connection").await;
    chain
        .given_api_destination("fan-out-destination", &connection)
        .await;
    let config = chain
        .rest_ok(
            "lambda",
            "POST",
            CODE_SIGNING_CONFIGS_URI,
            Some(&json!({
                "AllowedPublishers": { "SigningProfileVersionArns": [signing_profile("Fanout")] },
                "Description": "the configuration the fan-out branch lists"
            })),
        )
        .await;
    let config_arn = string(
        config
            .get("CodeSigningConfig")
            .unwrap_or_else(|| panic!("CreateCodeSigningConfig answers a config, got {config}")),
        "CodeSigningConfigArn",
    );

    let branch = |name: &str, resource: &str| {
        json!({
            "StartAt": name,
            "States": {
                name: {
                    "Type": "Task",
                    "Resource": resource,
                    "Parameters": {},
                    "End": true
                }
            }
        })
    };
    let definition = json!({
        "StartAt": "FanOut",
        "States": {
            "FanOut": {
                "Type": "Parallel",
                "Branches": [
                    branch("Account", ACCOUNT_LIST_REGIONS),
                    branch("ApiGateway", APIGATEWAYV2_GET_APIS),
                    branch("EventBridge", EVENTBRIDGE_LIST_DESTINATIONS),
                    branch("Lambda", LAMBDA_LIST_CONFIGS)
                ],
                "End": true
            }
        }
    })
    .to_string();
    let case = "the four-branch fan-out under a role granting each action";
    let role_arn = chain.role("FanOutRole", &full_grant()).await;
    let machine = chain.machine("fan-out", &definition, &role_arn).await;
    let described = chain.ran(&machine, case).await;
    let reported = result(&described, case);
    let branches = reported
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected one result per branch, got {reported}"));
    assert_eq!(
        branches.len(),
        4,
        "case {case:?}: expected four branch results, got {reported}"
    );

    // The account branch: every Region of the commercial partition, each naming itself.
    let regions = listed(&branches[0], "Regions", case);
    assert!(
        !regions.is_empty(),
        "case {case:?}: expected a non-empty Regions list, got {}",
        branches[0]
    );
    for region in &regions {
        assert!(
            region.get("RegionName").and_then(Value::as_str).is_some(),
            "case {case:?}: expected every entry to carry RegionName, got {region}"
        );
    }

    // The API Gateway V2 branch: the apis of the account, under the member that model
    // spells `Items` whatever its wire name is. The entries BELOW that member carry the
    // invoked model's own wire spelling, which is this crate's documented divergence —
    // only the top level of a result is re-spelled, because below it a response carries a
    // caller's data beside the operation's members — so the api is looked for under both
    // spellings of its id rather than under one this suite would be pinning.
    let items = listed(&branches[1], "Items", case);
    let ids: Vec<String> = items
        .iter()
        .filter_map(|item| {
            ["ApiId", "apiId"]
                .iter()
                .find_map(|member| item.get(*member))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .collect();
    assert!(
        ids.contains(&api_id),
        "case {case:?}: expected the api {api_id:?} among {ids:?}"
    );

    // The EventBridge branch: the api destinations of the account.
    let destinations = listed(&branches[2], "ApiDestinations", case);
    let names: Vec<String> = destinations
        .iter()
        .filter_map(|destination| destination.get("Name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert_eq!(
        names,
        vec!["fan-out-destination".to_string()],
        "case {case:?}: expected the destination EventBridge holds, got {}",
        branches[2]
    );

    // The Lambda branch: the code signing configurations of the account.
    let configs = listed(&branches[3], "CodeSigningConfigs", case);
    let arns: Vec<String> = configs
        .iter()
        .filter_map(|config| config.get("CodeSigningConfigArn").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert_eq!(
        arns,
        vec![config_arn.clone()],
        "case {case:?}: expected the configuration Lambda holds, got {}",
        branches[3]
    );

    // The AWS SDK family reports the operation's own members alone: the SDK response
    // envelope belongs to the optimized family.
    for (index, branch) in branches.iter().enumerate() {
        assert!(
            branch.get("SdkHttpMetadata").is_none(),
            "case {case:?}: branch {index} must carry no SDK envelope, got {branch}"
        );
    }
}

// ---------------------------------------------------------------------------
// The rule beyond the recorded four: the operation the action names
// ---------------------------------------------------------------------------

/// One row: the `Resource` a `Task` names, and the members of the invoked operation's own
/// output its result must carry.
struct Row {
    case: &'static str,
    /// The state machine's name, which has to be unique in the account.
    machine: &'static str,
    resource: &'static str,
    parameters: Value,
    reports: &'static [&'static str],
}

/// A `Task` state's AWS SDK integration performs the operation its action names, whichever
/// service the token names and whatever that service's protocol does with each member.
///
/// The rows differ along what the rule generalises over: the SERVICE (four of them, two
/// reached by the AWS SDK family alone), the ACTION — none of which this emulator names in
/// code — whether it READS or WRITES, the protocol the request goes on the wire under (a
/// REST-JSON document body, a REST-JSON query, a REST-JSON path label, an AWS JSON body),
/// and the spelling the invoked model uses for its own members (PascalCase, and a model
/// that carries every member lower-camelled on the wire).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aws_sdk_task_calls_the_operation_of_the_service_its_token_names() {
    let chain = Chain::new();
    let connection = chain.given_connection("listed-connection").await;
    chain
        .given_api_destination("listed-destination", &connection)
        .await;
    let role_arn = chain.role("EveryActionRole", &every_action()).await;

    let rows = vec![
        // The recorded four, each the whole work of one state machine.
        Row {
            case: "the account's Regions, a POST whose body is the operation's input",
            machine: "lists-regions",
            resource: ACCOUNT_LIST_REGIONS,
            parameters: json!({}),
            reports: &["Regions"],
        },
        Row {
            case: "the account's apis, a GET whose paging is its query",
            machine: "lists-apis",
            resource: APIGATEWAYV2_GET_APIS,
            parameters: json!({ "MaxResults": "10" }),
            reports: &["Items"],
        },
        Row {
            case: "the account's api destinations, an AWS JSON body",
            machine: "lists-destinations",
            resource: EVENTBRIDGE_LIST_DESTINATIONS,
            parameters: json!({ "NamePrefix": "listed-" }),
            reports: &["ApiDestinations"],
        },
        Row {
            case: "the account's code signing configurations, a GET with a query bound",
            machine: "lists-configs",
            resource: LAMBDA_LIST_CONFIGS,
            parameters: json!({ "MaxItems": 5 }),
            reports: &["CodeSigningConfigs"],
        },
        // Other operations of the same services, which no code here names either.
        Row {
            case: "one Region's opt-in status, a read whose input names the Region",
            machine: "reads-a-region",
            resource: "arn:aws:states:::aws-sdk:account:getRegionOptStatus",
            parameters: json!({ "RegionName": "ap-east-1" }),
            reports: &["RegionName", "RegionOptStatus"],
        },
        Row {
            case: "the account's own information",
            machine: "reads-the-account",
            resource: "arn:aws:states:::aws-sdk:account:getAccountInformation",
            parameters: json!({}),
            reports: &["AccountId"],
        },
        Row {
            case: "an api the task CREATES, a write whose members are lower-camelled",
            machine: "creates-an-api",
            resource: "arn:aws:states:::aws-sdk:apigatewayv2:createApi",
            parameters: json!({ "Name": "task-made-api", "ProtocolType": "HTTP" }),
            reports: &["ApiId", "Name", "ProtocolType"],
        },
        Row {
            case: "a code signing configuration the task CREATES",
            machine: "creates-a-config",
            resource: "arn:aws:states:::aws-sdk:lambda:createCodeSigningConfig",
            parameters: json!({
                "AllowedPublishers": { "SigningProfileVersionArns": [signing_profile("Task")] },
                "Description": "written by a task"
            }),
            reports: &["CodeSigningConfig"],
        },
    ];

    for row in rows {
        let Row {
            case,
            machine,
            resource,
            parameters,
            reports,
        } = row;
        let described = chain
            .ran_task(machine, resource, &parameters, &role_arn, case)
            .await;
        let reported = result(&described, case);
        for member in reports {
            assert!(
                reported.get(*member).is_some(),
                "case {case:?}: expected the result to carry {member:?}, got {reported}"
            );
        }
    }

    // The far end of the two writes: each is the owning service's own record, readable
    // through that service's own API.
    let apis = chain.rest_ok("apigateway", "GET", APIS_URI, None).await;
    let names: Vec<String> = apis
        .get("items")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected items in {apis}"))
        .iter()
        .filter_map(|item| item.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert!(
        names.contains(&"task-made-api".to_string()),
        "case \"the api a task created\": expected it among {names:?}"
    );
    let configs = chain
        .rest_ok("lambda", "GET", CODE_SIGNING_CONFIGS_URI, None)
        .await;
    let descriptions: Vec<String> = configs
        .get("CodeSigningConfigs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected CodeSigningConfigs in {configs}"))
        .iter()
        .filter_map(|config| config.get("Description").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert!(
        descriptions.contains(&"written by a task".to_string()),
        "case \"the configuration a task created\": expected it among {descriptions:?}"
    );
}

// ---------------------------------------------------------------------------
// The call the execution role may not make
// ---------------------------------------------------------------------------

/// One row of the authorization table: the call, the action it is taken under, and the
/// refusal the invoked service answers it with.
struct Refusal {
    case: &'static str,
    machine: &'static str,
    resource: &'static str,
    /// The IAM action the call is taken under, which the role's policy below withholds.
    action: &'static str,
    /// The error name the task reports, which is the invoked service as the AWS SDK names
    /// it and the class that SDK resolves the answer to.
    error: &'static str,
    /// The name that SDK's own client gives the service in the metadata of the refusal.
    service: &'static str,
    /// The status that service's front door answers an unauthorized call with.
    status: u16,
}

/// A `Task` state whose AWS SDK integration call the execution role may not make fails
/// with the INVOKED service's own access-denied refusal, over the resource that action is
/// taken on — and the same call under a role that grants the action is made.
///
/// The rows differ in the service that refused, whether the invoked operation MODELS an
/// access error of its own (so the class is that error's) or models none (so it is the
/// service's base SDK exception), and the status that service's front door answers with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_the_execution_role_may_not_make_fails_with_the_services_own_refusal() {
    let chain = Chain::new();
    let rows = vec![
        Refusal {
            case: "the account listing, whose operation models its own access error",
            machine: "denied-regions",
            resource: ACCOUNT_LIST_REGIONS,
            action: LIST_REGIONS_ACTION,
            error: "Account.AccessDeniedException",
            service: "Account",
            status: 403,
        },
        Refusal {
            case: "the EventBridge listing, whose operation models no access error",
            machine: "denied-destinations",
            resource: EVENTBRIDGE_LIST_DESTINATIONS,
            action: LIST_DESTINATIONS_ACTION,
            error: "EventBridge.AmazonCloudWatchEventsException",
            service: "EventBridge",
            status: 400,
        },
        Refusal {
            case: "the Lambda listing, whose front door answers a Forbidden",
            machine: "denied-configs",
            resource: LAMBDA_LIST_CONFIGS,
            action: LIST_CONFIGS_ACTION,
            error: "Lambda.AWSLambdaException",
            service: "Lambda",
            status: 403,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let Refusal {
            case,
            machine,
            resource,
            action,
            error,
            service,
            status,
        } = row;
        // A role granting every action of these services EXCEPT the one this row's call is
        // taken under, so the only thing the row varies is that grant.
        let granted: Vec<&str> = [
            LIST_REGIONS_ACTION,
            LIST_DESTINATIONS_ACTION,
            LIST_CONFIGS_ACTION,
        ]
        .into_iter()
        .filter(|granted| *granted != action)
        .collect();
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": granted,
                "Resource": "*"
            }]
        });
        let role_arn = chain
            .role(&format!("LeastPrivilegeRole{index}"), &policy)
            .await;
        let described = chain
            .ran_task(machine, resource, &json!({}), &role_arn, case)
            .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the call the role may not make to fail, got {described}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(error),
            "case {case:?}: expected error {error:?}, got {described}"
        );
        let cause = string(&described, "cause");
        // The action is the one the invoked operation publishes, and the resource is every
        // resource: these actions name no resource type at all.
        assert!(
            cause.contains(&format!("to perform: {action} on resource: *")),
            "case {case:?}: expected the cause to name {action:?} over every resource, got \
             {cause:?}"
        );
        // The principal is the SESSION of the execution role the call was made as, whose
        // ARN names the calling account.
        let principal = crate::task_refusal_session_principal::session_principal(&role_arn, &cause);
        assert!(
            cause.contains(&principal),
            "case {case:?}: expected the cause to blame {principal}, got {cause:?}"
        );
        assert!(
            cause.contains(ACCOUNT),
            "case {case:?}: expected the cause to name the calling account {ACCOUNT}, got \
             {cause:?}"
        );
        // And the metadata the AWS SDK's own client closes the exception with, naming the
        // service that refused and the status its answer carried.
        let expected = refusal_metadata(resource, service, ACCESS_DENIED_CODE, status);
        let reported = masked(&cause);
        assert!(
            reported.ends_with(&expected),
            "case {case:?}: expected the cause to close {expected:?}, got {reported:?}"
        );
    }

    // The positive control: the same call under a role that grants the action is made.
    let role_arn = chain.role("GrantedRole", &full_grant()).await;
    let case = "the same listing under a role that grants it";
    let described = chain
        .ran_task("granted-regions", ACCOUNT_LIST_REGIONS, &json!({}), &role_arn, case)
        .await;
    let reported = result(&described, case);
    assert!(
        !listed(&reported, "Regions", case).is_empty(),
        "case {case:?}: expected the Regions the listing answered, got {reported}"
    );
}
