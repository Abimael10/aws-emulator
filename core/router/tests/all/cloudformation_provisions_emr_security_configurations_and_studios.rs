// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::EMR::SecurityConfiguration`, an
//! `AWS::EMR::Studio` or an `AWS::EMR::StudioSessionMapping` really creates them in Amazon
//! EMR.
//!
//! The rule under test: **a resource type CloudFormation publishes for a service is one a
//! template may declare, and a stack reaches a successful terminal state only once it has
//! created each such declared resource through the OWNING service's own create API — so the
//! resource is readable through that service's own read right after the stack completes,
//! carrying what the template declared and the identity the stack publishes for it; a create
//! that service refuses fails the resource with its refusal and leaves nothing behind, and
//! deleting the stack deletes the resource there.** Four crates have to agree for that to
//! hold (CloudFormation makes the calls, Amazon EMR serves them, and Amazon EC2 and IAM
//! answer for the network and the role a Studio names), so the suite lives here rather than
//! in any one of them.
//!
//! What a client does with such a stack is the shape of every case: it reads the stack's
//! `Outputs` and then drives Amazon EMR's own API by the names and ids it found. A
//! `CREATE_COMPLETE` stack whose configuration `DescribeSecurityConfiguration` answers
//! "does not exist" for, or whose Studio `DescribeStudio` resolves nothing for, is not a
//! state AWS can be in.
//!
//! The rows differ along what the rule generalises over: which of the three EMR types the
//! template declares, whether the template NAMES the resource or leaves the name to
//! CloudFormation, how the security-configuration document is written (an inline JSON
//! structure or the text of one) and which sections it configures, the Region the stack is
//! deployed in, whether the create is accepted or REFUSED by Amazon EMR (a malformed
//! document), and whether the resource is identified by its own name, by an identifier that
//! service mints, or by the triple that addresses a session mapping.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, opt_element, outputs_of};

/// How long a case waits for a stack operation to reach a terminal status before giving up:
/// the stack owes each of its resources a clock-driven span, so the status is not terminal on
/// the first read. DELIBERATELY far longer than any stack here takes (they settle in well
/// under a second on an idle machine): the case polls for the CONDITION and stops as soon as
/// it holds, so this bounds only a stack that never settles — and a host running the whole
/// workspace's tests at once is slow enough that a tight bound here would fail a stack that
/// was merely waiting for the machine.
const SETTLE_DEADLINE: Duration = Duration::from_secs(600);

/// How long a poll waits between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The identity type and the session policy every session mapping below is declared with:
/// `IdentityType` is one of the values `com.amazonaws.emr#IdentityType` publishes, and the
/// policy is an AWS managed policy ARN, which is what `SessionPolicyArn` takes.
const IDENTITY_TYPE: &str = "USER";
const SESSION_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonEMRFullAccessPolicy_v2";

/// The identity one Studio session mapping below maps.
const IDENTITY_NAME: &str = "studio-analyst";

