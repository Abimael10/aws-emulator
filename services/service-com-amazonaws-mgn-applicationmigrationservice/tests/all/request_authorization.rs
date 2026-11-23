// SPDX-License-Identifier: CC0-1.0

//! WHO may reach the operations of this service, driven end to end over the real wire path
//! (a restJson1 request through the multi-service router into the mounted service), so what
//! is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every operation whose model declares `AccessDeniedException`
//! evaluates the identity-based IAM policies in force on the principal its credentials name
//! against `mgn:<OperationName>` on the resource the request addresses BEFORE it looks at
//! this service's own state — so a request no statement allows (IAM's default deny), or one
//! an explicit `Deny` covers, is answered HTTP 403 `AccessDeniedException` with IAM's
//! implicit-deny sentence, changes nothing and reports nothing, while a request a matching
//! `Allow` covers reaches the operation's own answer. Because the decision comes first, an
//! unauthorized identity is told it is unauthorized whatever the account's business state
//! says — uninitialized, at its template quota, or with its place free.
//!
//! The user, the role, the access key and the policy documents are made through IAM's OWN
//! awsQuery operations on the SAME shared engine (`iam:CreateUser`, `iam:CreateAccessKey`,
//! `iam:PutUserPolicy`, `iam:CreateRole`, `iam:PutRolePolicy`), so the documents under test
//! are the ones a client wrote and the principal a refusal blames is the one IAM minted the
//! signing key for. Only the assumed-role SESSION is seeded into the STS store directly:
//! that is the one precondition no call of this suite can reach.
//!
//! The tables span what the rule generalises over: the operation (every one of this
//! service's thirteen that models the refusal), the resource form its decision is taken
//! against (an ARN the request carried, an ARN built from an id the request carried, the
//! resource-type wildcard a create is judged against, and no resource at all), the document
//! in force (none, another action of this service, another service's actions, this action
//! scoped to another resource, an explicit deny beside an allow, an allow of exactly the
//! action, of the action in another case, of the whole service, of everything, and one
//! scoped to the resource addressed), the principal form (an IAM user's long-term key, an
//! assumed-role session and the account's own published credential) and the account's
//! business state when the request arrives.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_mgn_applicationmigrationservice::state::service_state;
use service_com_amazonaws_mgn_applicationmigrationservice::{
    ComAmazonawsMgnApplicationmigrationserviceState as MgnState, EmulatedService,
};
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

use crate::account_initialization::{elements, form_encode};
use crate::application_lifecycle::{error_code, EMULATOR_ACCOUNT, REGION};
use crate::network_migration_mapping_start::{definition_request, uuid_shaped};
use crate::resource_tags::label;

/// IAM's awsQuery API version, which every fixture call names (`iam:Version`).
const IAM_VERSION: &str = "2010-05-08";

/// The access key id every PRIVILEGED call signs with: a key neither STS nor IAM holds a
/// record for, which is the position the account's own published credential is in.
const ACCOUNT_KEY_ID: &str = "AKIAEXAMPLEEXAMPLE";

/// The IAM user the unprivileged cases drive their calls as, and the inline policy name
/// their document is written under. One name is enough: every case runs against an engine of
/// its own.
const USER_NAME: &str = "migration-operator";
const POLICY_NAME: &str = "inline";

/// The role an assumed-role session is taken from, and that session's name.
const ROLE_NAME: &str = "migration-role";
const SESSION_NAME: &str = "migration-session";

/// The access key id the seeded session signs with. STS mints an `ASIA…` key for a session.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// How long the seeded session lasts: an hour, STS's own default, so it is live for the
/// whole of a case however the host is loaded.
const SESSION_SECONDS: i32 = 3_600;

/// The error shape every refusal under test names, and the status the model gives it
/// (`smithy_query(query="shape",shape="AccessDeniedException")`: `errorKind` client,
/// `httpError` 403).
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The IAM action namespace of this service, under which its actions are published
/// (`mgn:<OperationName>`); it is the service's `arnNamespace`
/// (`smithy_query(query="service_info")`).
const ACTION_PREFIX: &str = "mgn";

