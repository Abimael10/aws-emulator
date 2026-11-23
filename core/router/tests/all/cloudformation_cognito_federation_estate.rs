// SPDX-License-Identifier: CC0-1.0

//! The documented Amazon Cognito federation topology, deployed with CloudFormation and then
//! USED: a user pool, an app client of it, an identity pool federating that client, the IAM
//! role a federated identity assumes and the role attachment that binds the two.
//!
//! RULE under test: **a stack whose resources are all well-formed and whose intrinsic
//! references resolve reaches `CREATE_COMPLETE`, which it can only do when every provider in
//! the chain hands the next one the identifier the OWNING service minted** — a resource whose
//! identity its owning service assigns has that identifier as its physical resource id, so
//! `Ref`, `Fn::GetAtt` and `Fn::Sub` over it publish a handle that service's own calls take,
//! never the name the template declared it under. The two shapes are disjoint, which is what
//! makes the rule observable: a user pool's handle is `UserPoolIdType` (`@length 1..55`,
//! `@pattern ^[\w-]+_[0-9a-zA-Z]+$`) while its declared name is `UserPoolNameType`
//! (`@pattern ^[\w\s+=,.@-]+$`), and an identity pool's handle is `IdentityPoolId`
//! (`@length 1..55`, `@pattern ^[\w-]+:[0-9a-f-]+$`, "an identity pool ID in the format
//! REGION:GUID") while its declared name is `IdentityPoolName`
//! (`smithy_query(query="shape",shape="UserPoolIdType,UserPoolNameType,IdentityPoolId")`).
//! So a stack that published a name would be refused by the very next provider in the chain
//! and roll back, and one that publishes the minted ids completes and leaves an estate a
//! client can drive.
//!
//! That second half is what each row then does, end to end on the deployed estate:
//! `AdminCreateUser` / `AdminSetUserPassword` / `AdminInitiateAuth` against the published pool
//! and client, `GetId` and `GetCredentialsForIdentity` against the published identity pool
//! with the pool's own token as the login, and `sts:GetCallerIdentity` with the credentials
//! that came back — which must resolve to a session of the role the stack attached.
//!
//! The rows differ along what the rule generalises over: the template language (YAML
//! short-form intrinsics, JSON), the intrinsic the downstream property is written with
//! (`Ref`, `Fn::GetAtt`, `Fn::Sub`), whether the template NAMES each resource or leaves
//! CloudFormation to name it, the form of the role mapping the attachment carries (a `Token`
//! mapping keyed by provider-and-client, a `Rules` mapping keyed by provider alone, no mapping
//! at all) and the Region deployed in.
//!
//! Five crates have to agree for the rule to hold — CloudFormation makes the create calls,
//! Amazon Cognito user pools serves the pool and the client, Amazon Cognito Identity the
//! identity pool and its roles, IAM holds the role and STS resolves the session it vends — so
//! the suite lives here rather than in any one of them. Every expected value is computed from
//! what the row declares and from the ids the owning services minted in the same run; nothing
//! is a literal from a recording.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{element, elements, form_encode, opt_element, outputs_of};
// The predicate for an identifier Amazon Cognito minted for a user pool, and the accessor for
// one output of a stack, both stated once by the sibling suite that first needed them.
use super::cloudformation_provisions_cognito_user_pools::{is_minted_pool_id, output};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
///
/// Visible to the other suites of this target, so a sibling that assembles the same router
/// sizes its thread by the same constant rather than by a second copy of it.
pub(crate) const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// How long a row goes on polling for a stack status that is already due, and how often: the
/// transition is owed on the stack's own clock, so the wait is for the CONDITION, with a
/// deadline generous enough for a loaded host.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// The API versions the awsQuery services this suite calls declare.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";
const STS_API_VERSION: &str = "2011-06-15";

/// The access key id every signed call carries. The signature is never checked — this router
/// runs with request authentication off — so what the credential scope carries is the key id
/// and the Region each handler reads.
///
/// Visible to the other suites of this target for the reason [`Wire`] is: a sibling signing a
/// setup call through this harness signs it with the same key id rather than with a second
/// spelling of it.
pub(crate) const SETUP_KEY_ID: &str = "AKIDTEST";