/// The clause Amazon EMR refuses a `SecurityConfiguration` document that is not JSON with,
/// which the failed resource's status reason has to carry for the stack's failure to say what
/// went wrong.
const MALFORMED_DOCUMENT_CLAUSE: &str = "Invalid security configuration; Malformed Json string.";

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process call CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_emr_elasticmapreduce::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon EMR operation over the awsJson1_1 wire its model declares, signed for
/// `region`.
async fn emr(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("elasticmapreduce.{region}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .header("authorization", authorization("elasticmapreduce", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The wire error code of an awsJson failure body (`__type`, with any namespace prefix
/// dropped).
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message an awsJson failure body carries, whichever casing it uses.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Read the stack until it reports a status that is no longer in progress, and answer that
/// read's body. The condition is polled for, so a case takes only as long as the stack does.
async fn poll_to_terminal(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
) -> String {
    let started = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must answer 200, got {status} {described}"
        );
        let stack_status = opt_element(&described, "StackStatus").unwrap_or_default();
        if !stack_status.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the stack stayed {stack_status:?} rather than settling"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every output of the described stack as the key/value pairs a client reads it as.
fn outputs(case: &str, described: &str) -> BTreeMap<String, String> {
    let read = outputs_of(described);
    assert!(
        !read.is_empty(),
        "case {case:?}: expected the stack's outputs, got {described}"
    );
    read
}

/// The names `ListSecurityConfigurations` reports for the account.
async fn listed_configurations(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
) -> Vec<String> {
    let (status, answered) = emr(router, region, "ListSecurityConfigurations", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListSecurityConfigurations must answer 200, got {status} with {answered}"
    );
    answered["SecurityConfigurations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| entry["Name"].as_str().map(str::to_string))
        .collect()
}

/// The Studio ids `ListStudios` reports for the account.
async fn listed_studios(router: &MultiServiceRouter, region: &str, case: &str) -> Vec<String> {
    let (status, answered) = emr(router, region, "ListStudios", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListStudios must answer 200, got {status} with {answered}"
    );
    answered["Studios"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|studio| studio["StudioId"].as_str().map(str::to_string))
        .collect()
}

/// A template declaring ONE security configuration, written the way `document` states the
/// resource's own document — an inline JSON structure, or the text of one — with the `Name`
/// property only where `named` carries it.
fn security_configuration_template(named: Option<&str>, document: &str) -> String {
    let name = named
        .map(|name| format!("\"Name\": \"{name}\","))
        .unwrap_or_default();
    format!(
        r#"{{"AWSTemplateFormatVersion": "2010-09-09",
             "Resources": {{
               "SecConf": {{"Type": "AWS::EMR::SecurityConfiguration",
                 "Properties": {{{name}
                   "SecurityConfiguration": {document}}}}}}},
             "Outputs": {{
               "ConfigName": {{"Value": {{"Ref": "SecConf"}}}}}}}}"#
    )
}

/// The template the Studio case deploys: the network a Studio's Workspaces run in, the role it
/// assumes, the Studio itself and one identity mapped onto it, with the ids the stack
/// publishes read back as outputs.
const STUDIO_TEMPLATE: &str = r#"{
 "AWSTemplateFormatVersion": "2010-09-09",
 "Resources": {
  "StudioVpc": {
   "Type": "AWS::EC2::VPC",
   "Properties": { "CidrBlock": "10.91.0.0/16" }
  },
  "StudioSubnet": {
   "Type": "AWS::EC2::Subnet",
   "Properties": {
    "VpcId": { "Ref": "StudioVpc" },
    "CidrBlock": "10.91.1.0/24"
   }
  },
  "WorkspaceGroup": {
   "Type": "AWS::EC2::SecurityGroup",
   "Properties": {
    "GroupDescription": "the Studio Workspace group",
    "VpcId": { "Ref": "StudioVpc" }
   }
  },
  "EngineGroup": {
   "Type": "AWS::EC2::SecurityGroup",
   "Properties": {
    "GroupDescription": "the Studio Engine group",
    "VpcId": { "Ref": "StudioVpc" }
   }
  },
  "StudioRole": {
   "Type": "AWS::IAM::Role",
   "Properties": {
    "AssumeRolePolicyDocument": {
     "Version": "2012-10-17",
     "Statement": [{
      "Effect": "Allow",
      "Principal": { "Service": "elasticmapreduce.amazonaws.com" },
      "Action": "sts:AssumeRole"
     }]
    }
   }
  },
  "Studio": {
   "Type": "AWS::EMR::Studio",
   "Properties": {
    "Name": "cfn-emr-studio",
    "AuthMode": "IAM",
    "VpcId": { "Ref": "StudioVpc" },
    "SubnetIds": [{ "Ref": "StudioSubnet" }],
    "ServiceRole": { "Fn::GetAtt": ["StudioRole", "Arn"] },
    "WorkspaceSecurityGroupId": { "Fn::GetAtt": ["WorkspaceGroup", "GroupId"] },
    "EngineSecurityGroupId": { "Fn::GetAtt": ["EngineGroup", "GroupId"] },
    "DefaultS3Location": "s3://cfn-emr-studio-backup/studio/",
    "Description": "the Studio a stack deployed",
    "Tags": [{ "Key": "tier", "Value": "analytics" }]
   }
  },
  "Mapping": {
   "Type": "AWS::EMR::StudioSessionMapping",
   "Properties": {
    "StudioId": { "Ref": "Studio" },
    "IdentityType": "USER",
    "IdentityName": "studio-analyst",
    "SessionPolicyArn": "arn:aws:iam::aws:policy/AmazonEMRFullAccessPolicy_v2"
   }
  }
 },
 "Outputs": {
  "StudioRef": { "Value": { "Ref": "Studio" } },
  "StudioArn": { "Value": { "Fn::GetAtt": ["Studio", "Arn"] } },
  "MappingRef": { "Value": { "Ref": "Mapping" } },
  "VpcId": { "Value": { "Ref": "StudioVpc" } },
  "SubnetId": { "Value": { "Ref": "StudioSubnet" } }
 }
}"#;