/// The `http.uri` of `CreateReplicationConfigurationTemplate`
/// (`smithy_query(query="operation",operation="CreateReplicationConfigurationTemplate")`).
const CREATE_TEMPLATE_PATH: &str = "/CreateReplicationConfigurationTemplate";

/// One answer the emulator gave: the status and the parsed body (restJson1 names the error
/// shape in the body's `__type`).
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// Whether this answer is the authorization refusal.
    fn refused(&self) -> bool {
        self.status == ACCESS_DENIED_STATUS && error_code(&self.body) == ACCESS_DENIED
    }

    /// The refusal's `message` member, or the empty string when it carries none.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }
}

/// This service mounted beside IAM — which holds the user, the role, the access key and the
/// policy documents — and STS, where the assembled emulator resolves a request's access key
/// id first, all over ONE shared engine.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing is signed here: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
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
        Self { router, engine }
    }

    /// This service's own store, read directly only to check that a refused call wrote
    /// nothing.
    fn store(&self) -> engine::ServiceState<MgnState> {
        service_state(&*self.engine).expect("this service has a store in this engine")
    }

    /// One call to this service at `method` and `path`, signed with `access_key_id` — what
    /// names the principal the request speaks for.
    async fn call_as(
        &self,
        access_key_id: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Answer {
        let payload = body
            .map(|body| serde_json::to_vec(body).expect("the request body serializes"))
            .unwrap_or_default();
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://mgn.{REGION}.amazonaws.com{path}"))
            .header("host", format!("mgn.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={access_key_id}/20260101/{REGION}/mgn/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let body = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        Answer { status, body }
    }

    /// `CreateReplicationConfigurationTemplate` as `access_key_id`, with the same body every
    /// time, so what differs between two calls of it is the identity and the account's state.
    async fn create_template(&self, access_key_id: &str) -> Answer {
        self.call_as(
            access_key_id,
            "POST",
            CREATE_TEMPLATE_PATH,
            Some(&template_request("subnet-0123456789abcdef")),
        )
        .await
    }

    /// One awsQuery call to IAM, answered with the HTTP status and the XML body. Signed with
    /// the account's own credential — the caller that may administer its own identities.
    async fn iam(&self, action: &str, params: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version={IAM_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = ::http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = String::from_utf8(response.body().to_vec()).expect("a UTF-8 response body");
        (status, body)
    }

    /// Create the IAM user and mint it a long-term access key, with NO policy attached — the
    /// position a freshly created IAM principal is in. Answers the ARN IAM holds the user
    /// under (what a refusal names) and the access key id to sign this service's calls with,
    /// both read out of IAM's own answers.
    async fn given_user_with_no_policy(&self, case: &str) -> (String, String) {
        let (status, created) = self.iam("CreateUser", &[("UserName", USER_NAME)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateUser must succeed, got {status}: {created}"
        );
        let user_arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateUser answers an Arn, got {created}"));
        let (status, key) = self.iam("CreateAccessKey", &[("UserName", USER_NAME)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateAccessKey must succeed, got {status}: {key}"
        );
        let access_key_id = xml_element(&key, "AccessKeyId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateAccessKey answers an id, got {key}"));
        (user_arn, access_key_id)
    }

    /// Write `document` on the user as its inline policy — what `iam:PutUserPolicy` does.
    async fn put_user_policy(&self, case: &str, document: &Value) {
        let (status, put) = self
            .iam(
                "PutUserPolicy",
                &[
                    ("UserName", USER_NAME),
                    ("PolicyName", POLICY_NAME),
                    ("PolicyDocument", &document.to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture PutUserPolicy must succeed, got {status}: {put}"
        );
    }

    /// Create the role an assumed-role session speaks for, and record the session that key
    /// was minted for. The ROLE is made through `iam:CreateRole`; the session is recorded in
    /// the STS store, which is what `sts:AssumeRole` writes. Answers the session's ARN —
    /// what a refusal names.
    async fn given_assumed_role_session(&self, case: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": "ec2.amazonaws.com"},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let (status, created) = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", ROLE_NAME),
                    ("AssumeRolePolicyDocument", &trust),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateRole must succeed, got {status}: {created}"
        );
        let role_arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateRole answers an Arn, got {created}"));
        let session_arn =
            format!("arn:aws:sts::{EMULATOR_ACCOUNT}:assumed-role/{ROLE_NAME}/{SESSION_NAME}");
        let vend = sts::NewSession {
            role_arn: Some(role_arn.clone()),
            ..sts::NewSession::new(
                sts::Identity {
                    user_id: sts::role_id(&role_arn),
                    account: EMULATOR_ACCOUNT.to_string(),
                    arn: session_arn.clone(),
                },
                sts::SessionKind::AssumedRole,
                SESSION_SECONDS,
            )
        };
        let store = sts::service_state(&*self.engine).expect("the STS store");
        store
            .lock()
            .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
            .expect("the session is recorded");
        session_arn
    }

    /// Write `document` on that role as its inline policy — what `iam:PutRolePolicy` does.
    async fn put_role_policy(&self, case: &str, document: &Value) {
        let (status, put) = self
            .iam(
                "PutRolePolicy",
                &[
                    ("RoleName", ROLE_NAME),
                    ("PolicyName", POLICY_NAME),
                    ("PolicyDocument", &document.to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture PutRolePolicy must succeed, got {status}: {put}"
        );
    }

    /// `InitializeService` as the account's own credential, so the account the cases act on
    /// is initialized.
    async fn initialize(&self) {
        let answer = self
            .call_as(
                ACCOUNT_KEY_ID,
                "POST",
                "/InitializeService",
                Some(&json!({})),
            )
            .await;
        assert_eq!(
            answer.status, 204,
            "the fixture InitializeService expected 204, got {answer:?}"
        );
    }

    /// `CreateApplication` as the account's own credential, answering the ARN the created
    /// application carries — a resource of this account the tag operations address by ARN.
    async fn create_application(&self, name: &str) -> String {
        let answer = self
            .call_as(
                ACCOUNT_KEY_ID,
                "POST",
                "/CreateApplication",
                Some(&json!({"name": name})),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "the fixture CreateApplication expected 201, got {answer:?}"
        );
        answer.body["arn"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateApplication answers an arn, got {answer:?}"))
            .to_string()
    }

    /// `CreateNetworkMigrationDefinition` as the account's own credential, answering the
    /// minted `networkMigrationDefinitionID`.
    async fn create_definition(&self, name: &str) -> String {
        let answer = self
            .call_as(
                ACCOUNT_KEY_ID,
                "POST",
                "/network-migration/CreateNetworkMigrationDefinition",
                Some(&definition_request(name)),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "the fixture CreateNetworkMigrationDefinition expected 201, got {answer:?}"
        );
        answer.body["networkMigrationDefinitionID"]
            .as_str()
            .unwrap_or_else(|| panic!("the create answers an id, got {answer:?}"))
            .to_string()
    }
}

/// One operation held to the rule: its name, the request that reaches it, and the resource
/// its decision is taken against (`None` for an action that addresses no resource of its
/// own, which IAM states no resource clause for).
struct Gated {
    name: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    resource: Option<String>,
}

/// A `CreateReplicationConfigurationTemplate` body carrying the members the model marks
/// `@required` (`smithy_query(query="operation",
/// operation="CreateReplicationConfigurationTemplate")`).
fn template_request(subnet: &str) -> Value {
    json!({
        "stagingAreaSubnetId": subnet,
        "associateDefaultSecurityGroup": true,
        "replicationServersSecurityGroupsIDs": [],
        "replicationServerInstanceType": "t3.small",
        "useDedicatedReplicationServer": false,
        "defaultLargeStagingDiskType": "GP3",
        "ebsEncryption": "DEFAULT",
        "bandwidthThrottling": 0,
        "dataPlaneRouting": "PRIVATE_IP",
        "createPublicIP": false,
        "stagingAreaTags": {},
    })
}

/// The ARN IAM judges a `CreateReplicationConfigurationTemplate` against: the create names
/// no template, so every template of the account it acts on. The form is the IAM Service
/// Authorization Reference's for this service's replication-configuration-template resource,
/// `arn:${Partition}:mgn:${Region}:${Account}:replication-configuration-template/${id}`.
fn template_scope() -> String {
    format!("arn:aws:mgn:{REGION}:{EMULATOR_ACCOUNT}:replication-configuration-template/*")
}

/// The ARN of one network migration definition, in that same reference's form for this
/// service's network-migration-definition resource.
fn definition_arn(definition_id: &str) -> String {
    format!("arn:aws:mgn:{REGION}:{EMULATOR_ACCOUNT}:network-migration-definition/{definition_id}")
}

/// Every operation of this service whose model declares `AccessDeniedException`
/// (`smithy_query(query="operation",operation=…)` → `errors`) and whose handler serves
/// requests, with a request each would otherwise act on.
///
/// `definition_id` names a definition the account holds and `tagged_arn` a resource of the
/// account the tag operations address; the execution id is one of the modelled shape, which
/// is all the decision needs, since it is taken before anything is looked up.
fn gated_operations(definition_id: &str, tagged_arn: &str) -> Vec<Gated> {
    let execution_id = uuid_shaped(7);
    let definition = definition_arn(definition_id);
    let network = |operation: &str| format!("/network-migration/{operation}");
    vec![
        Gated {
            name: "CreateReplicationConfigurationTemplate",
            method: "POST",
            path: CREATE_TEMPLATE_PATH.to_string(),
            body: Some(template_request("subnet-0123456789abcdef")),
            resource: Some(template_scope()),
        },
        Gated {
            name: "InitializeService",
            method: "POST",
            path: "/InitializeService".to_string(),
            body: Some(json!({})),
            resource: None,
        },
        Gated {
            name: "TagResource",
            method: "POST",
            path: format!("/tags/{}", label(tagged_arn)),
            body: Some(json!({"tags": {"env": "dev"}})),
            resource: Some(tagged_arn.to_string()),
        },
        Gated {
            name: "UntagResource",
            method: "DELETE",
            path: format!("/tags/{}?tagKeys=env", label(tagged_arn)),
            body: None,
            resource: Some(tagged_arn.to_string()),
        },
        Gated {
            name: "ListTagsForResource",
            method: "GET",
            path: format!("/tags/{}", label(tagged_arn)),
            body: None,
            resource: Some(tagged_arn.to_string()),
        },
        Gated {
            name: "GetNetworkMigrationDefinition",
            method: "POST",
            path: network("GetNetworkMigrationDefinition"),
            body: Some(json!({"networkMigrationDefinitionID": definition_id})),
            resource: Some(definition.clone()),
        },
        Gated {
            name: "UpdateNetworkMigrationDefinition",
            method: "POST",
            path: network("UpdateNetworkMigrationDefinition"),
            body: Some(json!({
                "networkMigrationDefinitionID": definition_id,
                "description": "a changed plan",
            })),
            resource: Some(definition.clone()),
        },
        Gated {
            name: "DeleteNetworkMigrationDefinition",
            method: "POST",
            path: network("DeleteNetworkMigrationDefinition"),
            body: Some(json!({"networkMigrationDefinitionID": definition_id})),
            resource: Some(definition.clone()),
        },
        Gated {
            name: "ListNetworkMigrationDefinitions",
            method: "POST",
            path: network("ListNetworkMigrationDefinitions"),
            body: Some(json!({})),
            resource: None,
        },
        Gated {
            name: "ListNetworkMigrationExecutions",
            method: "POST",
            path: network("ListNetworkMigrationExecutions"),
            body: Some(json!({"networkMigrationDefinitionID": definition_id})),
            resource: Some(definition.clone()),
        },
        Gated {
            name: "ListNetworkMigrationMappings",
            method: "POST",
            path: network("ListNetworkMigrationMappings"),
            body: Some(json!({
                "networkMigrationDefinitionID": definition_id,
                "networkMigrationExecutionID": execution_id,
            })),
            resource: Some(definition.clone()),
        },
        Gated {
            name: "StartNetworkMigrationMapping",
            method: "POST",
            path: network("StartNetworkMigrationMapping"),
            body: Some(json!({
                "networkMigrationDefinitionID": definition_id,
                "networkMigrationExecutionID": execution_id,
            })),
            resource: Some(definition.clone()),
        },
        Gated {
            name: "StartNetworkMigrationAnalysis",
            method: "POST",
            path: network("StartNetworkMigrationAnalysis"),
            body: Some(json!({
                "networkMigrationDefinitionID": definition_id,
                "networkMigrationExecutionID": execution_id,
            })),
            resource: Some(definition),
        },
    ]
}

/// Which of IAM's refusals a case expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The default deny of a request no statement allows.
    DefaultDeny,
    /// The explicit deny of a request a `Deny` statement covers.
    ExplicitDeny,
}

impl Refusal {
    /// The clause IAM closes the sentence with.
    fn reason(self, action: &str) -> String {
        match self {
            Self::DefaultDeny => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// IAM's deny sentence for this request, as the service must word it: the principal that
/// signed, the action it asked for, the resource it addressed (named only when the action
/// addresses one) and which rule refused it.
fn expected_message(
    principal: &str,
    operation: &str,
    resource: Option<&str>,
    refusal: Refusal,
) -> String {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let clause = resource
        .map(|resource| format!(" on resource: {resource}"))
        .unwrap_or_default();
    format!(
        "User: {principal} is not authorized to perform: {action}{clause} {}",
        refusal.reason(&action)
    )
}

/// Require `answer` to be the authorization refusal, worded IAM's way.
fn assert_refused(case: &str, answer: &Answer, expected: &str) {
    assert!(
        answer.refused(),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?}",
        answer.status,
        answer.body
    );
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?}: expected IAM's wording, got {:?}",
        answer.body
    );
}

/// Require `answer` to be something other than the authorization refusal: the request got
/// past the decision and met the operation's own answer, whatever that is.
fn assert_not_refused(case: &str, answer: &Answer) {
    assert!(
        !answer.refused(),
        "case {case:?}: the request must get past the authorization decision, got {} {:?}",
        answer.status,
        answer.body
    );
}

/// A one-statement identity-based policy document: `effect` on `action` over `resource`.
fn statement(effect: &str, action: Value, resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": action, "Resource": resource}],
    })
}

/// An allow of every action of this service, over every resource.
fn allow_this_service() -> Value {
    statement(
        "Allow",
        json!(format!("{ACTION_PREFIX}:*")),
        json!("*"),
    )
}

/// THE RULE, over every gated operation: an IAM user carrying no policy at all is refused
/// HTTP 403 `AccessDeniedException` naming it, the action and the resource the request
/// addressed, while the identical request from the account's own credential gets past the
/// decision. Each row runs against an engine of its own, so no row can pass on another's
/// write, and the privileged call is made both before and after the refused one, so neither
/// ordering is what decides.
#[tokio::test]
async fn every_gated_operation_refuses_an_identity_no_policy_allows() {
    for privileged_first in [false, true] {
        for row_index in 0..gated_operations("nmd-0000000000000000a", "arn:aws:mgn:::x").len() {
            let emulator = Emulator::new();
            emulator.initialize().await;
            let tagged = emulator.create_application("tagged-app").await;
            let definition = emulator.create_definition("authorization-plan").await;
            let row = gated_operations(&definition, &tagged)
                .into_iter()
                .nth(row_index)
                .expect("the row index is one of the gated operations");
            let (user_arn, user_key) = emulator
                .given_user_with_no_policy("an IAM user with no policy")
                .await;
            let case = format!(
                "{} as an IAM user with no policy{}",
                row.name,
                if privileged_first {
                    ", the privileged caller going first"
                } else {
                    ""
                }
            );

            if privileged_first {
                let served = emulator
                    .call_as(ACCOUNT_KEY_ID, row.method, &row.path, row.body.as_ref())
                    .await;
                assert_not_refused(&format!("{case} (the privileged caller)"), &served);
            }

            let answer = emulator
                .call_as(&user_key, row.method, &row.path, row.body.as_ref())
                .await;
            assert_refused(
                &case,
                &answer,
                &expected_message(
                    &user_arn,
                    row.name,
                    row.resource.as_deref(),
                    Refusal::DefaultDeny,
                ),
            );
            // A refused call reports no part of the operation's own answer.
            for member in ["items", "arn", "jobID", "tags"] {
                assert!(
                    answer.body.get(member).is_none(),
                    "case {case:?}: a refused call must report no {member}, got {:?}",
                    answer.body
                );
            }

            if !privileged_first {
                let served = emulator
                    .call_as(ACCOUNT_KEY_ID, row.method, &row.path, row.body.as_ref())
                    .await;
                assert_not_refused(&format!("{case} (the privileged caller)"), &served);
            }
        }
    }
}

/// THE RULE's other half: WHICH document decides. The table spans the documents in force on
/// the principal, over two operations whose decision is taken against different resource
/// forms — a create judged against the resource type it mints, and a tag write judged against
/// the ARN the request carried.
#[tokio::test]
async fn the_document_in_force_on_the_identity_decides() {
    for (operation, served_status) in [
        ("CreateReplicationConfigurationTemplate", 201u16),
        ("TagResource", 204u16),
    ] {
        let action = format!("{ACTION_PREFIX}:{operation}");
        // Another action of this service, which a row grants instead of the one under test.
        let other_action = format!("{ACTION_PREFIX}:DeleteReplicationConfigurationTemplate");
        for (row, document, refused) in documents_in_force(&action, &other_action) {
            let case = format!("{operation} with {row}");
            let emulator = Emulator::new();
            emulator.initialize().await;
            let tagged = emulator.create_application("tagged-app").await;
            let definition = emulator.create_definition("authorization-plan").await;
            let under_test = gated_operations(&definition, &tagged)
                .into_iter()
                .find(|candidate| candidate.name == operation)
                .expect("the operation under test is one of the gated ones");
            let (user_arn, user_key) = emulator.given_user_with_no_policy(&case).await;
            if let Some(document) = &document {
                // A row scoped to "the resource addressed" is scoped to the ARN THIS
                // operation's decision is taken against.
                let addressed = under_test.resource.clone().unwrap_or_default();
                emulator
                    .put_user_policy(&case, &substitute_addressed(document, &addressed))
                    .await;
            }

            let answer = emulator
                .call_as(
                    &user_key,
                    under_test.method,
                    &under_test.path,
                    under_test.body.as_ref(),
                )
                .await;
            match refused {
                Some(refusal) => assert_refused(
                    &case,
                    &answer,
                    &expected_message(
                        &user_arn,
                        operation,
                        under_test.resource.as_deref(),
                        refusal,
                    ),
                ),
                None => assert_eq!(
                    answer.status, served_status,
                    "case {case:?}: an allowed request must be served, got {} {:?}",
                    answer.status, answer.body
                ),
            }
        }
    }
}

/// The documents a row puts in force, and which of IAM's refusals the request then meets —
/// `None` for a request a matching `Allow` permits. [`ADDRESSED`] stands for the ARN the
/// request under test addresses, substituted per operation.
type DocumentRow = (&'static str, Option<Value>, Option<Refusal>);

fn documents_in_force(action: &str, other: &str) -> Vec<DocumentRow> {
    vec![
        ("no policy at all", None, Some(Refusal::DefaultDeny)),
        (
            "an allow of another action of this service, but not this one",
            Some(statement("Allow", json!(other), json!("*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of every action of another service",
            Some(statement("Allow", json!("s3:*"), json!("*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of this action scoped to another service's resources",
            Some(statement(
                "Allow",
                json!(action),
                json!(format!("arn:aws:s3:::{EMULATOR_ACCOUNT}-bucket/*")),
            )),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an explicit deny of this whole service beside an allow of exactly this action",
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    {"Effect": "Allow", "Action": action, "Resource": "*"},
                    {"Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*"},
                ],
            })),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "an allow of exactly this action",
            Some(statement("Allow", json!(action), json!("*"))),
            None,
        ),
        (
            // IAM compares action names case-insensitively, so the spelling of the allow is
            // not what decides the request.
            "an allow of this action spelled in lower case",
            Some(statement("Allow", json!(action.to_lowercase()), json!("*"))),
            None,
        ),
        (
            "an allow listing this action beside another service's",
            Some(statement(
                "Allow",
                json!(["s3:GetObject", action]),
                json!("*"),
            )),
            None,
        ),
        ("an allow of every action of this service", Some(allow_this_service()), None),
        (
            "an allow of every action",
            Some(statement("Allow", json!("*"), json!("*"))),
            None,
        ),
        (
            "an allow of this action scoped to the resource the request addresses",
            Some(statement("Allow", json!(action), json!(ADDRESSED))),
            None,
        ),
    ]
}

/// The placeholder [`documents_in_force`] writes where the resource the request addresses
/// goes, substituted once the operation under test is known.
const ADDRESSED: &str = "<the resource addressed>";

/// `document` with every [`ADDRESSED`] placeholder replaced by `resource`.
fn substitute_addressed(document: &Value, resource: &str) -> Value {
    let rendered = document.to_string().replace(ADDRESSED, resource);
    serde_json::from_str(&rendered).expect("the substituted document is still JSON")
}

/// THE ORDERING: the decision is taken before the account's business state is looked at, so
/// the same unauthorized identity is refused 403 whatever that state says — an uninitialized
/// account, an account with its template place free (where the privileged caller is served)
/// and an account at its quota — and it mints nothing. Once the action is granted, the
/// identical call collapses onto the privileged caller's own answer.
#[tokio::test]
async fn authorization_is_decided_ahead_of_the_account_state_and_mints_nothing() {
    let emulator = Emulator::new();
    let (user_arn, user_key) = emulator.given_user_with_no_policy("the ordering").await;
    let refusal = expected_message(
        &user_arn,
        "CreateReplicationConfigurationTemplate",
        Some(&template_scope()),
        Refusal::DefaultDeny,
    );

    // 1. The account is NOT initialized: the privileged caller is told so, while the
    //    unauthorized identity is told it is not authorized instead.
    let privileged = emulator.create_template(ACCOUNT_KEY_ID).await;
    assert_eq!(
        (privileged.status, error_code(&privileged.body)),
        (400, "UninitializedAccountException"),
        "the privileged caller must meet the account gate, got {privileged:?}"
    );
    assert_refused(
        "an uninitialized account",
        &emulator.create_template(&user_key).await,
        &refusal,
    );

    // 2. The account is initialized and its template place is FREE, so the privileged caller
    //    is served: the unauthorized identity is still refused, and mints nothing.
    emulator.initialize().await;
    assert_refused(
        "a free quota",
        &emulator.create_template(&user_key).await,
        &refusal,
    );
    let held = emulator
        .store()
        .lock()
        .replication_configuration_templates_of(EMULATOR_ACCOUNT)
        .len();
    assert_eq!(
        held, 0,
        "a refused create must store no template, got {held} in the account"
    );
    let served = emulator.create_template(ACCOUNT_KEY_ID).await;
    assert_eq!(
        served.status, 201,
        "the privileged caller must be served with the place free, got {served:?}"
    );
    let minted = served.body["replicationConfigurationTemplateID"]
        .as_str()
        .unwrap_or_else(|| panic!("the create answers an id, got {served:?}"))
        .to_string();

    // 3. The account is now AT its quota, which is what the privileged caller meets: the
    //    unauthorized identity is told it is not authorized rather than told the quota.
    let privileged = emulator.create_template(ACCOUNT_KEY_ID).await;
    assert_eq!(
        (privileged.status, error_code(&privileged.body)),
        (402, "ServiceQuotaExceededException"),
        "the privileged caller must meet the quota, got {privileged:?}"
    );
    assert_refused(
        "a met quota",
        &emulator.create_template(&user_key).await,
        &refusal,
    );

    // 4. Granted the action, the identical call collapses onto the privileged answer: the
    //    quota while the account holds its one template, and a success once it does not.
    emulator
        .put_user_policy("the grant", &allow_this_service())
        .await;
    let granted = emulator.create_template(&user_key).await;
    assert_eq!(
        (granted.status, error_code(&granted.body)),
        (402, "ServiceQuotaExceededException"),
        "a granted identity must meet the same quota the privileged caller does, got {granted:?}"
    );
    let deleted = emulator
        .call_as(
            ACCOUNT_KEY_ID,
            "POST",
            "/DeleteReplicationConfigurationTemplate",
            Some(&json!({"replicationConfigurationTemplateID": minted})),
        )
        .await;
    assert_eq!(
        deleted.status, 204,
        "the fixture delete expected 204, got {deleted:?}"
    );
    let granted = emulator.create_template(&user_key).await;
    assert_eq!(
        granted.status, 201,
        "a granted identity must be served once the place is free, got {granted:?}"
    );
}

/// The same rule under the other principal form: an assumed-role session is judged on the
/// ROLE's policies, and the refusal names the SESSION the credentials speak for.
#[tokio::test]
async fn an_assumed_role_session_is_judged_on_the_roles_policies() {
    // (case, the document on the role, the refusal it meets)
    let rows: Vec<DocumentRow> = vec![
        ("no policy at all", None, Some(Refusal::DefaultDeny)),
        (
            "an explicit deny of this service",
            Some(statement(
                "Deny",
                json!(format!("{ACTION_PREFIX}:*")),
                json!("*"),
            )),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "an allow of every action of this service",
            Some(allow_this_service()),
            None,
        ),
    ];
    for (row, document, refused) in rows {
        let case = format!("an assumed-role session with {row}");
        let emulator = Emulator::new();
        emulator.initialize().await;
        let session_arn = emulator.given_assumed_role_session(&case).await;
        if let Some(document) = &document {
            emulator.put_role_policy(&case, document).await;
        }

        let answer = emulator.create_template(SESSION_KEY_ID).await;
        match refused {
            Some(refusal) => assert_refused(
                &case,
                &answer,
                &expected_message(
                    &session_arn,
                    "CreateReplicationConfigurationTemplate",
                    Some(&template_scope()),
                    refusal,
                ),
            ),
            None => assert_eq!(
                answer.status, 201,
                "case {case:?}: an allowed session must be served, got {} {:?}",
                answer.status, answer.body
            ),
        }
    }
}

/// The text of the FIRST `<name>…</name>` element of an awsQuery response, read with the
/// sibling suite's reader of such an answer (`crate::account_initialization::elements`), so
/// this binary carries one reading of an awsQuery element and one form encoder
/// ([`form_encode`]). Every element read here is a leaf carrying an id or an ARN.
fn xml_element(body: &str, name: &str) -> Option<String> {
    elements(body, name).into_iter().next()
}