/// The terminal `StackStatus` a create every resource of which was created rests in — the one
/// a create whose resource failed never reaches, the default `OnFailure=ROLLBACK` taking it to
/// `ROLLBACK_COMPLETE` instead.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The terminal `StackStatus` an in-place update that applied every change it declared rests
/// in, which is what [`Wire::update_stack`] holds an update to.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The `RoleSessionName` every session an identity pool vends carries, as AWS publishes it in
/// such a session's caller ARN
/// (`arn:aws:sts::<account>:assumed-role/<role>/CognitoIdentityCredentials`).
const VENDED_ROLE_SESSION_NAME: &str = "CognitoIdentityCredentials";

/// The service shapes the two Amazon Cognito models bind their `X-Amz-Target` to
/// (`smithy_query(query="service_info")`), and their signing names. Visible to the other
/// suites of this target for the reason [`Wire`] is.
pub(crate) const COGNITO_IDP: (&str, &str) = ("cognito-idp", "AWSCognitoIdentityProviderService");
/// See [`COGNITO_IDP`].
pub(crate) const COGNITO_IDENTITY: (&str, &str) = ("cognito-identity", "AWSCognitoIdentityService");

/// The emulator's own address, for the unsigned legs: `GetId` and `GetCredentialsForIdentity`
/// list no auth scheme, so a client sends them with no credential scope and the service name
/// travels in `X-Amz-Target` alone.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// What one call answered.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: bytes::Bytes,
}

impl Answer {
    pub(crate) fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// The assembled emulator and the runtime its requests are driven on, carried together so
/// each leg below names only what it is ABOUT.
///
/// Visible to the other suites of this target: a sibling driving the same two Amazon Cognito
/// services and CloudFormation over one assembled router calls THIS harness rather than
/// writing a second encoder, a second signer and a second settle loop, which would drift from
/// these.
pub(crate) struct Wire<'a> {
    pub(crate) router: &'a MultiServiceRouter,
    pub(crate) runtime: &'a tokio::runtime::Runtime,
}