/// One row of the security-configuration table: how the template states the resource, and the
/// document the stack must leave in Amazon EMR.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the configuration is created in.
    region: &'static str,
    stack_name: &'static str,
    /// The name the template states, or `None` where it states none and CloudFormation
    /// generates one.
    declared_name: Option<&'static str>,
    /// The `SecurityConfiguration` property as the template writes it.
    declared_document: &'static str,
    /// The document Amazon EMR must then hold, as the JSON it parses as — the same document
    /// however the template spelled it.
    expected_document: fn() -> Value,
}

fn rows() -> Vec<Row> {
    vec![
        // A named configuration whose document the template states as an inline structure:
        // encryption at rest with the Amazon S3 managed keys.
        Row {
            case: "a named configuration, its document stated inline",
            region: "us-west-2",
            stack_name: "emr-secconf-named",
            declared_name: Some("cfn-emr-secconf"),
            declared_document: r#"{"EncryptionConfiguration": {
                 "EnableAtRestEncryption": true,
                 "EnableInTransitEncryption": false,
                 "AtRestEncryptionConfiguration": {
                   "S3EncryptionConfiguration": {"EncryptionMode": "SSE-S3"}}}}"#,
            expected_document: || {
                json!({"EncryptionConfiguration": {
                    "EnableAtRestEncryption": true,
                    "EnableInTransitEncryption": false,
                    "AtRestEncryptionConfiguration": {
                        "S3EncryptionConfiguration": {"EncryptionMode": "SSE-S3"}}}})
            },
        },
        // The same type with the name left to CloudFormation, in another Region, configuring
        // another section of the document altogether.
        Row {
            case: "a configuration whose name CloudFormation generates, in another Region",
            region: "eu-west-1",
            stack_name: "emr-secconf-generated",
            declared_name: None,
            declared_document: r#"{"InstanceMetadataServiceConfiguration": {
                 "MinimumInstanceMetadataServiceVersion": 2,
                 "HttpPutResponseHopLimit": 1}}"#,
            expected_document: || {
                json!({"InstanceMetadataServiceConfiguration": {
                    "MinimumInstanceMetadataServiceVersion": 2,
                    "HttpPutResponseHopLimit": 1}})
            },
        },
        // A document the template states as TEXT rather than as a structure, which is the
        // other spelling the type's JSON property takes.
        Row {
            case: "a document the template states as text",
            region: "us-west-2",
            stack_name: "emr-secconf-text",
            declared_name: Some("cfn-emr-secconf-text"),
            declared_document: r#""{\"EncryptionConfiguration\":{\"EnableAtRestEncryption\":\
                true,\"AtRestEncryptionConfiguration\":{\"LocalDiskEncryptionConfiguration\":\
                {\"EncryptionKeyProviderType\":\"AwsKms\",\"AwsKmsKey\":\"alias/emr\"}}}}""#,
            expected_document: || {
                json!({"EncryptionConfiguration": {
                    "EnableAtRestEncryption": true,
                    "AtRestEncryptionConfiguration": {
                        "LocalDiskEncryptionConfiguration": {
                            "EncryptionKeyProviderType": "AwsKms",
                            "AwsKmsKey": "alias/emr"}}}})
            },
        },
    ]
}