impl Wire<'_> {
    /// Dispatch one request, signed for a signing name in `region` with an access key id, or
    /// unsigned when `signed` is `None` — which is how an SDK sends an operation whose model
    /// lists no auth scheme.
    pub(crate) fn dispatch(
        &self,
        signed: Option<(&str, &str)>,
        region: &str,
        headers: &[(&str, &str)],
        body: bytes::Bytes,
    ) -> Answer {
        let host = match signed {
            Some((name, _)) => format!("{name}.{region}.amazonaws.com"),
            None => EMULATOR_HOST.to_string(),
        };
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host);
        if let Some((signing_name, access_key_id)) = signed {
            builder = builder.header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={access_key_id}/20260101/{region}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            );
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(body).expect("a well-formed request");
        let response = self.runtime.block_on(self.router.dispatch(request));
        let (parts, body) = response.into_parts();
        Answer {
            status: parts.status.as_u16(),
            body,
        }
    }

    /// One awsJson1_1 call to one of the two Amazon Cognito services, signed or unsigned.
    pub(crate) fn rpc(
        &self,
        service: (&str, &str),
        signed: bool,
        region: &str,
        operation: &str,
        body: &Value,
    ) -> Answer {
        let (signing_name, target_prefix) = service;
        self.dispatch(
            signed.then_some((signing_name, SETUP_KEY_ID)),
            region,
            &[
                ("content-type", "application/x-amz-json-1.1"),
                ("x-amz-target", &format!("{target_prefix}.{operation}")),
            ],
            bytes::Bytes::from(body.to_string()),
        )
    }

    /// One awsJson1_1 call that has to be served, with the case in the panic of one that was
    /// not.
    pub(crate) fn rpc_ok(
        &self,
        service: (&str, &str),
        signed: bool,
        region: &str,
        operation: &str,
        body: &Value,
        case: &str,
    ) -> Value {
        let answered = self.rpc(service, signed, region, operation, body);
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from {operation}, got {} with body {:?}",
            answered.status,
            answered.text()
        );
        answered.json()
    }

    /// One awsQuery call, which is how CloudFormation and STS are called, signed with
    /// `access_key_id`.
    pub(crate) fn query(
        &self,
        signing_name: &str,
        access_key_id: &str,
        region: &str,
        action: &str,
        version: &str,
        members: &[(&str, &str)],
    ) -> Answer {
        let mut body = format!("Action={action}&Version={version}");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        self.dispatch(
            Some((signing_name, access_key_id)),
            region,
            &[("content-type", "application/x-www-form-urlencoded")],
            bytes::Bytes::from(body),
        )
    }

    /// One CloudFormation call over its awsQuery wire.
    pub(crate) fn cloudformation(
        &self,
        region: &str,
        action: &str,
        members: &[(&str, &str)],
    ) -> Answer {
        self.query(
            "cloudformation",
            SETUP_KEY_ID,
            region,
            action,
            CLOUDFORMATION_API_VERSION,
            members,
        )
    }

    /// Every `ResourceStatusReason` the stack's event stream carries, which is the
    /// deployment's own account of itself: what a failed assertion about the terminal status
    /// names.
    pub(crate) fn event_reasons(&self, region: &str, stack: &str) -> Vec<String> {
        let answered = self.cloudformation(region, "DescribeStackEvents", &[("StackName", stack)]);
        elements(&answered.text(), "ResourceStatusReason")
    }

    /// Poll `stack` until it reports a status that is not an in-progress one, and answer the
    /// `DescribeStacks` body it was read from.
    ///
    /// The transition out of an `*_IN_PROGRESS` status is owed on the stack's own clock, so
    /// what this waits for is the CONDITION: look again until the status is no longer an
    /// in-progress one, or the generous deadline passes.
    ///
    /// Visible to the other suites of this target: one settle loop, called by every operation
    /// below that waits for one and by every sibling that drives a deployment through this
    /// harness, so no second copy can drift from it.
    pub(crate) fn settled(&self, region: &str, stack: &str, case: &str) -> String {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let answered = self.cloudformation(region, "DescribeStacks", &[("StackName", stack)]);
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected HTTP 200 from DescribeStacks, got {} with body {:?}",
                answered.status,
                answered.text()
            );
            let described = answered.text();
            let reported = opt_element(&described, "StackStatus").unwrap_or_default();
            if !reported.ends_with("_IN_PROGRESS") {
                return described;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the stack never settled; it still reports {reported:?}"
            );
            self.runtime
                .block_on(async { tokio::time::sleep(SETTLE_INTERVAL).await });
        }
    }

    /// Deploy `template` as `stack`, wait for it to settle, and answer the outputs the
    /// completed stack publishes.
    ///
    /// `CAPABILITY_NAMED_IAM` acknowledges the IAM role every row declares, in the form that
    /// covers both a role the template names and one CloudFormation names ("If you have IAM
    /// resources with custom names, you must specify CAPABILITY_NAMED_IAM").
    pub(crate) fn deploy(
        &self,
        region: &str,
        stack: &str,
        template: &str,
        case: &str,
    ) -> BTreeMap<String, String> {
        let created = self.cloudformation(
            region,
            "CreateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", template),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        );
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with body {:?}",
            created.status,
            created.text()
        );

        let described = self.settled(region, stack, case);
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(CREATE_COMPLETE),
            "case {case:?}: expected the estate to reach {CREATE_COMPLETE:?}; the stack \
             recorded {:?}",
            self.event_reasons(region, stack)
        );
        outputs_of(&described)
    }

    /// Apply `template` to the stack already deployed under `stack`, wait for the update to
    /// settle, and answer the outputs the updated stack publishes.
    ///
    /// The same acknowledgement the create carries: an update states the whole template, so a
    /// stack whose create needed the capability needs it again.
    pub(crate) fn update_stack(
        &self,
        region: &str,
        stack: &str,
        template: &str,
        case: &str,
    ) -> BTreeMap<String, String> {
        let answered = self.cloudformation(
            region,
            "UpdateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", template),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        );
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {} with body {:?}",
            answered.status,
            answered.text()
        );
        let described = self.settled(region, stack, case);
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(UPDATE_COMPLETE),
            "case {case:?}: expected the update to reach {UPDATE_COMPLETE:?}; the stack \
             recorded {:?}",
            self.event_reasons(region, stack)
        );
        outputs_of(&described)
    }
}

/// Whether `id` has the form Amazon Cognito Identity mints for an identity pool of `region`:
/// `IdentityPoolId`, `@length 1..55`, `@pattern ^[\w-]+:[0-9a-f-]+$`, documented as
/// "REGION:GUID".
///
/// Visible to the other suites of this target for the reason [`is_minted_client_id`] is.
pub(crate) fn is_minted_identity_pool_id(id: &str, region: &str) -> bool {
    id.chars().count() <= 55
        && id.strip_prefix(&format!("{region}:")).is_some_and(|guid| {
            !guid.is_empty() && guid.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        })
}

/// Whether `id` has the form Amazon Cognito mints for an app client: `ClientIdType`,
/// `@length 1..128`, `@pattern ^[\w+]+$` — which a hyphenated generated name cannot satisfy.
///
/// Visible to the other suites of this target, so the predicate for a minted app-client id is
/// stated once wherever one is asserted on.
pub(crate) fn is_minted_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+')
}

/// One claim set of a compact JWS, as the `Logins` entry of an exchange carries the token
/// (RFC 7515 §3.1): the middle segment, base64url without padding.
fn claims_of(token: &str) -> Value {
    let claims = token
        .split('.')
        .nth(1)
        .unwrap_or_else(|| panic!("a compact JWS has three segments, got {token:?}"));
    let padded = match claims.len() % 4 {
        0 => claims.to_string(),
        remainder => format!("{claims}{}", "=".repeat(4 - remainder)),
    };
    let bytes = aws_smithy_types::base64::decode(padded.replace('-', "+").replace('_', "/"))
        .unwrap_or_else(|error| panic!("the claim set of {token:?} decodes: {error}"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("the claim set of {token:?} is JSON: {error}"))
}

/// The form a role mapping's key takes, which is what the row's template writes the mapping's
/// `IdentityProvider` as: the provider alone, or the provider and the app client — "the string
/// identifies the identity provider, for example, graph.facebook.com or
/// cognito-idp.us-east-1.amazonaws.com/us-east-1_abcdefghi:app_client_id"
/// (`SetIdentityPoolRolesInput$RoleMappings`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MappingKey {
    /// No `RoleMappings` at all: the attachment states only the pool's roles.
    None,
    /// `<provider name>`.
    Provider,
    /// `<provider name>:<client id>`.
    ProviderAndClient,
}

/// One row: a template deploying the whole federation estate, and the user the row then drives
/// that estate with.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region every resource lives in.
    region: &'static str,
    /// The stack name the row deploys under.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The `UserPoolName` the template declares, or `None` for a pool CloudFormation names.
    declared_pool_name: Option<&'static str>,
    /// The `RoleName` the template declares, or `None` for a role CloudFormation names.
    declared_role_name: Option<&'static str>,
    /// What the attachment's `RoleMappings` is keyed by.
    mapping_key: MappingKey,
    /// Whether the identity pool's provider declares `ServerSideTokenCheck`, and as what.
    declared_server_side_token_check: Option<bool>,
    /// The user the row creates in the deployed pool, and the password it signs in with.
    username: &'static str,
    password: &'static str,
}