/// Every row: the stack reaches `CREATE_COMPLETE`, its `Ref` output is the configuration's
/// name, and the very next read through Amazon EMR's own API answers that configuration with
/// the document the template declared and reports the name exactly once — then deleting the
/// stack removes it from that service.
#[tokio::test]
async fn a_stack_declaring_an_emr_security_configuration_creates_it_in_emr() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let body = security_configuration_template(row.declared_name, row.declared_document);
        let (status, answered) = cloudformation(
            &router,
            row.region,
            "CreateStack",
            &[("StackName", row.stack_name), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200, got {status} {answered}"
        );
        let stack_id = opt_element(&answered, "StackId").unwrap_or_default();
        assert!(
            stack_id.contains(":stack/"),
            "case {case:?}: CreateStack must answer the new stack's id, got {answered}"
        );

        let described = poll_to_terminal(&router, row.region, case, row.stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );
        let out = outputs(case, &described);
        let name = out
            .get("ConfigName")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a ConfigName output, got {out:?}"));
        match row.declared_name {
            Some(declared) => assert_eq!(
                name, declared,
                "case {case:?}: expected the Ref to be the declared name, got {name:?}"
            ),
            None => assert!(
                !name.is_empty(),
                "case {case:?}: expected the Ref to carry the generated name, got {name:?}"
            ),
        }

        // The configuration, read through the service that OWNS it, by the name the stack
        // published.
        let (status, answered) = emr(
            &router,
            row.region,
            "DescribeSecurityConfiguration",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a CREATE_COMPLETE stack's security configuration must be readable \
             in Amazon EMR, got {status} with {answered}"
        );
        assert_eq!(
            answered["Name"],
            json!(name),
            "case {case:?}: expected the configuration to carry the name the stack published, \
             got {answered}"
        );
        let held: Value = answered["SecurityConfiguration"]
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or(Value::Null);
        assert_eq!(
            held,
            (row.expected_document)(),
            "case {case:?}: expected the document the template declared, got {answered}"
        );

        // And the account's own listing reports that name exactly once.
        let listed = listed_configurations(&router, row.region, case).await;
        assert_eq!(
            listed.iter().filter(|held| *held == &name).count(),
            1,
            "case {case:?}: expected ListSecurityConfigurations to report {name:?} once, got \
             {listed:?}"
        );

        // DELETING THE STACK deletes it there: a configuration a stack no longer records must
        // not be left behind in the service that holds it.
        let (status, answered) = cloudformation(
            &router,
            row.region,
            "DeleteStack",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack must answer 200, got {status} {answered}"
        );
        let described = poll_to_terminal(&router, row.region, case, &stack_id).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("DELETE_COMPLETE"),
            "case {case:?}: expected the delete to complete, got {described}"
        );
        let (status, answered) = emr(
            &router,
            row.region,
            "DescribeSecurityConfiguration",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidRequestException"),
            "case {case:?}: expected the modelled client error for a configuration the stack \
             deleted, got {status} with {answered}"
        );
        assert_eq!(
            error_message(&answered),
            format!("Security configuration with name \"{name}\" does not exist."),
            "case {case:?}: expected Amazon EMR's own sentence for an absent configuration, \
             got {answered}"
        );
        let listed = listed_configurations(&router, row.region, case).await;
        assert!(
            !listed.contains(&name),
            "case {case:?}: expected {name:?} to be gone from the listing, got {listed:?}"
        );
    }
}