/// The rows. Each one declares the SAME topology; what differs is how it is written and what
/// the attachment carries.
const ROWS: &[Row] = &[
    Row {
        // The reported shape, written as a client writes it: YAML short forms throughout, the
        // mapping keyed by provider-and-client through `Fn::Sub` over two resources whose ids
        // their owning services mint.
        case: "YAML short forms, a Token mapping keyed by provider and client",
        region: "us-east-1",
        stack: "cognito-federation-estate-yaml",
        template: r#"AWSTemplateFormatVersion: "2010-09-09"
Resources:
  UserPool:
    Type: AWS::Cognito::UserPool
    Properties:
      UserPoolName: federation-estate-directory
  UserPoolClient:
    Type: AWS::Cognito::UserPoolClient
    Properties:
      UserPoolId: !Ref UserPool
      ClientName: federation-estate-app
      GenerateSecret: false
      ExplicitAuthFlows: [ALLOW_ADMIN_USER_PASSWORD_AUTH, ALLOW_REFRESH_TOKEN_AUTH]
  IdentityPool:
    Type: AWS::Cognito::IdentityPool
    Properties:
      IdentityPoolName: federation_estate_identities
      AllowUnauthenticatedIdentities: false
      CognitoIdentityProviders:
        - ClientId: !Ref UserPoolClient
          ProviderName: !GetAtt UserPool.ProviderName
          ServerSideTokenCheck: false
  AuthenticatedRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Principal: { Federated: cognito-identity.amazonaws.com }
            Action: sts:AssumeRoleWithWebIdentity
            Condition:
              StringEquals: { "cognito-identity.amazonaws.com:aud": !Ref IdentityPool }
              "ForAnyValue:StringLike": { "cognito-identity.amazonaws.com:amr": authenticated }
  IdentityPoolRoleAttachment:
    Type: AWS::Cognito::IdentityPoolRoleAttachment
    Properties:
      IdentityPoolId: !Ref IdentityPool
      Roles: { authenticated: !GetAtt AuthenticatedRole.Arn }
      RoleMappings:
        cognitoProvider:
          Type: Token
          AmbiguousRoleResolution: AuthenticatedRole
          IdentityProvider: !Sub "cognito-idp.${AWS::Region}.amazonaws.com/${UserPool}:${UserPoolClient}"
Outputs:
  UserPoolId:
    Value: !Ref UserPool
  UserPoolClientId:
    Value: !Ref UserPoolClient
  IdentityPoolId:
    Value: !Ref IdentityPool
  RoleName:
    Value: !Ref AuthenticatedRole
"#,
        declared_pool_name: Some("federation-estate-directory"),
        declared_role_name: None,
        mapping_key: MappingKey::ProviderAndClient,
        declared_server_side_token_check: Some(false),
        username: "ada",
        password: "Federation#Pass9",
    },
    Row {
        // The same topology in JSON, with a NAMED role, the mapping keyed by the provider
        // alone and written with `Fn::GetAtt` rather than `Fn::Sub`, and a `Rules` mapping
        // that needs its rules configuration — a rule whose `RoleARN` is the same role, so
        // what the vend resolves is the role the stack attached whether the rule matched or
        // the ambiguous resolution decided it.
        case: "JSON, a named role and a Rules mapping keyed by the provider alone",
        region: "us-west-2",
        stack: "cognito-federation-estate-json",
        template: r#"{
  "Resources": {
    "UserPool": {
      "Type": "AWS::Cognito::UserPool",
      "Properties": {
        "UserPoolName": "estate_directory",
        "Policies": {"PasswordPolicy": {"MinimumLength": 8}}
      }
    },
    "UserPoolClient": {
      "Type": "AWS::Cognito::UserPoolClient",
      "Properties": {
        "UserPoolId": {"Ref": "UserPool"},
        "ClientName": "estate-app",
        "GenerateSecret": false,
        "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
      }
    },
    "IdentityPool": {
      "Type": "AWS::Cognito::IdentityPool",
      "Properties": {
        "IdentityPoolName": "estate_identities",
        "AllowUnauthenticatedIdentities": false,
        "CognitoIdentityProviders": [{
          "ClientId": {"Ref": "UserPoolClient"},
          "ProviderName": {"Fn::GetAtt": ["UserPool", "ProviderName"]}
        }]
      }
    },
    "AuthenticatedRole": {
      "Type": "AWS::IAM::Role",
      "Properties": {
        "RoleName": "estate-authenticated-role",
        "AssumeRolePolicyDocument": {
          "Version": "2012-10-17",
          "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": "cognito-identity.amazonaws.com"},
            "Action": "sts:AssumeRoleWithWebIdentity",
            "Condition": {
              "StringEquals": {
                "cognito-identity.amazonaws.com:aud": {"Ref": "IdentityPool"}
              },
              "ForAnyValue:StringLike": {
                "cognito-identity.amazonaws.com:amr": "authenticated"
              }
            }
          }]
        }
      }
    },
    "IdentityPoolRoleAttachment": {
      "Type": "AWS::Cognito::IdentityPoolRoleAttachment",
      "Properties": {
        "IdentityPoolId": {"Ref": "IdentityPool"},
        "Roles": {"authenticated": {"Fn::GetAtt": ["AuthenticatedRole", "Arn"]}},
        "RoleMappings": {
          "userpool": {
            "Type": "Rules",
            "AmbiguousRoleResolution": "AuthenticatedRole",
            "IdentityProvider": {"Fn::GetAtt": ["UserPool", "ProviderName"]},
            "RulesConfiguration": {"Rules": [{
              "Claim": "cognito:username",
              "MatchType": "Equals",
              "Value": "grace.hopper",
              "RoleARN": {"Fn::GetAtt": ["AuthenticatedRole", "Arn"]}
            }]}
          }
        }
      }
    }
  },
  "Outputs": {
    "UserPoolId": {"Value": {"Ref": "UserPool"}},
    "UserPoolClientId": {"Value": {"Ref": "UserPoolClient"}},
    "IdentityPoolId": {"Value": {"Ref": "IdentityPool"}},
    "RoleName": {"Value": {"Ref": "AuthenticatedRole"}}
  }
}"#,
        declared_pool_name: Some("estate_directory"),
        declared_role_name: Some("estate-authenticated-role"),
        mapping_key: MappingKey::Provider,
        declared_server_side_token_check: None,
        username: "grace.hopper",
        password: "Estate#Pass42",
    },
    Row {
        // Nothing named, nothing mapped: CloudFormation names the pool, the client and the
        // role, the attachment carries the pool's roles alone, and the handles the stack
        // publishes are read with `Fn::Sub` instead of `Ref`.
        case: "YAML, every name left to CloudFormation and no role mapping at all",
        region: "eu-central-1",
        stack: "cognito-federation-estate-generated",
        template: r#"Resources:
  UserPool:
    Type: AWS::Cognito::UserPool
    Properties:
      MfaConfiguration: 'OFF'
  UserPoolClient:
    Type: AWS::Cognito::UserPoolClient
    Properties:
      UserPoolId: !Ref UserPool
      ExplicitAuthFlows:
        - ALLOW_ADMIN_USER_PASSWORD_AUTH
        - ALLOW_REFRESH_TOKEN_AUTH
  IdentityPool:
    Type: AWS::Cognito::IdentityPool
    Properties:
      AllowUnauthenticatedIdentities: true
      CognitoIdentityProviders:
        - ClientId: !Ref UserPoolClient
          ProviderName: !GetAtt UserPool.ProviderName
          ServerSideTokenCheck: true
  AuthenticatedRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Principal: { Federated: cognito-identity.amazonaws.com }
            Action: sts:AssumeRoleWithWebIdentity
            Condition:
              StringEquals: { "cognito-identity.amazonaws.com:aud": !Ref IdentityPool }
  IdentityPoolRoleAttachment:
    Type: AWS::Cognito::IdentityPoolRoleAttachment
    Properties:
      IdentityPoolId: !Ref IdentityPool
      Roles: { authenticated: !GetAtt AuthenticatedRole.Arn }
Outputs:
  UserPoolId:
    Value: !Sub '${UserPool}'
  UserPoolClientId:
    Value: !Sub '${UserPoolClient}'
  IdentityPoolId:
    Value: !Sub '${IdentityPool}'
  RoleName:
    Value: !Ref AuthenticatedRole
"#,
        declared_pool_name: None,
        declared_role_name: None,
        mapping_key: MappingKey::None,
        declared_server_side_token_check: Some(true),
        username: "k.johnson",
        password: "Generated#Pass42",
    },
];