/// The negative leg of the same rule: a document Amazon EMR will not store fails the resource
/// with THAT service's refusal, the stack does not reach `CREATE_COMPLETE`, and nothing is
/// left in Amazon EMR under the name the template asked for.
#[tokio::test]
async fn a_malformed_security_configuration_document_fails_the_resource_and_stores_nothing() {
    let case = "a document that is not JSON";
    let region = "us-west-2";
    let stack_name = "emr-secconf-malformed";
    let name = "cfn-emr-secconf-malformed";
    let router = build_router();
    let body = security_configuration_template(Some(name), r#""{not json""#);
    let (status, answered) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200 — the template is well formed, got \
         {status} {answered}"
    );
    let stack_id = opt_element(&answered, "StackId").unwrap_or_default();

    let described = poll_to_terminal(&router, region, case, &stack_id).await;
    let reached = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_ne!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the stack NOT to complete, got {described}"
    );
    assert!(
        reached.starts_with("ROLLBACK") || reached.starts_with("CREATE_FAILED"),
        "case {case:?}: expected a failed create, got {reached:?} in {described}"
    );

    // The failure says what the owning service said: the resource's status reason carries
    // Amazon EMR's own refusal of the document.
    let (status, events) =
        cloudformation(&router, region, "DescribeStackEvents", &[("StackName", &stack_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackEvents must answer 200, got {status} {events}"
    );
    let reasons = elements(&events, "ResourceStatusReason");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains(MALFORMED_DOCUMENT_CLAUSE)),
        "case {case:?}: expected a resource status reason carrying {MALFORMED_DOCUMENT_CLAUSE:?}, \
         got {reasons:?}"
    );
    // And it carries that refusal the way the owning service's SDK raises it: the service, the
    // status it answered and the call's own id, then the attempts the SDK made. Only the fixed
    // text is asserted — the id and the count are the call's.
    assert!(
        reasons.iter().any(|reason| {
            reason.contains("(Service: Emr, Status Code: 400, Request ID: ")
                && reason.contains("(SDK Attempt Count: ")
        }),
        "case {case:?}: expected the reason to carry Amazon EMR's own SDK diagnostic, got \
         {reasons:?}"
    );

    // And nothing was stored: the name resolves to no configuration at all.
    let (status, answered) = emr(
        &router,
        region,
        "DescribeSecurityConfiguration",
        &json!({ "Name": name }),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "InvalidRequestException"),
        "case {case:?}: expected no configuration under the refused name, got {status} with \
         {answered}"
    );
    let listed = listed_configurations(&router, region, case).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected the failed stack to leave no configuration behind, got \
         {listed:?}"
    );
}