/// Every row: the estate the stack deployed reaches `CREATE_COMPLETE`, the four handles it
/// publishes are the ones the owning services minted, the resources those handles resolve hold
/// exactly what the template declared about each other, and the whole federated sign-in the
/// topology exists for runs on them.
#[test]
fn a_deployed_cognito_federation_estate_vends_its_attached_role() {
    std::thread::Builder::new()
        .name("cognito-federation-estate".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            let wire = Wire {
                router: &router,
                runtime: &runtime,
            };
            for row in ROWS {
                drive(&wire, row);
            }
        })
        .expect("spawn the federation-estate thread")
        .join()
        .expect("a deployed federation estate must vend its attached role");
}

/// One row, from the deploy to the vended session.
fn drive(wire: &Wire<'_>, row: &Row) {
    let case = row.case;
    let region = row.region;
    let outputs = wire.deploy(region, row.stack, row.template, case);

    // The handles: each one of the form the OWNING service mints, and none of them the name
    // the template declared.
    let pool = output(&outputs, "UserPoolId", case);
    let client = output(&outputs, "UserPoolClientId", case);
    let identity_pool = output(&outputs, "IdentityPoolId", case);
    let role_name = output(&outputs, "RoleName", case);
    assert!(
        is_minted_pool_id(&pool, region),
        "case {case:?}: expected the published user-pool handle to be the id Amazon Cognito \
         minted, got {pool:?}"
    );
    assert!(
        is_minted_client_id(&client),
        "case {case:?}: expected the published app-client handle to be the id Amazon Cognito \
         minted, got {client:?}"
    );
    assert!(
        is_minted_identity_pool_id(&identity_pool, region),
        "case {case:?}: expected the published identity-pool handle to be the REGION:GUID \
         Amazon Cognito Identity minted, got {identity_pool:?}"
    );
    if let Some(declared) = row.declared_pool_name {
        assert_ne!(
            pool, declared,
            "case {case:?}: expected the published user-pool handle to be the minted id, not \
             the declared UserPoolName"
        );
    }
    if let Some(declared) = row.declared_role_name {
        assert_eq!(
            role_name, declared,
            "case {case:?}: expected the published role handle to be the declared RoleName"
        );
    }

    // The provider name a user pool publishes, which is what the identity pool federates and
    // what a login of that pool is made under: computed here from the handle and the Region,
    // never read back out of the answer being checked.
    let provider = format!("cognito-idp.{region}.amazonaws.com/{pool}");
    let role_arn = format!(
        "arn:aws:iam::{}:role/{role_name}",
        engine::emulator_account_id()
    );

    // The identity pool the stack created federates the app client by the id the stack
    // published, under that provider name.
    let described = wire.rpc_ok(
        COGNITO_IDENTITY,
        true,
        region,
        "DescribeIdentityPool",
        &json!({"IdentityPoolId": &identity_pool}),
        case,
    );
    let federated = &described["CognitoIdentityProviders"][0];
    assert_eq!(
        federated["ClientId"].as_str(),
        Some(client.as_str()),
        "case {case:?}: expected the identity pool to federate the published app client, got \
         {described}"
    );
    assert_eq!(
        federated["ProviderName"].as_str(),
        Some(provider.as_str()),
        "case {case:?}: expected the identity pool's provider to be the published pool's own \
         provider name, got {described}"
    );
    assert_eq!(
        federated["ServerSideTokenCheck"].as_bool(),
        row.declared_server_side_token_check,
        "case {case:?}: expected the declared ServerSideTokenCheck to be the one the owning \
         service holds, got {described}"
    );

    // The role attachment the stack applied: the pool's authenticated role is the role the
    // stack created, and the mapping — where the row declares one — is keyed by the provider
    // identifier the template's intrinsic resolved to.
    let configured = wire.rpc_ok(
        COGNITO_IDENTITY,
        true,
        region,
        "GetIdentityPoolRoles",
        &json!({"IdentityPoolId": &identity_pool}),
        case,
    );
    assert_eq!(
        configured["Roles"]["authenticated"].as_str(),
        Some(role_arn.as_str()),
        "case {case:?}: expected the attached authenticated role to be the role the stack \
         created ({role_arn}), got {configured}"
    );
    let expected_mapping_key = match row.mapping_key {
        MappingKey::None => None,
        MappingKey::Provider => Some(provider.clone()),
        MappingKey::ProviderAndClient => Some(format!("{provider}:{client}")),
    };
    let mapped: Vec<String> = configured["RoleMappings"]
        .as_object()
        .map(|mappings| mappings.keys().cloned().collect())
        .unwrap_or_default();
    assert_eq!(
        mapped,
        expected_mapping_key.iter().cloned().collect::<Vec<String>>(),
        "case {case:?}: expected the role mappings to be keyed by the resolved provider \
         identifier, got {configured}"
    );

    // The sign-in the estate exists for, every call addressed by a published handle.
    let created = wire.rpc_ok(
        COGNITO_IDP,
        true,
        region,
        "AdminCreateUser",
        &json!({"UserPoolId": &pool, "Username": row.username, "MessageAction": "SUPPRESS"}),
        case,
    );
    assert_eq!(
        created["User"]["Username"].as_str(),
        Some(row.username),
        "case {case:?}: expected AdminCreateUser to answer with the user it created, got \
         {created}"
    );
    wire.rpc_ok(
        COGNITO_IDP,
        true,
        region,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": &pool,
            "Username": row.username,
            "Password": row.password,
            "Permanent": true
        }),
        case,
    );
    let signed_in = wire.rpc_ok(
        COGNITO_IDP,
        true,
        region,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": &pool,
            "ClientId": &client,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": {"USERNAME": row.username, "PASSWORD": row.password}
        }),
        case,
    );
    let id_token = signed_in["AuthenticationResult"]["IdToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a sign-in to mint an IdToken, got {signed_in}"))
        .to_string();
    let claims = claims_of(&id_token);
    assert_eq!(
        claims["iss"].as_str(),
        Some(format!("https://{provider}").as_str()),
        "case {case:?}: expected the token to be issued by the published pool, got {claims}"
    );
    assert_eq!(
        claims["aud"].as_str(),
        Some(client.as_str()),
        "case {case:?}: expected the token's audience to be the published app client, got \
         {claims}"
    );

    // The exchange: the token of that pool, presented as the login of the provider the
    // identity pool federates, buys an identity of the identity pool the stack published and
    // then the credentials of a session of the role the stack attached. Both legs are
    // unsigned, as a browser client sends them.
    let logins = json!({&provider: id_token});
    let minted = wire.rpc_ok(
        COGNITO_IDENTITY,
        false,
        region,
        "GetId",
        &json!({"IdentityPoolId": &identity_pool, "Logins": &logins}),
        case,
    );
    let identity_id = minted["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected GetId to mint an IdentityId, got {minted}"))
        .to_string();
    assert!(
        identity_id.starts_with(&format!("{region}:")),
        "case {case:?}: expected the identity to be one of this Region's pool, got \
         {identity_id:?}"
    );
    let vended = wire.rpc_ok(
        COGNITO_IDENTITY,
        false,
        region,
        "GetCredentialsForIdentity",
        &json!({"IdentityId": &identity_id, "Logins": &logins}),
        case,
    );
    let access_key_id = vended["Credentials"]["AccessKeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the vend to carry an AccessKeyId, got {vended}"))
        .to_string();

    // WHO those credentials speak for: a session of the role the stack attached, which is the
    // whole point of the topology.
    let answered = wire.query(
        "sts",
        &access_key_id,
        region,
        "GetCallerIdentity",
        STS_API_VERSION,
        &[],
    );
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected GetCallerIdentity under the vended credential to answer 200, \
         got {} with body {:?}",
        answered.status,
        answered.text()
    );
    let body = answered.text();
    let expected = format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{VENDED_ROLE_SESSION_NAME}",
        engine::emulator_account_id()
    );
    assert_eq!(
        element(&body, "Arn"),
        expected,
        "case {case:?}: expected the vended session to speak for the role the stack attached, \
         got {body}"
    );
}