/// The same rule for the two Studio types: a template declaring them is ACCEPTED, the Studio
/// is created in Amazon EMR under the `es-` id that service mints — which is what `Ref`
/// resolves to — the identity the template maps onto it is a mapping that service reports, and
/// deleting the stack removes both.
#[tokio::test]
async fn a_stack_declaring_an_emr_studio_creates_it_and_its_session_mapping_in_emr() {
    let case = "a Studio with its network, its role and one session mapping";
    let region = "us-west-2";
    let stack_name = "emr-studio-estate";
    let router = build_router();
    let (status, answered) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", STUDIO_TEMPLATE),
            // The template declares an IAM role, which CloudFormation deploys only for a
            // request that acknowledges it ("Requires capabilities: [CAPABILITY_IAM]").
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200 for a template declaring published EMR \
         resource types, got {status} {answered}"
    );
    let stack_id = opt_element(&answered, "StackId").unwrap_or_default();
    assert!(
        stack_id.contains(":stack/"),
        "case {case:?}: CreateStack must answer the new stack's id, got {answered}"
    );

    let described = poll_to_terminal(&router, region, case, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );
    let out = outputs(case, &described);
    let studio_id = out
        .get("StudioRef")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a StudioRef output, got {out:?}"));
    assert!(
        studio_id.starts_with("es-") && studio_id.len() > "es-".len(),
        "case {case:?}: expected the Ref to be the es- Studio id Amazon EMR mints, got \
         {studio_id:?}"
    );

    // THE STUDIO, read through the service that owns it, by the id the stack published.
    let (status, answered) = emr(
        &router,
        region,
        "DescribeStudio",
        &json!({ "StudioId": studio_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a CREATE_COMPLETE stack's Studio must be readable in Amazon EMR, got \
         {status} with {answered}"
    );
    let studio = &answered["Studio"];
    assert_eq!(
        studio["Name"],
        json!("cfn-emr-studio"),
        "case {case:?}: expected the name the template declared, got {studio}"
    );
    assert_eq!(
        studio["AuthMode"],
        json!("IAM"),
        "case {case:?}: expected the authentication mode the template declared, got {studio}"
    );
    assert_eq!(
        studio["VpcId"],
        json!(out["VpcId"]),
        "case {case:?}: expected the Studio to run in the VPC the stack created, got {studio}"
    );
    assert_eq!(
        studio["SubnetIds"],
        json!([out["SubnetId"]]),
        "case {case:?}: expected the subnet the stack created, got {studio}"
    );
    assert_eq!(
        studio["DefaultS3Location"],
        json!("s3://cfn-emr-studio-backup/studio/"),
        "case {case:?}: expected the backup location the template declared, got {studio}"
    );
    assert!(
        studio["ServiceRole"]
            .as_str()
            .is_some_and(|role| role.contains(":role/")),
        "case {case:?}: expected the Studio to assume the stack's own role, got {studio}"
    );
    // The ARN the stack publishes is the one Amazon EMR built for the Studio: a handle a
    // client reads out of the outputs has to be one that service answers for.
    assert_eq!(
        studio["StudioArn"],
        json!(out
            .get("StudioArn")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a StudioArn output, got {out:?}"))),
        "case {case:?}: expected the stack's StudioArn to be the ARN Amazon EMR reports, got \
         {studio}"
    );
    let listed = listed_studios(&router, region, case).await;
    assert_eq!(
        listed.iter().filter(|held| *held == &studio_id).count(),
        1,
        "case {case:?}: expected ListStudios to report {studio_id:?} once, got {listed:?}"
    );

    // THE SESSION MAPPING: identified by the triple that service addresses one by, which is
    // also what the stack publishes as its physical id.
    assert_eq!(
        out.get("MappingRef").map(String::as_str),
        Some(format!("{studio_id}|{IDENTITY_TYPE}|{IDENTITY_NAME}").as_str()),
        "case {case:?}: expected the mapping's Ref to be the triple it is identified by, got \
         {out:?}"
    );
    let mapping_request = json!({
        "StudioId": studio_id,
        "IdentityType": IDENTITY_TYPE,
        "IdentityName": IDENTITY_NAME,
    });
    let (status, answered) = emr(
        &router,
        region,
        "GetStudioSessionMapping",
        &mapping_request,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the stack's session mapping must be readable in Amazon EMR, got \
         {status} with {answered}"
    );
    let mapping = &answered["SessionMapping"];
    assert_eq!(
        mapping["SessionPolicyArn"],
        json!(SESSION_POLICY_ARN),
        "case {case:?}: expected the session policy the template declared, got {mapping}"
    );
    assert_eq!(
        mapping["IdentityName"],
        json!(IDENTITY_NAME),
        "case {case:?}: expected the identity the template mapped, got {mapping}"
    );

    // DELETING THE STACK deletes both in Amazon EMR.
    let (status, answered) = cloudformation(
        &router,
        region,
        "DeleteStack",
        &[("StackName", stack_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteStack must answer 200, got {status} {answered}"
    );
    let described = poll_to_terminal(&router, region, case, &stack_id).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the delete to complete, got {described}"
    );
    for (operation, request) in [
        ("DescribeStudio", json!({ "StudioId": studio_id })),
        ("GetStudioSessionMapping", mapping_request),
    ] {
        let (status, answered) = emr(&router, region, operation, &request).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidRequestException"),
            "case {case:?}: expected {operation} to refuse what the stack deleted, got {status} \
             with {answered}"
        );
    }
    let listed = listed_studios(&router, region, case).await;
    assert!(
        !listed.contains(&studio_id),
        "case {case:?}: expected {studio_id:?} to be gone from the listing, got {listed:?}"
    );
}
